//! Lane A: the per-effect-type option structs, field for field from
//! Circuitry's `PromptDefinition`, `ToolDefinition`, `UseDefinition`,
//! `YieldDefinition` and `ReflectorDefinition` (issue #408's Scope
//! section). Each struct's doc comment maps every field back to the
//! Python dataclass field it carries; `tests/definition_fields.rs`
//! checks this mapping stays exhaustive as Circuitry's dataclasses grow.
//!
//! Fields shared by every effect type live on [`crate::op::Op`] instead
//! of being repeated here: `name` (the Python dataclass's own `name`),
//! `on_error`, and `enabled`. `group` is per-type (only `PromptDefinition`
//! and `ToolDefinition` have it), so it stays on [`PromptOp`]/[`ToolOp`].

use crate::param::ParamNode;
use crate::region::{ExpectCondition, Region};
use crate::template::TemplateText;
use electricity_value::Value;
use indexmap::IndexMap;
use serde::Serialize;

/// `RetryPolicyDef`. Defaults: `max_attempts` 1, `backoff_ms` 1000.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff_ms: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_attempts: crate::defaults::RETRY_MAX_ATTEMPTS,
            backoff_ms: crate::defaults::RETRY_BACKOFF_MS,
        }
    }
}

/// `AssetRefDef`. `reference` carries Python's `ref` field — renamed
/// since `ref` is a Rust keyword.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssetRef {
    pub kind: String,
    pub reference: String,
}

/// `MessageDef.role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// `MessageDef`.
#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub role: Role,
    pub content: TemplateText,
}

/// `PromptDefinition.prompt_type`. `"image"` is refused at compile time
/// (`core/compiler.py`) and so has no variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PromptType {
    Text,
    Json,
    Boolean,
    Tool,
    Number,
    Array,
    Object,
}

/// `PromptDefinition.routing_override` (`bool | str | None`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum RoutingOverride {
    Bool(bool),
    Named(String),
}

/// `PromptDefinition.template`/`.messages` — exactly one of the two is
/// present, mirroring the Python dataclass's own either/or fields rather
/// than inventing a third "neither" state the compiler already rejects.
#[derive(Debug, Clone, Serialize)]
pub enum PromptContent {
    Template(TemplateText),
    Messages(Vec<Message>),
}

/// `PromptDefinition`, minus `name`/`on_error`/`enabled` (on
/// [`crate::op::Op`]).
#[derive(Debug, Clone, Serialize)]
pub struct PromptOp {
    /// `template`/`messages`.
    pub content: PromptContent,
    /// `prompt_type`. Default `Text` (Python default `"text"`).
    pub prompt_type: PromptType,
    pub schema: Option<Value>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub provider_fallbacks: Vec<String>,
    pub routing_override: Option<RoutingOverride>,
    /// `params` — the adapter's own model parameters, not a tool's
    /// `params:` (that's [`ToolOp::params`]).
    pub model_params: Option<ParamNode>,
    pub timeout_ms: Option<u64>,
    /// Default `false`.
    pub deterministic: bool,
    pub inputs: Option<ParamNode>,
    pub assets: Vec<AssetRef>,
    /// Default attempts 1 / backoff 1000ms.
    pub retries: RetryPolicy,
    pub description: Option<String>,
    pub group: Option<String>,
}

/// `ToolDefinition`, minus `name`/`on_error`/`enabled`.
#[derive(Debug, Clone, Serialize)]
pub struct ToolOp {
    pub provider: String,
    /// `params` — already past the security-sensitive-key literal check
    /// (DESIGN.md §5.2) and `allowed_commands` must-be-literal check.
    pub params: ParamNode,
    /// `params_json`, tagged for `JsonAwareCtx` rendering (DESIGN.md
    /// §5.2) at run time.
    pub params_json: Option<TemplateText>,
    pub prompt: Option<TemplateText>,
    pub model: Option<String>,
    pub timeout_ms: Option<u64>,
    pub retries: RetryPolicy,
    pub expect: Option<ExpectCondition>,
    pub description: Option<String>,
    pub group: Option<String>,
}

/// `UseDefinition.path`/`.orchestration`/`.inline` — exactly one present
/// (checked at compile time). `UseDefinition.ref` has no variant here:
/// electricity rejects `ref:` at compile time (DESIGN.md §4), since
/// there is no library-name/remote-library lookup to resolve it against
/// — a documented divergence, tracked (not type-carried) in
/// `tests/definition_fields.rs`'s field mapping.
#[derive(Debug, Clone, Serialize)]
pub enum UseSource {
    Path(String),
    Orchestration(String),
    Inline(TemplateText),
}

/// `UseDefinition`, minus `name`/`on_error`/`enabled`.
#[derive(Debug, Clone, Serialize)]
pub struct UseOp {
    pub source: UseSource,
    pub inputs: Option<ParamNode>,
    /// `outputs`, already normalized (DESIGN.md §4 step 3) to a name ->
    /// source-path mapping.
    pub outputs: Option<IndexMap<String, String>>,
    /// Default `true`.
    pub validate: bool,
    pub retries: RetryPolicy,
    pub expect: Option<ExpectCondition>,
    pub description: Option<String>,
}

/// `YieldDefinition`, minus `name`/`on_error`/`enabled`.
#[derive(Debug, Clone, Serialize)]
pub struct YieldOp {
    /// `template`, non-empty (checked at compile time). `Escape::None`
    /// (prompt-shaped text, #397).
    pub template: TemplateText,
    pub inputs: Option<ParamNode>,
    pub description: Option<String>,
}

/// `ReflectorDefinition`, minus `name`/`enabled` (`ReflectorDefinition`
/// has no `on_error` field of its own).
#[derive(Debug, Clone, Serialize)]
pub struct ReflectorOp {
    /// `inner` — the reflector's own `effects:`, compiled as an ordinary
    /// nested region (DESIGN.md §5.2), not folded into `LeafKind`.
    pub inner: Box<Region>,
    /// Default `"propose_steps"`.
    pub plan_from_step: String,
    /// Default `1`.
    pub max_iterations: u32,
    /// Default `"generated"`.
    pub generated_key: String,
    /// Default `true`.
    pub stop_on_done: bool,
    /// Defaults to Circuitry's reflector prime, copied byte for byte by
    /// `electricity/scripts/generate_reflector_prime.py --check` into
    /// `electricity-compiler`'s `src/reflector_prime.txt`.
    pub prime_template: TemplateText,
    /// Default `8`.
    pub max_effects: u32,
}
