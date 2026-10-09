//! `--live-state`: `cli/live_state.py::LiveStateMirror`, ported (issue
//! #431's "`--live-state`" section).
//!
//! Circuitry's own version runs a background thread so a slow disk
//! never stalls the effect dispatching it mirrors, coalescing writes
//! under a `threading.Condition`. electricity has no worker threads of
//! its own to stall -- the VM is single-threaded, cooperative `async`
//! (DESIGN.md §6.1-6.2) -- so this port coalesces the same way (at most
//! one write per [`LIVE_STATE_INTERVAL`]) but does so synchronously, on
//! whichever call lands on or after the next due time: there is no
//! second thread for a `--live-state` write to race against the store
//! lock Python's own version has to avoid holding during I/O (this
//! crate's `Store` has no such lock to begin with -- `std::cell::RefCell`
//! borrows are taken and released well before this module ever runs).
//!
//! The one write this module makes unconditionally, regardless of
//! timing, is [`LiveStateMirror::close`]'s final one -- `--live-state`'s
//! own promise to end equal to `--out` (issue #431's acceptance
//! criteria).

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

    /// A later write: skipped entirely unless at least [`Self::interval`]
    /// has passed since the last one landed -- *state* is computed lazily
    /// (only when actually due) so a caller's own snapshot (`Store::saved`)
    /// is never taken for nothing. A failure here is recorded (see
    /// [`LiveStateMirror::close`]), never propagated: the mirror is for
    /// watchers, not the run's own result.
    pub fn write_coalesced(&self, state: impl FnOnce() -> Value) {
        let now = Instant::now();
        let due = self.next_due.get().is_none_or(|due| now >= due);
        if !due {
            return;
        }
        self.next_due.set(Some(now + self.interval));
        if write_atomic(&self.path, &render_state(&state(), false)).is_err() {
            self.had_failure.set(true);
        }
    }

    /// The final write (issue #431's run-wiring step 19): unconditional
    /// regardless of timing, so `--live-state` ends equal to `--out`.
    /// Returns whether any write over this mirror's whole lifetime --
    /// mid-run or this one -- failed, so the caller can fold that into
    /// one warning on the run's result.
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
    fn a_coalesced_write_before_the_interval_is_skipped() {
        let path = temp_path("coalesced");
        let mirror = LiveStateMirror::with_interval(path.clone(), Duration::from_secs(60));
        mirror.write_initial(&Value::Dict(Dict::new())).unwrap();
        let mut dict = Dict::new();
        dict.insert(Value::Str("x".to_string()), Value::from(1i64));
        mirror.write_coalesced(|| Value::Dict(dict));
        // Still the initial (empty) snapshot: the second write landed
        // well inside the interval and was skipped.
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}\n");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_coalesced_write_after_the_interval_lands() {
        let path = temp_path("due");
        let mirror = LiveStateMirror::with_interval(path.clone(), Duration::from_millis(1));
        mirror.write_initial(&Value::Dict(Dict::new())).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let mut dict = Dict::new();
        dict.insert(Value::Str("x".to_string()), Value::from(1i64));
        mirror.write_coalesced(|| Value::Dict(dict));
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"x\": 1}\n");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn close_always_writes_the_final_state_regardless_of_timing() {
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
