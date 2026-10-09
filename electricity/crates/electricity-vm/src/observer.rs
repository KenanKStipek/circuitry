//! `RunObserver`: the VM's own effect start/complete, dispatch, and
//! write hooks -- what `--events`/`--live-state` (lane D) and a run-time
//! plugin's `on_effect_start`/`on_effect_complete` (out of scope for
//! M0-H) both hang off of.

use electricity_bytecode::EffectPath;

/// One run's worth of execution callbacks. Every method has a default
/// no-op body, so a caller that only cares about one hook (a test, say)
/// implements just that one.
pub trait RunObserver {
    /// A named effect is about to run (DESIGN.md's own "balanced start/
    /// complete hooks on every exit path; no hooks for an unnamed `if`"
    /// rule, issue #431's Lane B section).
    fn effect_start(&self, _path: &EffectPath) {}

    /// A named effect finished -- `error` is the text that would become
    /// `meta.error`, `None` on success.
    fn effect_complete(&self, _path: &EffectPath, _error: Option<&str>) {}

    /// A tree `dynamic`/`each` loop is about to dispatch -- `--events`'s
    /// own `dispatch` event (`branches`: `n` itself, `concurrency`:
    /// `min(max_workers, n)`; issue #431 review finding on PR #440 --
    /// this pairing was backwards).
    fn dispatch(&self, _path: &EffectPath, _branches: usize, _concurrency: usize) {}

    /// The store changed in a way a live-state mirror should eventually
    /// reflect -- coalesced by the caller (lane D's own `LiveStateMirror`,
    /// not this trait), never a promise of one write per call.
    fn write(&self) {}
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
        observer.effect_start(&path);
        observer.effect_complete(&path, Some("boom"));
        observer.dispatch(&path, 3, 2);
        observer.write();
    }
}
