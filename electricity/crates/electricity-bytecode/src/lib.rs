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
//! - [`refusal`] — [`first_unsupported`]: the first effect path a
//!   compiled `Program` has that the M0-H VM (issue #431) cannot run.
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
//! - `DynamicDefinition.max_concurrency`/`.stop_on_error` have no
//!   counterpart on [`region::Region::Block`] (a chain-flow `dynamic`):
//!   Circuitry's own compiler carries both unconditionally but documents
//!   them as "only meaningful when flow=\"tree\"" (`core/compiler.py`'s
//!   own comment), so [`region::Region::Parallel`] (tree flow) is the
//!   only variant that keeps them. A test projecting the IR back into
//!   Python's dumped `DynamicDefinition` JSON must ignore both fields
//!   for a chain-flow `dynamic`, not expect them reproduced.
//! - [`effects::PromptContent`] is either/or (`Template`/`Messages`),
//!   matching `PromptDefinition.template`/`.messages`' own either/or
//!   *shape* — but Python's two fields aren't mutually exclusive by
//!   construction, only by convention: a document that sets both
//!   compiles, and `core/prompt.py`'s `_materialize_input` prefers
//!   `template` whenever it's truthy, so `messages` is already dead at
//!   run time in that case. The compiler (lane C) reproduces that by
//!   choosing `Template` and discarding `messages` when a document sets
//!   both — a deliberate, behavior-preserving drop, not an oversight.
//! - Every `u32`/`u64` field here (loop/concurrency limits) narrows
//!   Python's unbounded `int` (checked only against the JSON Schema's
//!   own minimum, if any) to a fixed width. A document whose limit
//!   exceeds the IR's width is out of scope for M0: no known Circuitry
//!   orchestration sets one anywhere near `u32::MAX`.

pub mod defaults;
pub mod effects;
pub mod op;
pub mod param;
pub mod path;
pub mod program;
pub mod refusal;
pub mod region;
pub mod template;

pub use effects::{
    AssetRef, Message, PromptContent, PromptOp, PromptType, ReflectorOp, RetryPolicy, Role,
    RoutingOverride, ToolOp, UseOp, UseSource, YieldOp,
};
pub use op::{LeafKind, NodeKind, OnError, Op, python_type_name};
pub use param::ParamNode;
pub use path::{EffectPath, LoopId, PathSegment};
pub use program::{DocumentInfo, Program};
pub use refusal::{Refusal, RefusalReason, Supported, first_unsupported};
pub use region::{Condition, ExpectCondition, LoopFlow, LoopSpec, Region};
pub use template::{Escape, TemplateText};
