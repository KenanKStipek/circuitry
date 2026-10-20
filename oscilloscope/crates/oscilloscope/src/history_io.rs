//! `osi`'s own wiring for run history (issue #649): the one place in
//! the binary crate that reads `OSI_HISTORY`/`OSI_HISTORY_DIR`/
//! `$XDG_STATE_HOME`/`$HOME`, resolves the document a run or watch is
//! keyed by, reads a document's own history once and keeps its
//! statistics in memory, and writes one record at a run's own end.
//! `oscilloscope_core::history` holds every rule; this module only
//! decides *when* those functions run and with which paths.

use std::cell::RefCell;
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
/// state to decide from yet at all (too early to tell either way).
/// The caller only warns once, the first time this actually settles
/// on "off" rather than "not yet known".
fn resolve_watch_document_path(
    state: Option<&serde_json::Value>,
    plan_arg: Option<&Path>,
) -> Option<Option<String>> {
    if let Some(path) = state.and_then(oscilloscope_core::model::orchestration_path) {
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
    state.map(|_| None)
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
        }
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
                    eprintln!(
                        "osi: this watch has no absolute orchestration path and no --plan; \
                         history is off for it"
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

    /// The whole-run ETA for this frame (issue #649), or `None` when
    /// history is off, the document isn't known yet (a watch with no
    /// state), or there isn't enough history for one yet.
    pub fn eta(
        &self,
        state: Option<&serde_json::Value>,
        plan: &PlanTree,
        run_start_ts: Option<&str>,
    ) -> Option<EtaOutput> {
        self.ensure_document(state);
        let state = state?;
        let run_start_ts = run_start_ts?;
        let current_hash = state
            .pointer("/runtime/last_run/document_hash")
            .and_then(serde_json::Value::as_str);
        let stats = self.stats_for(current_hash)?;
        let elapsed_s = oscilloscope_core::observe::duration_seconds(
            run_start_ts,
            &oscilloscope_core::observe::now_rfc3339(),
        )
        .unwrap_or(0.0);
        let input = history::eta_input_from_state(state, plan, run_start_ts, elapsed_s);
        Some(history::compute_eta(&stats, &input))
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
            eprintln!("osi: couldn't write run history: {err}");
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
    let Some(eta) = history.eta(state, plan, header.run_start_ts.as_deref()) else {
        return;
    };
    header.history_eta_past_usual = eta.past_usual;
    header.history_eta_ts = eta.eta_ts(now);
}
