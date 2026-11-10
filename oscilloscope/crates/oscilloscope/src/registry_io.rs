//! `osi`'s own wiring for the run registry: the one place in the
//! binary crate that reads `OSI_RUNS_DIR`/`$XDG_RUNTIME_DIR`/
//! `$XDG_STATE_HOME`/`$HOME`, and decides *when* an entry is written
//! and removed for one `osi <doc>` or `osi watch` invocation.
//! `oscilloscope_core::registry` holds every other rule (the file
//! format, liveness, pruning).
//!
//! An entry needs a `run_id`, which only exists once the run (or the
//! watched run) has actually announced itself with a `run_start`
//! event -- so [`RegistryHandle::ensure_written`] is a no-op, every
//! time it's called, until that happens, and writes the entry
//! exactly once after it does. [`RegistryHandle`]'s own [`Drop`]
//! removes it again -- covering a detach and osi's own exit
//! (including every signal path, since every one of them already
//! unwinds back to the scope that owns this handle) without this
//! module needing to know which of those actually happened; callers
//! that can see the run's own end before they return call
//! [`RegistryHandle::remove`] explicitly right there instead of
//! waiting for the TUI's own finished screen to close first.
//!
//! A watch of a run another `osi` already launched shares that run's
//! own `run_id`, and so would share its registry file too --
//! [`RegistryHandle::ensure_written`] checks for that (an `is_watch`
//! handle never overwrites an existing, still-live entry), so the
//! watch's own detach can't delete the launcher's entry out from
//! under it.

use std::path::{Path, PathBuf};

use oscilloscope_core::model::RunModel;
use oscilloscope_core::observe::now_rfc3339;
use oscilloscope_core::registry::{self, Liveness, RegistryEntry};

use crate::history_io::absolute_path;

/// `OSI_RUNS_DIR`, else `$XDG_RUNTIME_DIR/circuitry/osi/runs`, else
/// `$XDG_STATE_HOME/circuitry/osi/runs`, else `~/.local/state/
/// circuitry/osi/runs`. Reads the process
/// environment exactly once, here; every other function in this
/// module takes the result as a plain parameter.
pub fn resolve_runs_dir() -> Option<PathBuf> {
    let override_dir = std::env::var_os("OSI_RUNS_DIR").map(PathBuf::from);
    let xdg_runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let xdg_state_home = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    registry::resolve_runs_dir(
        override_dir.as_deref(),
        xdg_runtime_dir.as_deref(),
        xdg_state_home.as_deref(),
        home.as_deref(),
    )
}

/// One `osi <doc>`/`osi watch` invocation's own registry entry,
/// written once its run's `run_id` is known and removed again when
/// this handle itself is dropped.
pub struct RegistryHandle {
    dir: Option<PathBuf>,
    config: Option<String>,
    /// `None` for `osi watch`, which doesn't know which engine wrote
    /// the run it's watching until its own `run_start` event says so
    /// -- [`Self::ensure_written`] falls back to `model.run_start_
    /// engine()` in that case, and skips writing (same as a missing
    /// `run_id`/pid) until even that is known.
    engine: Option<String>,
    launcher: String,
    /// Whether this handle is `osi watch`'s own, rather than a
    /// launch's -- the only difference [`Self::ensure_written`]
    /// makes on it: a watch never overwrites an existing, still-live
    /// entry for the same `run_id` (a launcher's own), and its own
    /// document is only made absolute when it's already given that
    /// way (see its own doc comment).
    is_watch: bool,
    run_dir: String,
    events: String,
    live_state: String,
    /// This handle's own launch (or attach) time, recorded once, at
    /// construction -- the registry entry's own `started_at`, so
    /// Select's own launch-order rows can trust it rather than the
    /// engine's own `run_start` time, which can trail a `cof` run's
    /// actual launch by however long its own cold start takes.
    started_at: String,
    liveness: Box<dyn Liveness>,
    /// The `run_id` this handle has actually written an entry for,
    /// once it has -- `None` until then, and the key `Drop` removes
    /// by.
    written_run_id: Option<String>,
    /// Set once an `is_watch` handle has found a launcher's own,
    /// still-live entry already at this `run_id`'s own path --
    /// [`Self::ensure_written`] stops trying from then on, and
    /// [`Self::remove`] has nothing of its own to remove either way.
    skip_write: bool,
}

impl RegistryHandle {
    /// Every path but `orchestration` (given fresh to
    /// [`Self::ensure_written`], see its own doc comment) is made
    /// absolute here ("document and config as absolute paths") -- a
    /// caller's own `--out-dir` may be relative, and the engine's own
    /// command still spawns fine with a relative run directory (it
    /// inherits osi's own cwd), but a registry entry is read by a
    /// *different* process, possibly from a different cwd, so every
    /// path in it is resolved once, here, rather than left for a
    /// reader to guess at. Also checks, right here and once, whether
    /// the resolved runs directory is already too open -- before
    /// this osi has entered the TUI's own alternate screen, so a
    /// warning about it is never written to stderr in the middle of
    /// a redraw.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        dir: Option<PathBuf>,
        config: Option<&Path>,
        engine: Option<&str>,
        launcher: String,
        run_dir: &Path,
        events: &Path,
        live_state: &Path,
        is_watch: bool,
    ) -> Self {
        if let Some(d) = &dir {
            if let Some(mode) = registry::insecure_runs_dir_mode(d) {
                eprintln!(
                    "osi: run registry directory {} is group- or world-accessible \
                     (mode {:o}); it can hold document and config paths",
                    d.display(),
                    mode
                );
            }
        }
        RegistryHandle {
            dir,
            config: config.map(|c| absolute_path(c).to_string_lossy().into_owned()),
            engine: engine.map(str::to_string),
            launcher,
            is_watch,
            run_dir: absolute_path(run_dir).to_string_lossy().into_owned(),
            events: absolute_path(events).to_string_lossy().into_owned(),
            live_state: absolute_path(live_state).to_string_lossy().into_owned(),
            started_at: now_rfc3339(),
            liveness: Box::new(registry::RealLiveness),
            written_run_id: None,
            skip_write: false,
        }
    }

    /// Writes this run's own entry, the first time `model` carries a
    /// `run_start` with both a `run_id` and a pid -- a no-op on every
    /// call before that, and on every call after the entry is
    /// already written ("written once `run_start` is known").
    /// `orchestration` is taken fresh on every call rather than fixed
    /// at construction, since `osi <doc>`'s caller always has it
    /// already but `osi watch`'s own document is resolved lazily
    /// (the same document a redraw's own header shows,
    /// `watch_document_name`) -- by the time `run_id` is actually
    /// known, whichever caller it is already has its own best answer
    /// in hand to pass in. Made absolute the same way every other
    /// path in the entry is, *except* for an `is_watch` handle given
    /// a relative one: that path is relative to the run's own
    /// launcher, not to this watch's own cwd, so resolving it here
    /// would only ever be wrong -- stored exactly as given instead,
    /// the same treatment `history_io`'s own watch document key
    /// already gives it.
    pub fn ensure_written(&mut self, model: &RunModel, orchestration: &str) {
        if self.written_run_id.is_some() || self.skip_write {
            return;
        }
        let Some(dir) = &self.dir else { return };
        let Some(run_id) = model.run_start_id() else {
            return;
        };
        let Some(pid) = model.run_start_pid() else {
            return;
        };
        let Some(engine) = self.engine.clone().or_else(|| {
            model
                .run_start_engine()
                .and_then(|e| e.split_whitespace().next())
                .map(str::to_string)
        }) else {
            return;
        };
        if self.is_watch {
            let existing_path = registry::registry_file_path(dir, run_id);
            if let Ok(bytes) = std::fs::read(&existing_path) {
                if let Ok(existing) = serde_json::from_slice::<RegistryEntry>(&bytes) {
                    if self.liveness.is_alive(existing.pid) {
                        self.skip_write = true;
                        return;
                    }
                }
            }
        }
        let orchestration_path = Path::new(orchestration);
        let orchestration = if self.is_watch && !orchestration_path.is_absolute() {
            orchestration.to_string()
        } else {
            absolute_path(orchestration_path)
                .to_string_lossy()
                .into_owned()
        };
        let entry = RegistryEntry {
            run_id: run_id.to_string(),
            orchestration,
            config: self.config.clone(),
            engine,
            pid: pid as u32,
            launcher: self.launcher.clone(),
            run_dir: self.run_dir.clone(),
            events: self.events.clone(),
            live_state: self.live_state.clone(),
            started_at: self.started_at.clone(),
        };
        if registry::write_entry(dir, &entry).is_ok() {
            self.written_run_id = Some(run_id.to_string());
        }
    }

    /// Removes this run's own entry, if one was ever written --
    /// idempotent, so a caller that calls it explicitly (every exit
    /// path that ends with `std::process::exit` rather than a plain
    /// return, which skips every other value's own `Drop` on the
    /// stack) and then still gets dropped the ordinary way afterwards
    /// removes nothing twice. Also checked against what is actually
    /// on disk right now: a launch handle never checks before it
    /// writes (see [`Self::ensure_written`]'s own doc comment), so a
    /// watch that wrote first, racing a launcher that hadn't written
    /// yet, can find its own entry already overwritten by the time
    /// it goes to remove it -- removing that would delete the
    /// launcher's own, still-live entry instead of this handle's own
    /// (now-gone) one.
    pub fn remove(&mut self) {
        let (Some(dir), Some(run_id)) = (&self.dir, self.written_run_id.take()) else {
            return;
        };
        let path = registry::registry_file_path(dir, &run_id);
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(entry) = serde_json::from_slice::<RegistryEntry>(&bytes) {
                if entry.launcher != self.launcher {
                    return;
                }
            }
        }
        let _ = registry::remove_entry(dir, &run_id);
    }
}

impl Drop for RegistryHandle {
    fn drop(&mut self) {
        self.remove();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oscilloscope_core::observe::Event;

    fn running_model(run_id: &str, pid: i32) -> RunModel {
        let mut model = RunModel::new();
        model.observe_event(&Event::RunStart {
            ts: "2026-01-01T00:00:00Z".to_string(),
            run_id: run_id.to_string(),
            pid: Some(pid),
            engine: Some("cof".to_string()),
            config: None,
        });
        model
    }

    fn handle_for(dir: &Path) -> RegistryHandle {
        RegistryHandle::new(
            Some(dir.to_path_buf()),
            Some(Path::new("/work/cfg.json")),
            Some("cof"),
            "osi 0.1.0".to_string(),
            Path::new("/work/run-dir"),
            Path::new("/work/run-dir/events.jsonl"),
            Path::new("/work/run-dir/state.live.json"),
            false,
        )
    }

    fn watch_handle_for(dir: &Path) -> RegistryHandle {
        RegistryHandle::new(
            Some(dir.to_path_buf()),
            None,
            None,
            "osi 0.1.0 watch".to_string(),
            Path::new("/work/run-dir"),
            Path::new("/work/run-dir/events.jsonl"),
            Path::new("/work/run-dir/state.live.json"),
            true,
        )
    }

    struct FixedLiveness(std::collections::HashSet<u32>);
    impl Liveness for FixedLiveness {
        fn is_alive(&self, pid: u32) -> bool {
            self.0.contains(&pid)
        }
    }

    #[test]
    fn ensure_written_is_a_noop_before_run_start_is_known() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        let mut handle = handle_for(&dir);
        handle.ensure_written(&RunModel::new(), "/work/doc.yml");
        assert!(!dir.exists());
    }

    #[test]
    fn ensure_written_then_dropped_writes_then_removes_the_entry() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        let model = running_model("run-1", 4242);
        {
            let mut handle = handle_for(&dir);
            handle.ensure_written(&model, "/work/doc.yml");
            let path = registry::registry_file_path(&dir, "run-1");
            assert!(path.exists());
            let bytes = std::fs::read(&path).unwrap();
            let entry: RegistryEntry = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(entry.run_id, "run-1");
            assert_eq!(entry.pid, 4242);
            assert_eq!(entry.orchestration, "/work/doc.yml");
            assert_eq!(entry.config, Some("/work/cfg.json".to_string()));
            assert_eq!(entry.engine, "cof");
            assert_eq!(entry.launcher, "osi 0.1.0");
        }
        assert!(!registry::registry_file_path(&dir, "run-1").exists());
    }

    #[test]
    fn ensure_written_is_idempotent_once_written() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        let model = running_model("run-1", 1);
        let mut handle = handle_for(&dir);
        handle.ensure_written(&model, "/work/doc.yml");
        let path = registry::registry_file_path(&dir, "run-1");
        let first_write = std::fs::metadata(&path).unwrap().modified().unwrap();
        // A later, different orchestration string must not overwrite
        // an already-written entry.
        handle.ensure_written(&model, "/work/other.yml");
        let second_write = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(first_write, second_write);
        let entry: RegistryEntry = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(entry.orchestration, "/work/doc.yml");
    }

    #[test]
    fn a_disabled_handle_writes_and_removes_nothing() {
        let mut handle = RegistryHandle::new(
            None,
            None,
            Some("cof"),
            "osi 0.1.0".to_string(),
            Path::new("/work/run-dir"),
            Path::new("/work/run-dir/events.jsonl"),
            Path::new("/work/run-dir/state.live.json"),
            false,
        );
        handle.ensure_written(&running_model("run-1", 1), "/work/doc.yml");
        assert!(handle.written_run_id.is_none());
    }

    #[test]
    fn ensure_written_records_this_handles_own_construction_time_as_started_at() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        let before = now_rfc3339();
        let mut handle = handle_for(&dir);
        handle.ensure_written(&running_model("run-1", 1), "/work/doc.yml");
        let after = now_rfc3339();
        let path = registry::registry_file_path(&dir, "run-1");
        let entry: RegistryEntry = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        // The engine's own `run_start` carries a different `ts`
        // (`running_model` sets it to `2026-01-01T00:00:00Z`) --
        // `started_at` must be this handle's own construction time,
        // not that one.
        assert_ne!(entry.started_at, "2026-01-01T00:00:00Z");
        assert!(before <= entry.started_at && entry.started_at <= after);
    }

    #[test]
    fn a_watch_handle_writes_its_own_entry_when_no_launcher_entry_exists() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        let mut handle = watch_handle_for(&dir);
        handle.ensure_written(&running_model("run-1", 4242), "/work/doc.yml");
        let path = registry::registry_file_path(&dir, "run-1");
        let entry: RegistryEntry = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(entry.launcher, "osi 0.1.0 watch");
    }

    #[test]
    fn a_watch_handle_never_overwrites_a_launchers_live_entry_and_removes_nothing() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        let mut launcher = handle_for(&dir);
        launcher.ensure_written(&running_model("run-1", 4242), "/work/doc.yml");
        let path = registry::registry_file_path(&dir, "run-1");
        let before = std::fs::read(&path).unwrap();

        let mut watch = watch_handle_for(&dir);
        watch.liveness = Box::new(FixedLiveness([4242].into_iter().collect()));
        watch.ensure_written(&running_model("run-1", 4242), "/work/doc.yml");
        let after = std::fs::read(&path).unwrap();
        assert_eq!(before, after, "the launcher's own entry must be untouched");

        drop(watch);
        assert!(
            path.exists(),
            "a watch that wrote nothing must remove nothing on detach"
        );
        drop(launcher);
        assert!(!path.exists());
    }

    #[test]
    fn a_launcher_overwrites_a_watch_entry_written_first_in_the_opposite_race() {
        // The reverse order from the test above: a watch can attach,
        // and observe `run_start`, before the launcher it's watching
        // gets its own first chance to write -- a launch handle is
        // never an `is_watch` one, so it always writes its own
        // authoritative entry regardless of what's already there.
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        let mut watch = watch_handle_for(&dir);
        watch.ensure_written(&running_model("run-1", 4242), "/work/doc.yml");
        let path = registry::registry_file_path(&dir, "run-1");
        let entry: RegistryEntry = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(entry.launcher, "osi 0.1.0 watch");

        let mut launcher = handle_for(&dir);
        launcher.ensure_written(&running_model("run-1", 4242), "/work/doc.yml");
        let entry: RegistryEntry = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(entry.launcher, "osi 0.1.0");

        drop(watch);
        assert!(
            path.exists(),
            "the watch's own remove must not delete the launcher's entry it overwrote"
        );
        drop(launcher);
        assert!(!path.exists());
    }

    #[test]
    fn a_watch_handle_writes_a_relative_document_path_exactly_as_given() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        let mut handle = watch_handle_for(&dir);
        // Relative to the run's own launcher, not to this watch's
        // own cwd -- resolving it against the watcher's cwd would
        // only ever produce a wrong path.
        handle.ensure_written(&running_model("run-1", 1), "do.yml");
        let path = registry::registry_file_path(&dir, "run-1");
        let entry: RegistryEntry = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(entry.orchestration, "do.yml");
    }

    #[test]
    fn a_watch_handle_still_makes_an_absolute_document_path_canonical() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        let mut handle = watch_handle_for(&dir);
        handle.ensure_written(&running_model("run-1", 1), "/work/doc.yml");
        let path = registry::registry_file_path(&dir, "run-1");
        let entry: RegistryEntry = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(entry.orchestration, "/work/doc.yml");
    }

    #[test]
    fn ensure_written_takes_only_the_first_word_of_the_watched_runs_own_engine_label() {
        let mut model = RunModel::new();
        model.observe_event(&Event::RunStart {
            ts: "2026-01-01T00:00:00Z".to_string(),
            run_id: "run-1".to_string(),
            pid: Some(1),
            engine: Some("cof 0.1.0".to_string()),
            config: None,
        });
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        let mut handle = watch_handle_for(&dir);
        handle.ensure_written(&model, "/work/doc.yml");
        let path = registry::registry_file_path(&dir, "run-1");
        let entry: RegistryEntry = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(entry.engine, "cof");
    }
}
