//! Lane A: `Op`, `NodeKind`, `LeafKind`, `OnError` (issue #408's Scope
//! section; DESIGN.md §5.1).

use crate::effects::{PromptOp, ReflectorOp, ToolOp, UseOp, YieldOp};
use crate::path::EffectPath;
use crate::region::Region;
use electricity_value::Value;
use serde::Serialize;

/// The Python class name `core/conditional.py::ConditionalDefinition::
/// _effect_record` reports for this node's `type(effect).__name__` --
/// an `if` node's `value.effects[].type` (issue #431's Scope section;
/// confirmed directly against every `*Definition` dataclass's own class
/// name: `core/{prompt,tool,use,yield_effect,reflector,dynamic,
/// conditional,loop}.py`). A control node's variant is read off its
/// [`Region`] shape, not the (lane-D-only) "was this document's `if`/
/// `loop`/`dynamic` keyword" distinction `compile_document` has already
/// erased by the time an `Op` exists: [`Region::Block`]/[`Region::
/// Parallel`] (a `dynamic`'s chain/tree flow) and [`Region::TryFinally`]
/// (always a `dynamic`'s own `finally:` wrapper -- only a `dynamic`/the
/// document root ever has one) are all `"DynamicDefinition"`;
/// [`Region::If`] is `"ConditionalDefinition"`; [`Region::Loop`] is
/// `"LoopDefinition"`.
pub fn python_type_name(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Leaf(leaf) => match leaf.as_ref() {
            LeafKind::Prompt(_) => "PromptDefinition",
            LeafKind::Tool(_) => "ToolDefinition",
            LeafKind::Use(_) => "UseDefinition",
            LeafKind::Yield(_) => "YieldDefinition",
            LeafKind::Reflector(_) => "ReflectorDefinition",
        },
        NodeKind::Control(region) => match region {
            Region::Block { .. } | Region::Parallel { .. } | Region::TryFinally { .. } => {
                "DynamicDefinition"
            }
            Region::If { .. } => "ConditionalDefinition",
            Region::Loop { .. } => "LoopDefinition",
        },
    }
}

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

impl Op {
    /// [`python_type_name`] for this node's own [`NodeKind`].
    pub fn python_type_name(&self) -> &'static str {
        python_type_name(&self.kind)
    }
}

#[cfg(test)]
mod python_type_name_tests {
    use super::*;
    use crate::effects::{
        PromptContent, PromptOp, PromptType, ReflectorOp, RetryPolicy, ToolOp, UseOp, UseSource,
        YieldOp,
    };
    use crate::param::ParamNode;
    use crate::path::EffectPath;
    use crate::region::{Condition, LoopFlow, LoopSpec, Region};
    use crate::template::{Escape, TemplateText};
    use indexmap::IndexMap;

    fn op(kind: NodeKind) -> Op {
        Op {
            path: EffectPath::root(),
            name: None,
            kind,
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        }
    }

    #[test]
    fn every_leaf_kind_reports_its_definition_class_name() {
        assert_eq!(
            op(NodeKind::Leaf(Box::new(LeafKind::Prompt(PromptOp {
                content: PromptContent::Template(TemplateText::new("hi", false, Escape::None)),
                prompt_type: PromptType::Text,
                schema: None,
                model: None,
                provider: None,
                provider_fallbacks: Vec::new(),
                routing_override: None,
                model_params: None,
                timeout_ms: None,
                deterministic: false,
                inputs: None,
                assets: Vec::new(),
                retries: RetryPolicy::default(),
                description: None,
                group: None,
            }))))
            .python_type_name(),
            "PromptDefinition"
        );
        assert_eq!(
            op(NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
                provider: "json".to_string(),
                params: ParamNode::Map(IndexMap::new()),
                params_json: None,
                prompt: None,
                model: None,
                timeout_ms: None,
                retries: RetryPolicy::default(),
                expect: None,
                description: None,
                group: None,
            }))))
            .python_type_name(),
            "ToolDefinition"
        );
        assert_eq!(
            op(NodeKind::Leaf(Box::new(LeafKind::Use(UseOp {
                source: UseSource::Path("child.yml".to_string()),
                inputs: None,
                outputs: None,
                validate: true,
                retries: RetryPolicy::default(),
                expect: None,
                description: None,
            }))))
            .python_type_name(),
            "UseDefinition"
        );
        assert_eq!(
            op(NodeKind::Leaf(Box::new(LeafKind::Yield(YieldOp {
                template: TemplateText::new("hi", false, Escape::None),
                inputs: None,
                description: None,
            }))))
            .python_type_name(),
            "YieldDefinition"
        );
        assert_eq!(
            op(NodeKind::Leaf(Box::new(LeafKind::Reflector(ReflectorOp {
                inner: Box::new(Region::Block {
                    ops: Vec::new(),
                    overlay: false,
                }),
                plan_from_step: "propose_steps".to_string(),
                max_iterations: 1,
                generated_key: "generated".to_string(),
                stop_on_done: true,
                prime_template: TemplateText::new("", false, Escape::None),
                max_effects: 8,
            }))))
            .python_type_name(),
            "ReflectorDefinition"
        );
    }

    #[test]
    fn every_dynamic_shaped_region_including_try_finally_is_dynamic_definition() {
        let block = Region::Block {
            ops: Vec::new(),
            overlay: false,
        };
        assert_eq!(
            op(NodeKind::Control(block.clone())).python_type_name(),
            "DynamicDefinition"
        );
        let parallel = Region::Parallel {
            branches: Vec::new(),
            max_concurrency: None,
            stop_on_error: false,
        };
        assert_eq!(
            op(NodeKind::Control(parallel)).python_type_name(),
            "DynamicDefinition"
        );
        let try_finally = Region::TryFinally {
            body: Box::new(block.clone()),
            finally: Box::new(block),
        };
        assert_eq!(
            op(NodeKind::Control(try_finally)).python_type_name(),
            "DynamicDefinition"
        );
    }

    #[test]
    fn conditional_and_loop_regions_report_their_own_class_names() {
        let cond = Region::If {
            cond: Condition::Cel {
                expr: "true".to_string(),
                strict: false,
            },
            then_: Box::new(Region::Block {
                ops: Vec::new(),
                overlay: true,
            }),
            else_: None,
            threshold: 0.5,
        };
        assert_eq!(
            op(NodeKind::Control(cond)).python_type_name(),
            "ConditionalDefinition"
        );
        let loop_region = Region::Loop {
            spec: LoopSpec::Each {
                in_path: "input.items".to_string(),
                as_name: "item".to_string(),
                truncate: false,
            },
            body: Box::new(Region::Block {
                ops: Vec::new(),
                overlay: true,
            }),
            flow: LoopFlow::Chain,
            max_concurrency: None,
            max_iterations: None,
            min_iterations: 0,
            collect: None,
        };
        assert_eq!(
            op(NodeKind::Control(loop_region)).python_type_name(),
            "LoopDefinition"
        );
    }
}
