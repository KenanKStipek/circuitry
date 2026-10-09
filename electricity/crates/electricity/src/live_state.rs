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
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// `cli/live_state.py::LIVE_STATE_INTERVAL_SECONDS`.
pub const LIVE_STATE_INTERVAL: Duration = Duration::from_millis(500);

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// How many candidate temp names [`create_temp_file`] tries before
/// giving up -- each collision (another writer's own live temp file,
/// or an attacker's planted symlink, see that function's own doc
/// comment) just means "pick another name", so this should never need
/// more than a handful of attempts in practice; the ceiling only turns
/// a pathological case (a directory an attacker has filled with every
/// name this could ever generate) into an `io::Error` instead of an
/// infinite loop.
const MAX_TEMP_NAME_ATTEMPTS: u32 = 1000;

/// Opens a fresh, exclusively-created temp file for *path*'s own
/// atomic write, alongside it in the same directory -- `tempfile::
/// mkstemp`'s own two defenses, ported directly rather than `fs::
/// write`'s plain "truncate or create" (PR #441 review finding 3):
///
/// - `O_CREAT | O_EXCL` (`create_new(true)`): fails outright if
///   *anything* already exists at the candidate path, symlink
///   included -- `fs::write`'s plain open follows a symlink there and
///   happily writes through it to whatever it points at. A
///   predictable name (pid plus a process-local counter) is exactly
///   what makes that attack possible: another local user able to
///   predict this run's own next temp name could plant a symlink at
///   it ahead of time and have this process overwrite -- or, since
///   the file was previously world-readable, read the contents of --
///   whatever that symlink points to. `O_EXCL` closes that window: a
///   pre-placed symlink (or file) at the exact name this call tries
///   makes this `Err(AlreadyExists)`, never followed, so
///   [`write_atomic`] just retries under a new name instead.
/// - mode `0600` (`cli/live_state.py::_replace_file`'s own final live
///   file is `0600` too): nobody but this process's own user can even
///   read the --live-state mirror while a run is in progress, not
///   just once it lands at *path* via the rename below.
fn create_temp_file(parent: &Path, file_name: &str) -> io::Result<(PathBuf, fs::File)> {
    let pid = std::process::id();
    for _ in 0..MAX_TEMP_NAME_ATTEMPTS {
        let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".{file_name}.{pid}.{counter}.tmp"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "could not create a unique temp file alongside {}",
            parent.display()
        ),
    ))
}

/// Writes *payload* to *path* atomically: a sibling temp file, written
/// in full, then renamed into place in the same directory (so the
/// rename stays on one filesystem) -- `cli/live_state.py::_replace_file`'s
/// own tmp-file-plus-rename shape, with [`create_temp_file`]'s own
/// `mkstemp`-equivalent exclusive create standing in for Python's own
/// `tempfile.mkstemp` call.
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
    let (tmp_path, mut file) = create_temp_file(&parent, file_name)?;
    let result = file
        .write_all(payload.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&tmp_path, path));
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

    // PR #441 review finding 3.
    #[test]
    fn the_final_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let path = temp_path("mode");
        let mirror = LiveStateMirror::new(path.clone());
        mirror.write_initial(&Value::Dict(Dict::new())).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_symlink_planted_at_the_predictable_temp_name_is_never_followed() {
        // Reproduces the attack directly: predict `create_temp_file`'s
        // very first candidate name for this `path`/pid and plant a
        // symlink there ahead of time, pointed at a file this test
        // owns outside the live-state directory entirely. If
        // `write_atomic` ever followed it (the pre-fix `fs::write`
        // behaviour), the target's contents would become the state
        // payload; with `O_EXCL` the create fails instead and a later
        // counter value is used, leaving the target untouched.
        let path = temp_path("symlink-attack");
        let parent = path.parent().unwrap();
        fs::create_dir_all(parent).unwrap();
        let file_name = path.file_name().unwrap().to_str().unwrap();
        let pid = std::process::id();
        let next_counter = TMP_COUNTER.load(Ordering::Relaxed);
        let predicted = parent.join(format!(".{file_name}.{pid}.{next_counter}.tmp"));
        let target = temp_path("symlink-attack-target");
        fs::write(&target, "attacker-controlled").unwrap();
        std::os::unix::fs::symlink(&target, &predicted).unwrap();

        let mirror = LiveStateMirror::new(path.clone());
        mirror.write_initial(&Value::Dict(Dict::new())).unwrap();

        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "attacker-controlled",
            "the planted symlink's target must never be written through"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}\n");
        // The planted symlink itself is left alone (never consumed by
        // `fs::rename`, which only ever targets the real temp file this
        // call actually created under a different name).
        assert!(predicted.symlink_metadata().is_ok());

        fs::remove_file(&predicted).unwrap();
        fs::remove_file(&target).unwrap();
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
