//! `osi`'s own wiring for the run registry (issue #698's "Settled
//! 1"): the one place in the binary crate that reads `OSI_RUNS_DIR`/
//! `$XDG_RUNTIME_DIR`/`$XDG_STATE_HOME`/`$HOME`, and decides *when*
//! an entry is written and removed for one `osi <doc>` or `osi
//! watch` invocation. `oscilloscope_core::registry` holds every
//! other rule (the file format, liveness, pruning).
//!
//! An entry needs a `run_id`, which only exists once the run (or the
//! watched run) has actually announced itself with a `run_start`
//! event -- so [`RegistryHandle::ensure_written`] is a no-op, every
//! time it's called, until that happens, and writes the entry
//! exactly once after it does. [`RegistryHandle`]'s own [`Drop`]
//! removes it again -- covering the run's own end, a detach, and
//! osi's own exit (including every signal path, since every one of
//! them already unwinds back to the scope that owns this handle)
//! without this module needing to know which of those actually
//! happened.

use std::path::{Path, PathBuf};

use oscilloscope_core::model::RunModel;
use oscilloscope_core::registry::{self, RegistryEntry};

use crate::history_io::absolute_path;

/// `OSI_RUNS_DIR`, else `$XDG_RUNTIME_DIR/circuitry/osi/runs`, else
/// `$XDG_STATE_HOME/circuitry/osi/runs`, else `~/.local/state/
/// circuitry/osi/runs` (issue #698's "Settled 1"). Reads the process
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
    run_dir: String,
    events: String,
    live_state: String,
    /// The `run_id` this handle has actually written an entry for,
    /// once it has -- `None` until then, and the key `Drop` removes
    /// by.
    written_run_id: Option<String>,
}

impl RegistryHandle {
    /// Every path but `orchestration` (given fresh to
    /// [`Self::ensure_written`], see its own doc comment) is made
    /// absolute here (issue #698's "Settled 1": "document and config
    /// as absolute paths") -- a caller's own `--out-dir` may be
    /// relative, and the engine's own command still spawns fine with
    /// a relative run directory (it inherits osi's own cwd), but a
    /// registry entry is read by a *different* process, possibly
    /// from a different cwd, so every path in it is resolved once,
    /// here, rather than left for a reader to guess at.
    pub fn new(
        dir: Option<PathBuf>,
        config: Option<&Path>,
        engine: Option<&str>,
        launcher: String,
        run_dir: &Path,
        events: &Path,
        live_state: &Path,
    ) -> Self {
        RegistryHandle {
            dir,
            config: config.map(|c| absolute_path(c).to_string_lossy().into_owned()),
            engine: engine.map(str::to_string),
            launcher,
            run_dir: absolute_path(run_dir).to_string_lossy().into_owned(),
            events: absolute_path(events).to_string_lossy().into_owned(),
            live_state: absolute_path(live_state).to_string_lossy().into_owned(),
            written_run_id: None,
        }
    }

    /// Writes this run's own entry, the first time `model` carries a
    /// `run_start` with both a `run_id` and a pid -- a no-op on every
    /// call before that, and on every call after the entry is
    /// already written (issue #698's "written once `run_start` is
    /// known"). `orchestration` is taken fresh on every call (made
    /// absolute here) rather than fixed at construction, since `osi
    /// <doc>`'s caller always has it already but `osi watch`'s own
    /// document is resolved lazily (the same document a redraw's own
    /// header shows, `watch_document_name`) -- by the time `run_id`
    /// is actually known, whichever caller it is already has its own
    /// best answer in hand to pass in.
    pub fn ensure_written(&mut self, model: &RunModel, orchestration: &str) {
        if self.written_run_id.is_some() {
            return;
        }
        let Some(dir) = &self.dir else { return };
        let Some(run_id) = model.run_start_id() else {
            return;
        };
        let Some(pid) = model.run_start_pid() else {
            return;
        };
        let Some(engine) = self
            .engine
            .clone()
            .or_else(|| model.run_start_engine().map(str::to_string))
        else {
            return;
        };
        let entry = RegistryEntry {
            run_id: run_id.to_string(),
            orchestration: absolute_path(Path::new(orchestration))
                .to_string_lossy()
                .into_owned(),
            config: self.config.clone(),
            engine,
            pid: pid as u32,
            launcher: self.launcher.clone(),
            run_dir: self.run_dir.clone(),
            events: self.events.clone(),
            live_state: self.live_state.clone(),
            started_at: model.run_start_ts().unwrap_or_default().to_string(),
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
    /// removes nothing twice.
    pub fn remove(&mut self) {
        if let (Some(dir), Some(run_id)) = (&self.dir, self.written_run_id.take()) {
            let _ = registry::remove_entry(dir, &run_id);
        }
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
        )
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
        );
        handle.ensure_written(&running_model("run-1", 1), "/work/doc.yml");
        assert!(handle.written_run_id.is_none());
    }
}
