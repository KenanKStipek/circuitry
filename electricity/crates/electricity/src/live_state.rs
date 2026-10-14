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
use electricity_bytecode::EffectPath;
use electricity_value::{Dict, Value};
use electricity_vm::{NodeRef, Store};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
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

/// `core/use.py::_grafted_snapshot`'s own seam (M1 lane I
/// prerequisites, issue #448, item U6): *root* with *node_path*'s own
/// existing value overlaid by *child_snapshot*'s mirrored effects --
/// every key of the child's own `prime` root except `value`/`meta`
/// (the child document's own effects, not its own top-level result).
/// `None`-shaped input (no `prime` dict in *child_snapshot*, or
/// nothing left once `value`/`meta` are stripped) returns *root*
/// unchanged, Python's own two early-return branches. A plain,
/// stateless `Value` -> `Value` transform -- unlike Python's own
/// `replace_node` (which walks a live `Store`'s own mutable state
/// dict), this only ever touches a `Store::saved` snapshot already
/// taken, so there is no node identity/aliasing of a live store to
/// preserve or disturb.
fn grafted_snapshot(root: &Value, node_path: &str, child_snapshot: &Value) -> Value {
    let Some(child_effects) = child_snapshot
        .as_dict()
        .and_then(|d| d.get(&Value::Str("prime".to_string())))
        .and_then(Value::as_dict)
    else {
        return root.clone();
    };
    let mirrored: Dict = child_effects
        .iter()
        .filter(|(k, _)| !matches!(k, Value::Str(s) if s == "value" || s == "meta"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if mirrored.is_empty() {
        return root.clone();
    }
    let segments: Vec<&str> = node_path.split('.').collect();
    overlay_at_path(root, &segments, &mirrored)
}

/// Walks *root* down *segments*, returning a full copy with the dict at
/// that path's own keys from *mirrored* inserted/overwritten -- every
/// other key already there (a `use` node's own `value`/`meta`
/// included) survives untouched, Python's own `{**node, **mirrored}`.
/// A missing/non-dict node along the way is treated as an empty dict,
/// same as [`crate::out::render_state`]'s own tolerant reads --
/// `node_path` always names a real, already-created node in practice
/// (the use effect's own `store.fire_effect_start` already ran before
/// any child write could happen), so this only ever matters for a test
/// fixture that skips that step.
fn overlay_at_path(root: &Value, segments: &[&str], mirrored: &Dict) -> Value {
    let mut dict = root.as_dict().cloned().unwrap_or_default();
    match segments {
        [] => {
            for (k, v) in mirrored {
                dict.insert(k.clone(), v.clone());
            }
        }
        [first, rest @ ..] => {
            let key = Value::Str(first.to_string());
            let existing = dict.get(&key).cloned().unwrap_or(Value::Dict(Dict::new()));
            dict.insert(key, overlay_at_path(&existing, rest, mirrored));
        }
    }
    Value::Dict(dict)
}

/// Issue #465's own overlay: *root* with *node_path*'s own existing
/// node (a tree `dynamic`/loop's own container, still holding whatever
/// skeleton `value`/`meta` it wrote before dispatching) overlaid by
/// every branch in *branches*, in index order -- `core/store/
/// store.py::Store.parallel_branches`'s own publisher, `node =
/// dict(self.state); for snapshot in latest: if snapshot is not None:
/// node.update(snapshot)`. Each branch's own current content is read
/// straight off its live [`NodeRef`] via [`Store::saved`] -- lazily,
/// right here, never pushed on every write (this function is only
/// ever called from [`LiveStateMirror::apply_grafts`], itself only
/// called from [`LiveStateMirror::flush_if_due`], the one place that
/// enforces the whole mirror's own coalescing). A branch that hasn't
/// written anything yet resolves to an empty dict, contributing
/// nothing -- the same as cof's own `latest[index] is None` check
/// skipping it, since merging an empty dict is a no-op either way.
/// A branch index earlier in *branches* whose keys a later branch
/// also wrote loses that key, exactly as cof's own ascending-index
/// `node.update` loop does.
fn tree_overlay_snapshot(
    root: &Value,
    node_path: &str,
    store: &Store,
    branches: &[NodeRef],
) -> Value {
    let mut merged = Dict::new();
    for branch in branches {
        if let Some(d) = store.saved(branch).as_dict() {
            for (k, v) in d.iter() {
                merged.insert(k.clone(), v.clone());
            }
        }
    }
    if merged.is_empty() {
        return root.clone();
    }
    let segments: Vec<&str> = node_path.split('.').collect();
    overlay_at_path(root, &segments, &merged)
}

/// One [`LiveStateMirror::grafts`] entry: either a `use` child's own
/// latest pushed snapshot ([`LiveStateMirror::graft_child_write`]), or
/// a tree's own still-live branch [`NodeRef`]s, read lazily
/// ([`LiveStateMirror::register_tree_branches`]) -- see each
/// variant's own constructor for which.
enum GraftEntry {
    Child(Value),
    Tree(Vec<NodeRef>),
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
    /// Every currently in-flight overlay -- a `use` child's own latest
    /// pushed write ([`Self::graft_child_write`]/[`Self::clear_graft`],
    /// issue #448 item U6) or a tree's own still-live branches
    /// ([`Self::register_tree_branches`]/[`Self::clear_tree_branches`],
    /// issue #465) -- keyed by the overlaid node's own absolute dotted
    /// path. A `BTreeMap`, not a `HashMap`, so [`Self::apply_grafts`]
    /// walks shallower paths before any deeper one nested under them (a
    /// dotted path that is a strict prefix of another always sorts
    /// first), composing a nested `use`/tree correctly on top of its
    /// own parent's, in whichever order the two kinds actually nest. A
    /// plain map, not a stack: a call for a path already present simply
    /// replaces that path's own entry, and a tree's own simultaneous
    /// sibling `use` branches (or a `use` child's own nested tree) each
    /// keep their own independent entry.
    grafts: RefCell<BTreeMap<String, GraftEntry>>,
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
            grafts: RefCell::new(BTreeMap::new()),
        }
    }

    /// [`electricity_vm::RunObserver::graft_child_write`]'s own
    /// implementation: records *snapshot* as *node_path*'s own latest
    /// grafted overlay (replacing whatever was there from an earlier
    /// write at the same path) and marks a mid-run write pending, the
    /// same as an ordinary [`Self::mark_pending`] call -- a graft is
    /// itself a store change a live-state reader should eventually
    /// see.
    pub fn graft_child_write(&self, node_path: &EffectPath, snapshot: &Value) {
        self.grafts
            .borrow_mut()
            .insert(node_path.to_string(), GraftEntry::Child(snapshot.clone()));
        self.mark_pending();
    }

    /// [`electricity_vm::RunObserver::clear_graft`]'s own
    /// implementation: *node_path* no longer needs an overlay (the
    /// child it named finished) -- a no-op if nothing was grafted
    /// there (or it was already cleared).
    pub fn clear_graft(&self, node_path: &EffectPath) {
        self.grafts.borrow_mut().remove(&node_path.to_string());
    }

    /// [`electricity_vm::RunObserver::register_tree_branches`]'s own
    /// implementation: records *branches* (cloned `Rc`s, cheap) as
    /// *node_path*'s own tree overlay, replacing whatever was there
    /// from an earlier registration at the same path. Unlike
    /// [`Self::graft_child_write`] this never marks a write pending on
    /// its own -- it runs once, before any branch has written anything
    /// at all, so there would be nothing new to publish yet; every
    /// branch's own first effect write already calls [`Self::
    /// mark_pending`] through the ordinary [`electricity_vm::
    /// RunObserver::write`] hook.
    pub fn register_tree_branches(&self, node_path: &EffectPath, branches: &[NodeRef]) {
        self.grafts
            .borrow_mut()
            .insert(node_path.to_string(), GraftEntry::Tree(branches.to_vec()));
    }

    /// [`electricity_vm::RunObserver::clear_tree_branches`]'s own
    /// implementation: *node_path*'s own tree has merged -- a no-op if
    /// nothing was registered there (or it was already cleared).
    pub fn clear_tree_branches(&self, node_path: &EffectPath) {
        self.grafts.borrow_mut().remove(&node_path.to_string());
    }

    /// *snapshot* with every currently active overlay applied,
    /// shallowest path first ([`BTreeMap`]'s own sorted iteration order
    /// over a dotted path -- this struct's own `grafts` field doc
    /// comment). *store* resolves a [`GraftEntry::Tree`] entry's own
    /// branch [`NodeRef`]s lazily, right here -- the only place any of
    /// them is ever read. Only [`Self::flush_if_due`] ever calls this:
    /// [`Self::write_initial`] runs before anything could have
    /// grafted, and [`Self::close`] writes the final, already-really-
    /// merged state, where no overlay should ever still apply.
    fn apply_grafts(&self, store: &Store, snapshot: Value) -> Value {
        self.grafts
            .borrow()
            .iter()
            .fold(snapshot, |acc, (path, entry)| match entry {
                GraftEntry::Child(child) => grafted_snapshot(&acc, path, child),
                GraftEntry::Tree(branches) => tree_overlay_snapshot(&acc, path, store, branches),
            })
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
    pub fn flush_if_due(&self, store: &Store, snapshot: impl FnOnce() -> Value) {
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
        let state = self.apply_grafts(store, snapshot());
        if let Err(err) = write_atomic(&self.path, &render_state(&state, false)) {
            self.had_failure.set(true);
            // `cli/live_state.py::LiveStateMirror._write`'s own
            // `logger.warning("Could not write --live-state %s: %s",
            // self._path, exc)`, once per failed write (issue #442) --
            // the OS-specific suffix is Rust's own `io::Error::Display`,
            // not byte-for-byte Python's `OSError.__str__` (same
            // long-standing divergence as `electricity-compiler`'s own
            // prompt-file read errors).
            log::warn!(
                "Could not write --live-state {}: {err}",
                self.path.display()
            );
        }
    }

    /// The final write (issue #431's run-wiring step 19): unconditional
    /// regardless of timing or the pending flag, so `--live-state` ends
    /// equal to `--out`. Returns whether any write over this mirror's
    /// whole lifetime -- mid-run or this one -- failed, so the caller
    /// can fold that into one warning on the run's result.
    pub fn close(&self, state: &Value) -> bool {
        if let Err(err) = write_atomic(&self.path, &render_state(state, false)) {
            self.had_failure.set(true);
            log::warn!(
                "Could not write --live-state {}: {err}",
                self.path.display()
            );
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
    fn a_grafted_child_write_mirrors_under_the_parent_use_node() {
        // M1 lane I prerequisites (issue #448, item U6): a parent
        // store with a `use` node mid-flight (`value: null`, an
        // ordinary `meta`) -- the child's own current snapshot, once
        // grafted, must mirror its own effects under that node without
        // disturbing the node's own `value`/`meta`.
        let path = temp_path("grafted");
        let mirror = LiveStateMirror::with_interval(path.clone(), Duration::from_millis(1));

        let mut use_meta = Dict::new();
        use_meta.insert(Value::Str("error".to_string()), Value::None);
        let mut use_node = Dict::new();
        use_node.insert(Value::Str("value".to_string()), Value::None);
        use_node.insert(Value::Str("meta".to_string()), Value::Dict(use_meta));
        let mut prime = Dict::new();
        prime.insert(
            Value::Str("fetch".to_string()),
            Value::Dict(use_node.clone()),
        );
        let mut root = Dict::new();
        root.insert(Value::Str("prime".to_string()), Value::Dict(prime));
        let root = Value::Dict(root);

        mirror.write_initial(&root).unwrap();
        std::thread::sleep(Duration::from_millis(5));

        // The child's own current snapshot: an `inner` tool effect
        // already ran inside it; the child's own top-level `value`/
        // `meta` are never mirrored (the parent's `use` node's own
        // decide those, once it actually merges for real).
        let mut inner = Dict::new();
        inner.insert(Value::Str("value".to_string()), Value::from("hi"));
        inner.insert(Value::Str("meta".to_string()), Value::Dict(Dict::new()));
        let mut child_prime = Dict::new();
        child_prime.insert(Value::Str("value".to_string()), Value::None);
        child_prime.insert(Value::Str("meta".to_string()), Value::Dict(Dict::new()));
        child_prime.insert(Value::Str("inner".to_string()), Value::Dict(inner.clone()));
        let mut child_snapshot = Dict::new();
        child_snapshot.insert(Value::Str("prime".to_string()), Value::Dict(child_prime));
        let child_snapshot = Value::Dict(child_snapshot);

        let node_path = EffectPath::root().push_name("fetch");
        mirror.graft_child_write(&node_path, &child_snapshot);
        let store = Store::new();
        mirror.flush_if_due(&store, || root.clone());

        // The expected published state: the use node's own `value`/
        // `meta` untouched, with `inner` mirrored in alongside them.
        let mut expected_fetch = use_node;
        expected_fetch.insert(Value::Str("inner".to_string()), Value::Dict(inner));
        let mut expected_prime = Dict::new();
        expected_prime.insert(Value::Str("fetch".to_string()), Value::Dict(expected_fetch));
        let mut expected_root = Dict::new();
        expected_root.insert(Value::Str("prime".to_string()), Value::Dict(expected_prime));

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            render_state(&Value::Dict(expected_root), false)
        );

        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn clear_graft_removes_a_path_so_a_later_flush_no_longer_overlays_it() {
        let path = temp_path("clear-grafted");
        let mirror = LiveStateMirror::with_interval(path.clone(), Duration::from_millis(1));
        mirror.write_initial(&Value::Dict(Dict::new())).unwrap();
        std::thread::sleep(Duration::from_millis(5));

        let mut child_prime = Dict::new();
        child_prime.insert(Value::Str("value".to_string()), Value::None);
        child_prime.insert(Value::Str("meta".to_string()), Value::Dict(Dict::new()));
        child_prime.insert(Value::Str("inner".to_string()), Value::from("leftover"));
        let mut child_snapshot = Dict::new();
        child_snapshot.insert(Value::Str("prime".to_string()), Value::Dict(child_prime));
        let child_snapshot = Value::Dict(child_snapshot);
        let node_path = EffectPath::root().push_name("fetch");
        mirror.graft_child_write(&node_path, &child_snapshot);
        mirror.clear_graft(&node_path);

        mirror.mark_pending();
        let store = Store::new();
        mirror.flush_if_due(&store, || Value::Dict(Dict::new()));
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}\n");

        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn grafted_snapshot_returns_root_unchanged_for_a_non_dict_or_empty_child() {
        let root = Value::Dict(Dict::new());
        assert_eq!(grafted_snapshot(&root, "prime.fetch", &Value::None), root);

        let mut only_value_and_meta = Dict::new();
        only_value_and_meta.insert(Value::Str("value".to_string()), Value::from("x"));
        only_value_and_meta.insert(Value::Str("meta".to_string()), Value::Dict(Dict::new()));
        let mut child = Dict::new();
        child.insert(
            Value::Str("prime".to_string()),
            Value::Dict(only_value_and_meta),
        );
        assert_eq!(
            grafted_snapshot(&root, "prime.fetch", &Value::Dict(child)),
            root,
            "value/meta alone leave nothing to mirror"
        );
    }

    #[test]
    fn grafted_snapshot_composes_at_a_nested_path() {
        // A nested `use` (`prime.outer.inner_use`) grafts correctly
        // onto its own already-grafted parent -- `apply_grafts`'
        // shallowest-first application, exercised here directly
        // against `grafted_snapshot` rather than through the mirror.
        let root = Value::Dict(Dict::new());
        let mut grandchild_prime = Dict::new();
        grandchild_prime.insert(Value::Str("deep".to_string()), Value::from("found"));
        let mut grandchild = Dict::new();
        grandchild.insert(
            Value::Str("prime".to_string()),
            Value::Dict(grandchild_prime),
        );

        let grafted = grafted_snapshot(&root, "prime.outer.inner_use", &Value::Dict(grandchild));
        let get = |v: &Value, key: &str| -> Value {
            v.as_dict()
                .and_then(|d| d.get(&Value::Str(key.to_string())))
                .cloned()
                .unwrap_or(Value::None)
        };
        let deep = get(
            &get(&get(&get(&grafted, "prime"), "outer"), "inner_use"),
            "deep",
        );
        assert_eq!(deep, Value::from("found"));
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
        let store = Store::new();
        mirror.flush_if_due(&store, || panic!("snapshot taken with nothing pending"));
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
        let store = Store::new();
        mirror.flush_if_due(&store, || Value::Dict(dict));
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
        let store = Store::new();
        mirror.flush_if_due(&store, || Value::Dict(dict));
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
        let store = Store::new();
        mirror.flush_if_due(&store, || Value::Dict(first));
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"x\": 1}\n");
        // Nothing pending right after a flush -- a poll landing before
        // the next change is marked must be a no-op, proving the flag
        // (not just the timer) gates every write.
        mirror.flush_if_due(&store, || panic!("snapshot taken with nothing pending"));
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

    fn str_key(s: &str) -> Value {
        Value::Str(s.to_string())
    }

    /// Issue #465's own acceptance list, first bullet: a two-branch
    /// `flow: tree` dynamic, mid-run -- `fast` has already finished,
    /// `slow` hasn't written anything yet. The published shape matches
    /// cof's own (established against a real `cof run --live-state`
    /// mid-run snapshot, issue #465's own PR body): the container's
    /// own skeleton `value`/`meta` stay first, `fast`'s own finished
    /// node is appended after them, and `slow` doesn't appear at all
    /// yet -- the same as cof's `latest[index] is None` branches being
    /// skipped by its own publisher.
    #[test]
    fn a_two_branch_tree_overlays_only_the_branch_that_has_written_so_far() {
        let path = temp_path("two-branch-tree");
        let mirror = LiveStateMirror::with_interval(path.clone(), Duration::from_millis(1));
        let store = Store::new();

        let prime = store.ensure_dict(&store.root, str_key("prime")).unwrap();
        let lp = store.ensure_dict(&prime, str_key("lp")).unwrap();
        store.set_leaf(&lp, str_key("value"), Value::None);
        store.set_leaf(&lp, str_key("meta"), Value::Dict(Dict::new()));

        mirror.write_initial(&store.saved(&store.root)).unwrap();
        std::thread::sleep(Duration::from_millis(5));

        let branches = store.parallel_branches(&lp, 2).unwrap();
        let node_path = EffectPath::root().push_name("lp");
        mirror.register_tree_branches(&node_path, &branches);

        // Branch 0 ("fast") finishes; branch 1 ("slow") never writes
        // anything during this flush.
        store.set_leaf(&branches[0], str_key("fast"), Value::from("fast done"));
        mirror.mark_pending();
        mirror.flush_if_due(&store, || store.saved(&store.root));

        let mut expected_lp = Dict::new();
        expected_lp.insert(str_key("value"), Value::None);
        expected_lp.insert(str_key("meta"), Value::Dict(Dict::new()));
        expected_lp.insert(str_key("fast"), Value::from("fast done"));
        let mut expected_prime = Dict::new();
        expected_prime.insert(str_key("lp"), Value::Dict(expected_lp));
        let mut expected_root = Dict::new();
        expected_root.insert(str_key("prime"), Value::Dict(expected_prime));

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            render_state(&Value::Dict(expected_root), false)
        );
        fs::remove_file(&path).unwrap();
    }

    /// Second acceptance bullet: a tree *loop*'s `iter_<N>` pass
    /// containers, mid-run -- `parallel_branches`' own branch roots are
    /// each wrapped in their own `iter_<idx>` child, exactly as
    /// `exec::loop_::run_each_tree` builds a named loop's own
    /// `pass_write_node`.
    #[test]
    fn a_tree_loops_iter_n_pass_containers_overlay_mid_run() {
        let path = temp_path("tree-loop");
        let mirror = LiveStateMirror::with_interval(path.clone(), Duration::from_millis(1));
        let store = Store::new();

        let prime = store.ensure_dict(&store.root, str_key("prime")).unwrap();
        let draft = store.ensure_dict(&prime, str_key("draft")).unwrap();
        store.set_leaf(&draft, str_key("meta"), Value::Dict(Dict::new()));

        mirror.write_initial(&store.saved(&store.root)).unwrap();
        std::thread::sleep(Duration::from_millis(5));

        let branches = store.parallel_branches(&draft, 2).unwrap();
        let node_path = EffectPath::root().push_name("draft");
        mirror.register_tree_branches(&node_path, &branches);

        let iter_0 = store.ensure_dict(&branches[0], str_key("iter_0")).unwrap();
        store.set_leaf(&iter_0, str_key("step"), Value::from("pass 0 done"));
        mirror.mark_pending();
        mirror.flush_if_due(&store, || store.saved(&store.root));

        let mut expected_iter_0 = Dict::new();
        expected_iter_0.insert(str_key("step"), Value::from("pass 0 done"));
        let mut expected_draft = Dict::new();
        expected_draft.insert(str_key("meta"), Value::Dict(Dict::new()));
        expected_draft.insert(str_key("iter_0"), Value::Dict(expected_iter_0));
        let mut expected_prime = Dict::new();
        expected_prime.insert(str_key("draft"), Value::Dict(expected_draft));
        let mut expected_root = Dict::new();
        expected_root.insert(str_key("prime"), Value::Dict(expected_prime));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            render_state(&Value::Dict(expected_root), false)
        );
        fs::remove_file(&path).unwrap();
    }

    /// Third acceptance bullet: dropped at merge -- once
    /// `clear_tree_branches` runs (`Store::merge`'s own caller,
    /// `exec::dynamic::execute_tree`/`exec::loop_::run_each_tree`), a
    /// later flush no longer overlays anything at that path, the same
    /// as [`clear_graft_removes_a_path_so_a_later_flush_no_longer_overlays_it`].
    #[test]
    fn clear_tree_branches_removes_a_path_so_a_later_flush_no_longer_overlays_it() {
        let path = temp_path("clear-tree-branches");
        let mirror = LiveStateMirror::with_interval(path.clone(), Duration::from_millis(1));
        let store = Store::new();

        let prime = store.ensure_dict(&store.root, str_key("prime")).unwrap();
        let lp = store.ensure_dict(&prime, str_key("lp")).unwrap();
        mirror.write_initial(&store.saved(&store.root)).unwrap();
        std::thread::sleep(Duration::from_millis(5));

        let branches = store.parallel_branches(&lp, 1).unwrap();
        let node_path = EffectPath::root().push_name("lp");
        mirror.register_tree_branches(&node_path, &branches);
        store.set_leaf(&branches[0], str_key("fast"), Value::from("leftover"));
        mirror.clear_tree_branches(&node_path);

        mirror.mark_pending();
        mirror.flush_if_due(&store, || store.saved(&store.root));

        let mut expected_root = Dict::new();
        expected_root.insert(
            str_key("prime"),
            Value::Dict({
                let mut prime = Dict::new();
                prime.insert(str_key("lp"), Value::Dict(Dict::new()));
                prime
            }),
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            render_state(&Value::Dict(expected_root), false)
        );
        fs::remove_file(&path).unwrap();
    }

    /// Fourth acceptance bullet: nested trees compose -- an outer
    /// tree's own overlay at `prime.outer` and an inner tree nested in
    /// one of its own branches, registered at `prime.outer.inner`, both
    /// apply, shallowest path first, the same way a nested `use`'s own
    /// graft already composes with its parent's.
    #[test]
    fn a_nested_tree_composes_with_its_own_parents_overlay() {
        let path = temp_path("nested-tree");
        let mirror = LiveStateMirror::with_interval(path.clone(), Duration::from_millis(1));
        let store = Store::new();

        let prime = store.ensure_dict(&store.root, str_key("prime")).unwrap();
        let outer = store.ensure_dict(&prime, str_key("outer")).unwrap();
        mirror.write_initial(&store.saved(&store.root)).unwrap();
        std::thread::sleep(Duration::from_millis(5));

        let outer_branches = store.parallel_branches(&outer, 1).unwrap();
        mirror.register_tree_branches(&EffectPath::root().push_name("outer"), &outer_branches);

        // The outer tree's one branch starts its own, nested tree,
        // writing its own skeleton directly into the branch's own root.
        let inner_branches = store.parallel_branches(&outer_branches[0], 1).unwrap();
        mirror.register_tree_branches(
            &EffectPath::root().push_name("outer").push_name("inner"),
            &inner_branches,
        );
        store.set_leaf(&inner_branches[0], str_key("leaf"), Value::from("deep"));

        mirror.mark_pending();
        mirror.flush_if_due(&store, || store.saved(&store.root));

        let get = |v: &Value, key: &str| -> Value {
            v.as_dict()
                .and_then(|d| d.get(&str_key(key)))
                .cloned()
                .unwrap_or(Value::None)
        };
        let text = fs::read_to_string(&path).unwrap();
        let parsed = electricity_json::loads(&text).unwrap();
        let leaf = get(&get(&get(&get(&parsed, "prime"), "outer"), "inner"), "leaf");
        assert_eq!(leaf, Value::from("deep"), "got {text}");
        fs::remove_file(&path).unwrap();
    }
}
