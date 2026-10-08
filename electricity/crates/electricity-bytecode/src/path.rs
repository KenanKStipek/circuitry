//! Lane A: `EffectPath`/`PathSegment`, the stable effect-path ID every
//! `Op` carries (DESIGN.md §5.1, issue #408's Scope section).
//!
//! For a non-loop effect the path is just the chain of names from the
//! document root (`prime.handle`). An effect nested inside a *named*
//! loop body keeps that loop's pass as a compile-time placeholder
//! (`PathSegment::Pass`) instead of a concrete index: the body compiles
//! once, not once per pass, and the VM (lane B/M0-H) concretizes the
//! placeholder against the running pass index only when it dispatches.
//! An *unnamed* `if`/`loop` never contributes a segment at all \u2014 it is
//! transparent, writing into the enclosing scope (issue #408).

use serde::Serialize;

/// One segment of an [`EffectPath`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PathSegment {
    /// A named effect or container.
    Name(String),
    /// A pass placeholder under the named loop identified by [`LoopId`].
    /// Only ever appears under a *named* loop (an unnamed loop is
    /// transparent and contributes no segment).
    Pass(LoopId),
}

/// Identifies which compiled loop a [`PathSegment::Pass`] placeholder
/// belongs to \u2014 a compile-time ordinal (assigned in document order by
/// the compiler, lane C), not a run-time pass index. The VM concretizes
/// a placeholder by substituting the loop's *current* pass index for the
/// `LoopId` it names, never the other way around.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct LoopId(pub u32);

/// A stable effect-path ID: the chain of [`PathSegment`]s from the
/// document root to one compiled `Op`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectPath(pub Vec<PathSegment>);

impl EffectPath {
    pub fn root() -> Self {
        EffectPath(vec![PathSegment::Name("prime".to_string())])
    }

    pub fn push_name(&self, name: impl Into<String>) -> Self {
        let mut segments = self.0.clone();
        segments.push(PathSegment::Name(name.into()));
        EffectPath(segments)
    }

    pub fn push_pass(&self, loop_id: LoopId) -> Self {
        let mut segments = self.0.clone();
        segments.push(PathSegment::Pass(loop_id));
        EffectPath(segments)
    }
}
