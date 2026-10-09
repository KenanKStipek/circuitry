//! Lane A: `EffectPath`/`PathSegment`, the stable effect-path ID every
//! `Op` carries (DESIGN.md §5.1, issue #408's Scope section).
//!
//! For a non-loop effect the path is just the chain of names from the
//! document root (`prime.handle`). An effect nested inside a *named*
//! loop body keeps that loop's pass as a compile-time placeholder
//! (`PathSegment::Pass`) instead of a concrete index: the body compiles
//! once, not once per pass, and the VM (M0-H) concretizes the
//! placeholder against the running pass index only when it dispatches.
//! An *unnamed* `if`/`loop` never contributes a segment at all — it is
//! transparent, writing into the enclosing scope (issue #408).

use serde::Serialize;
use std::collections::HashMap;
use std::fmt;

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
/// belongs to — a compile-time ordinal (assigned in document order by
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

    /// Resolves every [`PathSegment::Pass`] placeholder against
    /// *pass_indices* -- a lookup from the placeholder's [`LoopId`] to
    /// its loop's *current*, zero-based pass index -- producing the
    /// plain, dot-joined state path the VM (M0-H) and `--events`'s own
    /// `path` field use (`prime.shots.iter_2.describe`, runtime-
    /// semantics.md's own `iter_<N>` convention: §2.3, §8.7).
    ///
    /// *pass_indices* must carry an entry for every [`LoopId`] this path
    /// actually reaches -- a path the VM is executing always knows every
    /// enclosing named loop's current pass, so a missing entry is a
    /// caller bug, not a data problem: this returns `Err` naming the
    /// first such [`LoopId`] rather than silently emitting a path with a
    /// placeholder still in it or panicking.
    pub fn concretize(&self, pass_indices: &HashMap<LoopId, u32>) -> Result<String, LoopId> {
        let mut parts = Vec::with_capacity(self.0.len());
        for segment in &self.0 {
            match segment {
                PathSegment::Name(name) => parts.push(name.clone()),
                PathSegment::Pass(loop_id) => {
                    let index = pass_indices.get(loop_id).ok_or(*loop_id)?;
                    parts.push(format!("iter_{index}"));
                }
            }
        }
        Ok(parts.join("."))
    }
}

impl fmt::Display for PathSegment {
    /// A [`PathSegment::Pass`] has no run-time pass index to show here --
    /// [`EffectPath::concretize`] is the only thing that ever turns one
    /// into a real state-path segment. This `Display` only ever
    /// reproduces the compile-time placeholder shape, for debugging
    /// (`--dump-ir`'s own JSON already carries the structured form; nothing
    /// in this crate parses this string back).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathSegment::Name(name) => write!(f, "{name}"),
            PathSegment::Pass(loop_id) => write!(f, "<pass of loop #{}>", loop_id.0),
        }
    }
}

impl fmt::Display for EffectPath {
    /// Dot-joined, same shape as [`EffectPath::concretize`]'s output --
    /// but only when every segment is a concrete [`PathSegment::Name`].
    /// A path still carrying a [`PathSegment::Pass`] placeholder (this
    /// `Op`'s compiled, not-yet-dispatched form, inside a named loop
    /// body) prints [`PathSegment`]'s own placeholder spelling at that
    /// position instead -- never a silent `iter_0`/wrong index, which
    /// would misrepresent a path nothing has concretized yet. Used by
    /// the chain-flow error wrap (`"<path>: <message>"`,
    /// runtime-semantics.md's own `RuntimeError(f"{effect_path}: {e}")`)
    /// once every segment the VM actually dispatches is already a
    /// [`PathSegment::Name`] (a `Pass` is concretized to a `Name` before
    /// the VM ever builds a chain-error message from it).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, segment) in self.0.iter().enumerate() {
            if index > 0 {
                write!(f, ".")?;
            }
            write!(f, "{segment}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_joins_name_segments_with_dots() {
        let path = EffectPath::root().push_name("shots").push_name("describe");
        assert_eq!(path.to_string(), "prime.shots.describe");
    }

    #[test]
    fn concretize_resolves_a_pass_placeholder_to_iter_n() {
        let loop_id = LoopId(0);
        let path = EffectPath::root()
            .push_name("shots")
            .push_pass(loop_id)
            .push_name("describe");
        let mut passes = HashMap::new();
        passes.insert(loop_id, 2u32);
        assert_eq!(
            path.concretize(&passes).unwrap(),
            "prime.shots.iter_2.describe"
        );
    }

    #[test]
    fn concretize_fails_closed_on_a_missing_pass_index() {
        let loop_id = LoopId(0);
        let path = EffectPath::root().push_pass(loop_id);
        let err = path.concretize(&HashMap::new()).unwrap_err();
        assert_eq!(err, loop_id);
    }

    #[test]
    fn concretize_resolves_nested_loops_independently() {
        let outer = LoopId(0);
        let inner = LoopId(1);
        let path = EffectPath::root()
            .push_name("outer")
            .push_pass(outer)
            .push_name("inner")
            .push_pass(inner)
            .push_name("leaf");
        let mut passes = HashMap::new();
        passes.insert(outer, 1u32);
        passes.insert(inner, 3u32);
        assert_eq!(
            path.concretize(&passes).unwrap(),
            "prime.outer.iter_1.inner.iter_3.leaf"
        );
    }
}
