//! electricity-bytecode: the compiled IR for an electricity orchestration
//! (issue #408's Scope section; DESIGN.md §5).
//!
//! Bytecode is an internal implementation step (DESIGN.md §5): there is
//! no user-facing bytecode format, no persisted `.ebc` file, and no
//! stability promise on the shapes in this crate beyond "lane B, C and D
//! build on them without changing them out from under each other" — the
//! one *external* surface that touches this IR at all, `--dump-ir`, is
//! explicitly unstable (issue #408's CLI section).
//!
//! # Module map
//!
//! - [`path`] — `EffectPath`/`PathSegment`/`LoopId`, the stable path a
//!   compiled [`op::Op`] carries.
//! - [`op`] — `Op`, `NodeKind`, `LeafKind`, `OnError`.
//! - [`region`] — `Region`, `Condition`, `ExpectCondition`, `LoopSpec`,
//!   `LoopFlow`: control-flow containers.
//! - [`template`] — `TemplateText`/`Escape`.
//! - [`param`] — `ParamNode`, a tool's `params`/`use`'s `inputs`.
//! - [`effects`] — the per-effect-type option structs
//!   (`PromptOp`/`ToolOp`/`UseOp`/`YieldOp`/`ReflectorOp`), field for
//!   field from Circuitry's own `*Definition` dataclasses.
//! - [`program`] — `Program`, `DocumentInfo`: the compiled whole.
//! - [`defaults`] — the default values Circuitry applies, named here so
//!   every later lane applies the same one.
//!
//! # Which Python field lives where
//!
//! Every field of `PromptDefinition`, `ToolDefinition`, `UseDefinition`,
//! `YieldDefinition`, `ReflectorDefinition`, `DynamicDefinition`,
//! `ConditionalDefinition` and `LoopDefinition` (and their nested
//! `MessageDef`/`AssetRefDef`/`RetryPolicyDef`/`ExpectDef`/
//! `ConditionDef`/`LoopWhileDef`/`LoopEachDef`) has a documented IR
//! counterpart on the corresponding `*Op`/`Region`/`Op` field — see each
//! struct's own doc comment in [`effects`], [`op`] and [`region`].
//! `tests/definition_fields.rs` checks this mapping against a golden
//! list of the real dataclasses' fields
//! (`electricity/scripts/generate_compiler_definition_fields.py
//! --check`), so a new Python field fails CI here until the IR carries
//! it.
//!
//! # Known divergences
//!
//! - `UseDefinition.ref` has no IR counterpart: electricity rejects
//!   `ref:` at compile time (no library-name/remote-library
//!   resolution), so a compiled `use` is always `path`/`orchestration`/
//!   `inline` (`effects::UseSource`).
//! - `PromptDefinition.prompt_type: "image"` has no [`effects::PromptType`]
//!   variant: Circuitry's own compiler refuses it outright
//!   (`core/compiler.py`), so no compiled `Program` can carry it.

pub mod defaults;
pub mod effects;
pub mod op;
pub mod param;
pub mod path;
pub mod program;
pub mod region;
pub mod template;

pub use effects::{
    AssetRef, Message, PromptContent, PromptOp, PromptType, ReflectorOp, RetryPolicy, Role,
    RoutingOverride, ToolOp, UseOp, UseSource, YieldOp,
};
pub use op::{LeafKind, NodeKind, OnError, Op};
pub use param::ParamNode;
pub use path::{EffectPath, LoopId, PathSegment};
pub use program::{DocumentInfo, Program};
pub use region::{Condition, ExpectCondition, LoopFlow, LoopSpec, Region};
pub use template::{Escape, TemplateText};
