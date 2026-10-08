//! Lane A: `Op`, `NodeKind`, `LeafKind`, `OnError` (issue #408's Scope
//! section; DESIGN.md §5.1).

use crate::effects::{PromptOp, ReflectorOp, ToolOp, UseOp, YieldOp};
use crate::path::EffectPath;
use crate::region::Region;
use electricity_value::Value;
use serde::Serialize;

/// `on_error:`, normalized the way Python's coercion does (an invalid
/// value becomes `"fail"`, `core/compiler.py`). `Break` is only ever
/// produced for a `loop`'s own `on_error:` (`LoopDefinition.on_error`
/// allows `"break"` where every other effect type's `on_error` doesn't);
/// one shared enum carries every effect type's `on_error` field rather
/// than a separate type per type, since every other variant is common
/// to all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum OnError {
    Fail,
    Skip,
    Continue,
    Break,
}

/// A leaf effect's own payload (DESIGN.md §5.1: "prompt/tool/use/
/// reflector never contain a nested `Region` of their own" — a
/// reflector's `inner` dynamic is a *sibling field* on [`ReflectorOp`],
/// not folded into this enum).
#[derive(Debug, Clone, Serialize)]
pub enum LeafKind {
    Prompt(PromptOp),
    Tool(ToolOp),
    Use(UseOp),
    Yield(YieldOp),
    Reflector(ReflectorOp),
}

/// A compiled node is *either* a leaf effect or a control-flow region
/// that owns its own path (DESIGN.md §5.1) — `loop`/`dynamic`/`if`
/// compile directly to `Control`, never to a `LeafKind` variant that
/// then separately wraps a `Region`.
#[derive(Debug, Clone, Serialize)]
pub enum NodeKind {
    // Boxed: `LeafKind`'s largest variant (`PromptOp`) is far bigger than
    // `Region`'s, and this enum is `Op::kind`, one field of a type cloned
    // and nested throughout a compiled `Program` (clippy::large_enum_variant).
    Leaf(Box<LeafKind>),
    Control(Region),
}

/// One compiled effect or control-flow container.
#[derive(Debug, Clone, Serialize)]
pub struct Op {
    /// Stable effect-path ID (placeholder form inside a named loop body
    /// — see [`crate::path`]).
    pub path: EffectPath,
    /// `None` for an unnamed loop/conditional (transparent control,
    /// `ConditionalDefinition.name`/`LoopDefinition.name` are both
    /// `str | None`) — it writes into the enclosing scope at run time
    /// rather than a child node.
    pub name: Option<String>,
    pub kind: NodeKind,
    pub on_error: OnError,
    /// `labels:` (`DynamicDefinition.labels`/`ConditionalDefinition.labels`/
    /// `LoopDefinition.labels`) — kept as a raw [`Value`] since it's
    /// opaque metadata, never interpreted by the compiler itself.
    pub labels: Option<Value>,
    pub enabled: bool,
}
