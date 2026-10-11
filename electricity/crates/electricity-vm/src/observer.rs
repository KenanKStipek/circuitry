//! `RunObserver`: the VM's own effect start/complete, dispatch, write,
//! and warning hooks -- what `--events`/`--live-state` (lane D) and a
//! run-time plugin's `on_effect_start`/`on_effect_complete` (out of
//! scope for M0-H) both hang off of.

use electricity_bytecode::EffectPath;
use electricity_value::Value;

/// An opaque per-call identifier [`RunObserver::effect_start`] hands
/// back and [`RunObserver::effect_complete`] takes (issue #449's gate
/// lane item 7) -- `--events` pairs a `start` line with its `end` line;
/// Python gets that pairing for free from a per-thread call stack
/// (`cli/events.py`), but this VM runs concurrent tree branches (and,
/// from M1-H, concurrent/unnamed loop passes writing the same path) on
/// one thread, so two overlapping calls at the *same* [`EffectPath`]
/// can't be told apart by thread alone. The instance id is this crate's
/// own replacement: whichever call's own `effect_start` produced it is
/// the one `effect_complete` with that id belongs to, no matter how
/// many *other* calls at the same path started or finished in between.
///
/// `0` for [`NullObserver`] and every observer that doesn't need to
/// disambiguate calls (nothing in this crate reads an instance id back
/// out of a hook return value, only passes it through to the matching
/// `effect_complete`) -- a caller that does care (a future `--events`
/// writer, once loop passes can share a path) assigns a fresh,
/// non-zero id per call instead.
pub type InstanceId = u64;

/// One run's worth of execution callbacks. Every method has a default
/// no-op body, so a caller that only cares about one hook (a test, say)
/// implements just that one.
pub trait RunObserver {
    /// A named effect is about to run (DESIGN.md's own "balanced start/
    /// complete hooks on every exit path; no hooks for an unnamed `if`"
    /// rule, issue #431's Lane B section) -- returns this call's own
    /// [`InstanceId`], which the matching [`RunObserver::effect_complete`]
    /// call must be given back.
    fn effect_start(&self, _path: &EffectPath) -> InstanceId {
        0
    }

    /// A named effect finished -- *instance* is the id
    /// [`RunObserver::effect_start`] returned for *this same call*;
    /// *error* is the text that would become `meta.error`, `None` on
    /// success.
    fn effect_complete(&self, _path: &EffectPath, _instance: InstanceId, _error: Option<&str>) {}

    /// A tree `dynamic`/`each` loop is about to dispatch -- `--events`'s
    /// own `dispatch` event (`branches`: `n` itself, `concurrency`:
    /// `min(max_workers, n)`; issue #431 review finding on PR #440 --
    /// this pairing was backwards).
    fn dispatch(&self, _path: &EffectPath, _branches: usize, _concurrency: usize) {}

    /// The store changed in a way a live-state mirror should eventually
    /// reflect -- coalesced by the caller (lane D's own `LiveStateMirror`,
    /// not this trait), never a promise of one write per call.
    fn write(&self) {}

    /// A run-time warning with no effect of its own to attach it to --
    /// Circuitry's own stderr `WARNING: ...` lines (issue #442). Adding
    /// the hook itself is this gate lane's job (issue #449 item 7); the
    /// CLI's own stderr writer that turns a call here into a printed
    /// line is #442's own PR, built on top of this seam once it lands.
    fn warning(&self, _message: &str) {}

    /// An isolated child run's own write (`use`, lane I; decomposition,
    /// lane L2) -- `core/use.py::_child_on_write`/`_grafted_snapshot`'s
    /// own seam: on every child write, the run's own live-state reader
    /// republishes the whole-run snapshot with *snapshot* (the child's
    /// own current, `prime`-rooted state -- never the parent's)
    /// mirrored under *node_path* (the `use`/decomposition effect's own
    /// absolute path in the parent document, `Frame::effect_path`'s own
    /// output), composing to any depth because *snapshot* always
    /// arrives as this call's own argument rather than being read back
    /// off any store -- a nested child's own graft call lands at its
    /// own, deeper *node_path* independently of this one.
    ///
    /// Default no-op, like every other hook here; only the run wiring's
    /// own live-state mirror (`electricity` (lib), `src/live_state.rs`)
    /// implements it for real. [`RunObserver::clear_graft`] removes a
    /// path this call grafted once the child's own real output merges
    /// into the parent for good -- a later snapshot no longer needs
    /// (or should show) the overlay at all.
    fn graft_child_write(&self, _node_path: &EffectPath, _snapshot: &Value) {}

    /// *node_path* no longer needs [`RunObserver::graft_child_write`]'s
    /// own overlay -- the child it named has finished (merged for real,
    /// or failed) and a later snapshot reads its own now-final state
    /// directly. A no-op for a path that was never grafted (or already
    /// cleared), same as every other hook's own tolerant default here.
    fn clear_graft(&self, _node_path: &EffectPath) {}

    /// Whether this observer's own [`RunObserver::graft_child_write`]
    /// is ever actually consumed -- `false` by default, so a caller
    /// that would otherwise take a [`crate::Store::saved`] snapshot
    /// (an `O(size of the tree so far)` recursive clone) purely to hand
    /// it to a `graft_child_write` nobody reads never has to pay for
    /// that snapshot at all (`exec::use_`'s own `ChildObserver::write`,
    /// lane I's review finding on PR #472: a long loop inside a `use`
    /// child used to pay one such snapshot per child write regardless
    /// of `--live-state`). `electricity` (lib)'s own `Observer` is the
    /// only implementation that ever returns `true`, and only when a
    /// `--live-state` mirror is actually running.
    fn wants_child_snapshots(&self) -> bool {
        false
    }
}

/// A [`RunObserver`] that does nothing -- every lane A test (and any
/// caller that doesn't need `--events`/`--live-state`) uses this instead
/// of writing its own no-op type.
pub struct NullObserver;

impl RunObserver for NullObserver {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_observer_accepts_every_hook_without_panicking() {
        let observer = NullObserver;
        let path = EffectPath::root();
        let instance = observer.effect_start(&path);
        assert_eq!(instance, 0);
        observer.effect_complete(&path, instance, Some("boom"));
        observer.dispatch(&path, 3, 2);
        observer.write();
        observer.warning("a run-time warning with no effect of its own");
        observer.graft_child_write(&path, &Value::None);
        observer.clear_graft(&path);
    }
}
