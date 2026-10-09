//! `--live-state`: `cli/live_state.py::LiveStateMirror`, ported (issue
//! #431's "`--live-state`" section).
//!
//! Circuitry's own version runs a background thread so a slow disk
//! never stalls the effect dispatching it mirrors, coalescing writes
//! under a `threading.Condition`. electricity has no worker threads of
//! its own, and no second thread for a `--live-state` write to race a
//! store borrow against either -- but it still never takes a snapshot
//! (`Store::saved`) or does I/O from inside [`RunObserver::write`]
//! itself (PR #441 review finding 7): that hook fires synchronously,
//! *from inside* whatever `exec::dynamic`/`exec::tool` call just wrote
//! to the store, which may still be holding a `RefCell` borrow a step
//! or two up its own call stack -- `Store::saved` walking the same
//! tree right then could panic on an already-borrowed node. Instead,
//! [`LiveStateMirror::mark_pending`] (what the observer hook actually
//! calls) only ever sets a flag; [`LiveStateMirror::flush_if_due`] is
//! the one place that ever calls `Store::saved` or touches the
//! filesystem for a coalesced write, and `run_orchestration`'s own
//! `tokio::select!` loop (`src/lib.rs`) calls it only between
//! `execute_root` polls -- never while that future is still holding
//! control (and so never while any node's own borrow from this exact
//! poll could still be live).
//!
//! The one write this module makes unconditionally, regardless of
//! timing or the pending flag, is [`LiveStateMirror::close`]'s final
//! one -- `--live-state`'s own promise to end equal to `--out` (issue
//! #431's acceptance criteria).

use crate::out::render_state;
use electricity_value::Value;
use std::cell::Cell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// `cli/live_state.py::LIVE_STATE_INTERVAL_SECONDS`.
pub const LIVE_STATE_INTERVAL: Duration = Duration::from_millis(500);

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes *payload* to *path* atomically: a sibling temp file, written
/// in full, then renamed into place in the same directory (so the
/// rename stays on one filesystem) -- `cli/live_state.py::_replace_file`'s
/// own tmp-file-plus-rename shape, with a process-id-and-counter
/// temp name rather than `tempfile.mkstemp`'s `O_CREAT | O_EXCL`: this
/// crate has no local-attacker threat model of its own to defend
/// against (every path it ever writes to is one the same CLI invocation
/// was given directly), so a predictable-but-unique name is enough to
/// avoid colliding with a concurrent write to the very same path.
fn write_atomic(path: &Path, payload: &str) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    fs::create_dir_all(&parent)?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("live-state");
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_path = parent.join(format!(".{file_name}.{}.{counter}.tmp", std::process::id()));
    let result = fs::write(&tmp_path, payload).and_then(|()| fs::rename(&tmp_path, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

/// The `--live-state` writer -- see this module's own doc comment.
pub struct LiveStateMirror {
    path: PathBuf,
    interval: Duration,
    next_due: Cell<Option<Instant>>,
    /// Set by [`Self::mark_pending`] (the observer hook); cleared by
    /// [`Self::flush_if_due`] once it actually writes. `close` ignores
    /// this entirely -- its own write is unconditional.
    pending: Cell<bool>,
    had_failure: Cell<bool>,
}

impl LiveStateMirror {
    pub fn new(path: PathBuf) -> Self {
        LiveStateMirror::with_interval(path, LIVE_STATE_INTERVAL)
    }

    fn with_interval(path: PathBuf, interval: Duration) -> Self {
        LiveStateMirror {
            path,
            interval,
            next_due: Cell::new(None),
            pending: Cell::new(false),
            had_failure: Cell::new(false),
        }
    }

    /// The run's very first write: synchronous and fatal on error
    /// (issue #431's run-wiring step 16) -- an unwritable path fails
    /// the run up front, before any effect, rather than silently never
    /// mirroring anything.
    pub fn write_initial(&self, state: &Value) -> io::Result<()> {
        write_atomic(&self.path, &render_state(state, false))?;
        self.next_due.set(Some(Instant::now() + self.interval));
        Ok(())
    }

    /// Records that the store has changed since the last write --
    /// [`crate::run::RunObserver::write`]'s own hook, called
    /// synchronously from inside whatever just wrote to the store.
    /// Never touches the store or the filesystem itself (this module's
    /// own doc comment).
    pub fn mark_pending(&self) {
        self.pending.set(true);
    }

    /// Writes the current state if, and only if, both a change is
    /// pending ([`Self::mark_pending`] was called at least once since
    /// the last write) and at least [`Self::interval`] has passed since
    /// then -- *snapshot* (`Store::saved`) is computed lazily, only
    /// when a write is actually about to happen, so a caller that
    /// polls this far more often than the interval itself (as
    /// `run_orchestration`'s own select loop does, to stay responsive)
    /// never pays for a snapshot it then discards. A failure here is
    /// recorded (see [`Self::close`]), never propagated: the mirror is
    /// for watchers, not the run's own result.
    pub fn flush_if_due(&self, snapshot: impl FnOnce() -> Value) {
        if !self.pending.get() {
            return;
        }
        let now = Instant::now();
        let due = self.next_due.get().is_none_or(|due| now >= due);
        if !due {
            return;
        }
        self.pending.set(false);
        self.next_due.set(Some(now + self.interval));
        if write_atomic(&self.path, &render_state(&snapshot(), false)).is_err() {
            self.had_failure.set(true);
        }
    }

    /// The final write (issue #431's run-wiring step 19): unconditional
    /// regardless of timing or the pending flag, so `--live-state` ends
    /// equal to `--out`. Returns whether any write over this mirror's
    /// whole lifetime -- mid-run or this one -- failed, so the caller
    /// can fold that into one warning on the run's result.
    pub fn close(&self, state: &Value) -> bool {
        if write_atomic(&self.path, &render_state(state, false)).is_err() {
            self.had_failure.set(true);
        }
        self.had_failure.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "electricity-live-state-test-{}-{name}",
            std::process::id()
        ))
    }

    #[test]
    fn the_first_write_is_synchronous() {
        let path = temp_path("first");
        let mirror = LiveStateMirror::new(path.clone());
        mirror.write_initial(&Value::Dict(Dict::new())).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}\n");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn flush_if_due_does_nothing_with_no_pending_change() {
        let path = temp_path("no-pending");
        let mirror = LiveStateMirror::with_interval(path.clone(), Duration::from_millis(1));
        mirror.write_initial(&Value::Dict(Dict::new())).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        // Due, but nothing marked pending -- the closure below must
        // never even be called.
        mirror.flush_if_due(|| panic!("snapshot taken with nothing pending"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}\n");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_pending_change_before_the_interval_is_skipped() {
        let path = temp_path("coalesced");
        let mirror = LiveStateMirror::with_interval(path.clone(), Duration::from_secs(60));
        mirror.write_initial(&Value::Dict(Dict::new())).unwrap();
        mirror.mark_pending();
        let mut dict = Dict::new();
        dict.insert(Value::Str("x".to_string()), Value::from(1i64));
        mirror.flush_if_due(|| Value::Dict(dict));
        // Still the initial (empty) snapshot: the pending write landed
        // well inside the interval and was skipped.
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}\n");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_pending_change_after_the_interval_lands() {
        let path = temp_path("due");
        let mirror = LiveStateMirror::with_interval(path.clone(), Duration::from_millis(1));
        mirror.write_initial(&Value::Dict(Dict::new())).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        mirror.mark_pending();
        let mut dict = Dict::new();
        dict.insert(Value::Str("x".to_string()), Value::from(1i64));
        mirror.flush_if_due(|| Value::Dict(dict));
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"x\": 1}\n");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_second_pending_change_before_the_next_interval_still_lands_once() {
        let path = temp_path("re-pending");
        let mirror = LiveStateMirror::with_interval(path.clone(), Duration::from_millis(1));
        mirror.write_initial(&Value::Dict(Dict::new())).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        mirror.mark_pending();
        let mut first = Dict::new();
        first.insert(Value::Str("x".to_string()), Value::from(1i64));
        mirror.flush_if_due(|| Value::Dict(first));
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"x\": 1}\n");
        // Nothing pending right after a flush -- a poll landing before
        // the next change is marked must be a no-op, proving the flag
        // (not just the timer) gates every write.
        mirror.flush_if_due(|| panic!("snapshot taken with nothing pending"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"x\": 1}\n");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn close_always_writes_the_final_state_regardless_of_timing_or_pending() {
        let path = temp_path("close");
        let mirror = LiveStateMirror::with_interval(path.clone(), Duration::from_secs(60));
        mirror.write_initial(&Value::Dict(Dict::new())).unwrap();
        let mut dict = Dict::new();
        dict.insert(Value::Str("done".to_string()), Value::Bool(true));
        let had_failure = mirror.close(&Value::Dict(dict));
        assert!(!had_failure);
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"done\": true}\n");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_write_failure_is_recorded_not_propagated() {
        // A path through a file (not a directory) can never be created
        // as a directory out from under it -- every write under it
        // fails with ENOTDIR, exercising the recorded-failure branch.
        let blocker = temp_path("blocker-file");
        fs::write(&blocker, "not a directory").unwrap();
        let path = blocker.join("state.json");
        let mirror = LiveStateMirror::with_interval(path, Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));
        let mut dict = Dict::new();
        dict.insert(Value::Str("x".to_string()), Value::from(1i64));
        let had_failure = mirror.close(&Value::Dict(dict));
        assert!(had_failure);
        fs::remove_file(&blocker).unwrap();
    }
}
