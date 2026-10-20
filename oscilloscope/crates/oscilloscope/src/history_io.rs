//! `osi`'s own wiring for run history (issue #649): the one place in
//! the binary crate that reads `OSI_HISTORY`/`OSI_HISTORY_DIR`/
//! `$XDG_STATE_HOME`/`$HOME`, resolves the document a run or watch is
//! keyed by, reads a document's own history once and keeps its
//! statistics in memory, and writes one record at a run's own end.
//! `oscilloscope_core::history` holds every rule; this module only
//! decides *when* those functions run and with which paths.

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};

use oscilloscope_core::history::{self, DocumentStats, EtaOutput, HistoryRecord};
use oscilloscope_core::model::RunModel;
use oscilloscope_core::plan::PlanTree;
use oscilloscope_core::render::Header;

/// `OSI_HISTORY_DIR`, else `OSI_HISTORY=off`, else the XDG state path
/// (issue #649) -- `no_history_flag` is `--no-history`, checked
/// first since it always wins. Reads the process environment exactly
/// once, here; every other function in this module takes the result
/// as a plain parameter.
pub fn resolve_history_dir(no_history_flag: bool) -> Option<PathBuf> {
    if no_history_flag {
        return None;
    }
    if std::env::var("OSI_HISTORY").ok().as_deref() == Some("off") {
        return None;
    }
    let override_dir = std::env::var_os("OSI_HISTORY_DIR").map(PathBuf::from);
    let xdg_state_home = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    history::resolve_history_dir(
        override_dir.as_deref(),
        xdg_state_home.as_deref(),
        home.as_deref(),
    )
}

/// `osi <doc>`'s own document key (issue #649's `document (absolute
/// path)`): the orchestration argument `osi` was actually given,
/// made absolute -- `canonicalize` when the file still exists (the
/// common case, and the one that also resolves symlinks so two
/// different paths to the same file key to the same history), else a
/// plain `cwd`-joined fallback so a run that somehow still succeeds
/// after its own document vanished mid-run still gets a usable key.
pub fn resolve_run_document_path(orchestration: &Path) -> String {
    let resolved = std::fs::canonicalize(orchestration).unwrap_or_else(|_| {
        if orchestration.is_absolute() {
            orchestration.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(orchestration))
                .unwrap_or_else(|_| orchestration.to_path_buf())
        }
    });
    resolved.to_string_lossy().into_owned()
}

/// `osi watch`'s own document key: an absolute `runtime.last_run.
/// orchestration_path`, else `--plan`, else history is off for this
/// watch (issue #649's own decision) -- `None` only while there is no
/// state to decide from yet at all (too early to tell either way, and
/// too early to fall back to `--plan` either: deciding before the
/// first live-state write exists at all would settle on `--plan` on
/// the very first frame even when that write, once it arrives, would
/// have given an absolute path of its own -- `document_settled`
/// never asks again after this answers once). The caller only warns
/// once, the first time this actually settles on "off" rather than
/// "not yet known".
fn resolve_watch_document_path(
    state: Option<&serde_json::Value>,
    plan_arg: Option<&Path>,
) -> Option<Option<String>> {
    let state = state?;
    if let Some(path) = oscilloscope_core::model::orchestration_path(state) {
        let path = Path::new(path);
        if path.is_absolute() {
            // Canonicalized the same way `resolve_run_document_path`
            // already is (issue #649): without this, the very same
            // document could key to two different history files
            // depending on whether it was reached through `osi <doc>`
            // (which canonicalizes) or `osi watch`'s own state-read
            // path (which otherwise wouldn't) -- symlinked temp
            // directories (macOS's own `/tmp` included) make this a
            // real, not just theoretical, mismatch.
            let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            return Some(Some(resolved.to_string_lossy().into_owned()));
        }
    }
    if let Some(plan_arg) = plan_arg {
        let resolved = std::fs::canonicalize(plan_arg).unwrap_or_else(|_| plan_arg.to_path_buf());
        return Some(Some(resolved.to_string_lossy().into_owned()));
    }
    Some(None)
}

/// Run history's own state for one `osi`/`osi watch` invocation
/// (issue #649): resolves the document lazily for a watch (once the
/// first live-state write names it), reads a document's history
/// exactly once, and recomputes [`DocumentStats`] only when the
/// current run's own `document_hash` changes (it never does, but a
/// watch attaching mid-run and a run starting are both "the first
/// time" this is asked). `RefCell`s rather than `&mut self`
/// throughout: every real call site already holds `model`/`plan`
/// borrows of its own, and this is never touched from more than one
/// place at a time.
pub struct HistoryHandle {
    dir: Option<PathBuf>,
    document: RefCell<Option<String>>,
    /// `osi watch --plan`'s own value, used only to resolve
    /// [`Self::document`] when `runtime.last_run.orchestration_path`
    /// isn't absolute -- `None` for `osi <doc>`, which already knows
    /// its document from the CLI argument alone.
    watch_plan_fallback: Option<PathBuf>,
    /// Set once [`Self::ensure_document`] has made its one decision
    /// for a watch (a real path, or off) -- never retried after that,
    /// even if a later state write would have resolved differently.
    document_settled: std::cell::Cell<bool>,
    warned_no_document: std::cell::Cell<bool>,
    records: RefCell<Option<Vec<HistoryRecord>>>,
    stats: RefCell<Option<(Option<String>, DocumentStats)>>,
    /// A notice [`Self::ensure_document`] has, but hasn't printed yet
    /// -- `ensure_document` itself runs on every redraw inside the
    /// TUI's own raw-mode, alternate-screen loop, where a bare
    /// `eprintln!` would land as a stray line over the drawn frame.
    /// The caller takes these with [`Self::take_notices`] once the
    /// terminal is back to normal (or, for `--glance`, once its own
    /// loop ends) and prints them there instead.
    pending_notices: RefCell<Vec<String>>,
}

impl HistoryHandle {
    /// `osi <doc>`: the document is already known, so there is
    /// nothing left to resolve lazily.
    pub fn for_run(dir: Option<PathBuf>, document: String) -> Self {
        HistoryHandle {
            dir,
            document: RefCell::new(Some(document)),
            watch_plan_fallback: None,
            document_settled: std::cell::Cell::new(true),
            warned_no_document: std::cell::Cell::new(false),
            records: RefCell::new(None),
            stats: RefCell::new(None),
            pending_notices: RefCell::new(Vec::new()),
        }
    }

    /// `osi watch`: the document is resolved lazily, from the first
    /// live-state write that names one (issue #649).
    pub fn for_watch(dir: Option<PathBuf>, plan_fallback: Option<PathBuf>) -> Self {
        HistoryHandle {
            dir,
            document: RefCell::new(None),
            watch_plan_fallback: plan_fallback,
            document_settled: std::cell::Cell::new(false),
            warned_no_document: std::cell::Cell::new(false),
            records: RefCell::new(None),
            stats: RefCell::new(None),
            pending_notices: RefCell::new(Vec::new()),
        }
    }

    /// Every notice queued so far, removing them -- the caller's own
    /// job to print, once it's safe to (issue #649; see
    /// [`Self::pending_notices`]'s own doc comment).
    pub fn take_notices(&self) -> Vec<String> {
        std::mem::take(&mut self.pending_notices.borrow_mut())
    }

    fn ensure_document(&self, state: Option<&serde_json::Value>) {
        if self.document_settled.get() {
            return;
        }
        match resolve_watch_document_path(state, self.watch_plan_fallback.as_deref()) {
            None => {}
            Some(resolved) => {
                self.document_settled.set(true);
                *self.document.borrow_mut() = resolved;
                if self.document.borrow().is_none() && !self.warned_no_document.get() {
                    self.warned_no_document.set(true);
                    self.pending_notices.borrow_mut().push(
                        "osi: this watch has no absolute orchestration path and no --plan; \
                         history is off for it"
                            .to_string(),
                    );
                }
            }
        }
    }

    fn stats_for(&self, current_hash: Option<&str>) -> Option<DocumentStats> {
        let dir = self.dir.as_ref()?;
        let document = self.document.borrow().clone()?;
        if self.records.borrow().is_none() {
            *self.records.borrow_mut() = Some(history::read_records(dir, &document));
        }
        let needs_recompute = self
            .stats
            .borrow()
            .as_ref()
            .is_none_or(|(hash, _)| hash.as_deref() != current_hash);
        if needs_recompute {
            let records = self.records.borrow();
            let stats = history::compute_stats(records.as_ref().unwrap(), current_hash);
            *self.stats.borrow_mut() = Some((current_hash.map(str::to_string), stats));
        }
        self.stats.borrow().as_ref().map(|(_, stats)| stats.clone())
    }

    /// The whole-run ETA for this frame (issue #649), plus the
    /// statistics it came from (the now pane's own per-step usual
    /// time needs [`DocumentStats::per_step`] too) -- `None` under
    /// the same conditions [`HistoryHandle::eta`] documents.
    pub fn eta_with_stats(
        &self,
        state: Option<&serde_json::Value>,
        plan: &PlanTree,
        run_start_ts: Option<&str>,
        now: &str,
    ) -> Option<(EtaOutput, DocumentStats)> {
        self.ensure_document(state);
        let state = state?;
        // `last_run.started_at` when it's in hand, same anchor the
        // recorded runs' own offsets come from (`build_record`) --
        // `header.run_start_ts` can resolve earlier (cof sets it
        // before preflight runs), which would otherwise offset this
        // run's own figures from every recorded one by however long
        // that takes.
        let eta_start_ts = state
            .pointer("/runtime/last_run/started_at")
            .and_then(serde_json::Value::as_str)
            .or(run_start_ts)?;
        let current_hash = state
            .pointer("/runtime/last_run/document_hash")
            .and_then(serde_json::Value::as_str);
        let stats = self.stats_for(current_hash)?;
        let anchor_now = eta_anchor_now(state, now);
        let elapsed_s =
            oscilloscope_core::observe::duration_seconds(eta_start_ts, anchor_now).unwrap_or(0.0);
        let input = history::eta_input_from_state(state, plan, eta_start_ts, elapsed_s);
        let eta = history::compute_eta(&stats, &input);
        Some((eta, stats))
    }

    /// Writes one record for this run, once it has truly ended
    /// (issue #649: "a run that aborts with no final write ... [is]
    /// not recorded" -- covered here by `run_ended`, same gate every
    /// other ending already uses). A no-op with history off, an
    /// unresolved watch document, or a `state` that didn't end this
    /// way (still running, or genuinely aborted).
    pub fn write(
        &self,
        engine: Option<&str>,
        state: Option<&serde_json::Value>,
        model: &RunModel,
        plan: &PlanTree,
    ) {
        self.ensure_document(state);
        let Some(dir) = &self.dir else { return };
        let Some(document) = self.document.borrow().clone() else {
            return;
        };
        let Some(state) = state else { return };
        if !oscilloscope_core::model::run_ended(state) {
            return;
        }
        let Some(record) = history::build_record(&document, engine, state, model, plan) else {
            return;
        };
        if let Err(err) = history::append_record(dir, &document, &record) {
            // `let _ = writeln!(stderr())`, not `eprintln!`: this can
            // run right after a lost terminal (`main.rs`'s own
            // `lost_terminal`), where `eprintln!`'s own panic on a
            // write failure would turn a history write failure into
            // a crash.
            let _ = writeln!(std::io::stderr(), "osi: couldn't write run history: {err}");
            return;
        }
        let _ = history::evict_if_needed(dir);
    }
}

/// Applies [`HistoryHandle::eta`]'s own result onto `header` (issue
/// #649) -- the one place `header.history_eta_ts`/`history_eta_past_
/// usual` are ever set to anything but their own "no history"
/// defaults, called right after every real `render::build`/`build_
/// folded` in the TUI and `--glance` loops. A no-op (the header keeps
/// reading `loop ETA ~`, or nothing) when [`HistoryHandle::eta`]
/// itself returns `None`.
pub fn apply_history_eta(
    history: &HistoryHandle,
    header: &mut Header,
    state: Option<&serde_json::Value>,
    plan: &PlanTree,
    now: &str,
) {
    let Some((eta, stats)) =
        history.eta_with_stats(state, plan, header.run_start_ts.as_deref(), now)
    else {
        return;
    };
    header.history_eta_past_usual = eta.past_usual;
    let anchor_now = state.map(|s| eta_anchor_now(s, now)).unwrap_or(now);
    header.history_eta_ts = eta.eta_ts(anchor_now);
    header.history_eta_remaining_s = eta.remaining_s;
    header.history_eta_low_ts = eta_ts_for(eta.low_s, anchor_now);
    header.history_eta_high_ts = eta_ts_for(eta.high_s, anchor_now);
    header.history_vs_usual_s = eta.vs_usual_s;
    header.history_usual_total_s = eta.usual_total_s;
    header.history_same_hash_count = eta.whole_run_count;
    header.history_step_usual = stats.per_step;
}

/// Once a run has truly ended (issue #649), the ETA freezes at its
/// own end rather than the wall clock: the TUI can stay open on a
/// finished run's state for as long as the user likes, and measuring
/// elapsed time against a live `now` would show an ETA that kept
/// sliding forward, eventually past its own usual end.
fn eta_anchor_now<'a>(state: &'a serde_json::Value, now: &'a str) -> &'a str {
    if oscilloscope_core::model::run_ended(state) {
        if let Some(ts) = state
            .pointer("/runtime/last_run/completed_at")
            .and_then(serde_json::Value::as_str)
        {
            return ts;
        }
    }
    now
}

fn eta_ts_for(seconds: Option<f64>, now: &str) -> Option<String> {
    seconds.and_then(|s| {
        oscilloscope_core::diff::parse_instant(now).map(|now_dt| {
            (now_dt + chrono::Duration::milliseconds((s * 1000.0).round() as i64)).to_rfc3339()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_watch_document_path_waits_for_the_first_state_before_falling_back_to_plan() {
        // No state yet at all: too early to tell, even with `--plan`
        // in hand -- settling here would lock in `--plan` forever
        // (`document_settled` never asks again), pre-empting an
        // absolute `orchestration_path` the first real state write
        // might still give.
        assert_eq!(
            resolve_watch_document_path(None, Some(Path::new("/plan.yml"))),
            None
        );
    }

    #[test]
    fn resolve_watch_document_path_prefers_an_absolute_orchestration_path_over_plan() {
        let state =
            serde_json::json!({"runtime": {"last_run": {"orchestration_path": "/abs/doc.yml"}}});
        let resolved = resolve_watch_document_path(Some(&state), Some(Path::new("/plan.yml")));
        assert_eq!(resolved, Some(Some("/abs/doc.yml".to_string())));
    }

    #[test]
    fn resolve_watch_document_path_falls_back_to_plan_when_the_path_is_relative() {
        let state = serde_json::json!({"runtime": {"last_run": {"orchestration_path": "doc.yml"}}});
        let resolved = resolve_watch_document_path(Some(&state), Some(Path::new("/plan.yml")));
        assert_eq!(resolved, Some(Some("/plan.yml".to_string())));
    }

    #[test]
    fn resolve_watch_document_path_settles_off_once_state_exists_with_neither() {
        let state = serde_json::json!({"prime": {"value": null}});
        assert_eq!(resolve_watch_document_path(Some(&state), None), Some(None));
    }
}
