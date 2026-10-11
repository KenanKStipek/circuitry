//! `RunObserver`: the VM's own effect start/complete, dispatch, write,
//! and warning hooks -- what `--events`/`--live-state` (lane D) and a
//! run-time plugin's `on_effect_start`/`on_effect_complete` (out of
//! scope for M0-H) both hang off of.

use electricity_bytecode::EffectPath;

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
    }
}
