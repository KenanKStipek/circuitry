//! Lane A: a refusal walker over a compiled [`Program`] -- the first
//! effect path the M0-H VM (issue #431) cannot run, so lane D's CLI can
//! refuse it up front, before any state is written, with the preview
//! marker (#431's "Refused before the run starts" list) instead of
//! discovering mid-run that nothing implements a `prompt`/`loop`/`use`/
//! `reflector`/`yield` effect, a model-mode condition or `expect`, or an
//! unexpanded `{{> name}}` partial reference (the run-time half of #406,
//! out of scope for M0-H).
//!
//! Walks the whole compiled tree in document order and stops at the
//! first node [`RefusalReason`] names -- not a list of every offending
//! node, matching the CLI's own "the first" framing (one refusal message
//! per run, same as every other check-failure surface in this
//! workspace).

use crate::op::{LeafKind, NodeKind, Op};
use crate::param::ParamNode;
use crate::path::EffectPath;
use crate::program::Program;
use crate::region::{Condition, ExpectCondition, Region};
use crate::template::TemplateText;

/// Why [`first_unsupported`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// A `prompt` effect (M1-E).
    Prompt,
    /// A `loop` effect, `each` or `while` (M1-H).
    Loop,
    /// A `use` effect (M1-H).
    Use,
    /// A `reflector` effect (M1-H).
    Reflector,
    /// A `yield` effect (the run-time half of #406).
    Yield,
    /// An `if`/`while` condition in `mode: model` -- the compiler's own
    /// default when a condition has no `mode:` at all (#431's Scope
    /// section).
    ModelCondition,
    /// A tool/prompt/`use` `expect:` in `mode: model`.
    ModelExpect,
    /// The document declares a top-level `prompts:` map.
    DeclaredPrompts,
    /// An unexpanded `{{> name}}` partial reference in a field the M0-H
    /// VM would otherwise render (a supported tool's `prompt`/`params`/
    /// `params_json`) -- the run-time half of #406 never ran to splice
    /// it in.
    PartialReference,
}

/// The first effect path (and why) [`first_unsupported`] found the
/// M0-H VM cannot run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub path: EffectPath,
    pub reason: RefusalReason,
}

/// Walks *program* in document order, returning the first [`Refusal`],
/// or `None` once every effect path is one the M0-H VM actually runs
/// (`tool`/`dynamic`/a CEL `if`/`finally:` only -- #431's Goal).
pub fn first_unsupported(program: &Program) -> Option<Refusal> {
    if !program.prompts.is_empty() {
        return Some(Refusal {
            path: EffectPath::root(),
            reason: RefusalReason::DeclaredPrompts,
        });
    }
    walk_op(&program.root)
}

fn walk_op(op: &Op) -> Option<Refusal> {
    match &op.kind {
        NodeKind::Leaf(leaf) => walk_leaf(&op.path, leaf),
        NodeKind::Control(region) => walk_region(&op.path, region),
    }
}

fn refusal(path: &EffectPath, reason: RefusalReason) -> Option<Refusal> {
    Some(Refusal {
        path: path.clone(),
        reason,
    })
}

fn walk_leaf(path: &EffectPath, leaf: &LeafKind) -> Option<Refusal> {
    match leaf {
        LeafKind::Prompt(_) => refusal(path, RefusalReason::Prompt),
        LeafKind::Use(_) => refusal(path, RefusalReason::Use),
        LeafKind::Reflector(_) => refusal(path, RefusalReason::Reflector),
        LeafKind::Yield(_) => refusal(path, RefusalReason::Yield),
        LeafKind::Tool(tool) => {
            if matches!(&tool.expect, Some(ExpectCondition::Model { .. })) {
                return refusal(path, RefusalReason::ModelExpect);
            }
            if tool.prompt.as_ref().is_some_and(has_partial)
                || tool.params_json.as_ref().is_some_and(has_partial)
                || param_node_has_partial(&tool.params)
            {
                return refusal(path, RefusalReason::PartialReference);
            }
            None
        }
    }
}

fn walk_region(path: &EffectPath, region: &Region) -> Option<Refusal> {
    match region {
        Region::Block { ops, .. } => ops.iter().find_map(walk_op),
        Region::Parallel { branches, .. } => branches.iter().find_map(walk_op),
        Region::TryFinally { body, finally } => {
            walk_region(path, body).or_else(|| walk_region(path, finally))
        }
        Region::If {
            cond, then_, else_, ..
        } => {
            if matches!(cond, Condition::Model { .. }) {
                return refusal(path, RefusalReason::ModelCondition);
            }
            walk_region(path, then_)
                .or_else(|| else_.as_ref().and_then(|region| walk_region(path, region)))
        }
        // A `loop` is refused outright regardless of its own body or
        // `while:` condition's mode -- M1-H's own milestone, not a
        // narrower per-field gap this lane's `{{>`/model-mode checks
        // would otherwise also catch inside it.
        Region::Loop { .. } => refusal(path, RefusalReason::Loop),
    }
}

fn has_partial(text: &TemplateText) -> bool {
    text.source.contains("{{>")
}

fn param_node_has_partial(node: &ParamNode) -> bool {
    match node {
        ParamNode::Template(text) => has_partial(text),
        ParamNode::Map(entries) => entries.values().any(param_node_has_partial),
        ParamNode::List(items) => items.iter().any(param_node_has_partial),
        ParamNode::Literal(_) | ParamNode::From { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{PromptContent, PromptOp, PromptType, RetryPolicy, ToolOp};
    use crate::op::OnError;
    use crate::template::Escape;
    use indexmap::IndexMap;
    use std::collections::BTreeSet;

    fn tool_op(params: ParamNode) -> ToolOp {
        ToolOp {
            provider: "json".to_string(),
            params,
            params_json: None,
            prompt: None,
            model: None,
            timeout_ms: None,
            retries: RetryPolicy::default(),
            expect: None,
            description: None,
            group: None,
        }
    }

    fn leaf_op(path: EffectPath, kind: LeafKind) -> Op {
        Op {
            path,
            name: Some("n".to_string()),
            kind: NodeKind::Leaf(Box::new(kind)),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        }
    }

    fn root_block(ops: Vec<Op>) -> Op {
        Op {
            path: EffectPath::root(),
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops,
                overlay: false,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        }
    }

    fn program_with_root(root: Op) -> Program {
        Program {
            root,
            prompts: IndexMap::new(),
            effect_names: BTreeSet::new(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        }
    }

    #[test]
    fn a_plain_tool_chain_is_fully_supported() {
        let tool_path = EffectPath::root().push_name("fetch");
        let tool = leaf_op(
            tool_path,
            LeafKind::Tool(tool_op(ParamNode::Map(IndexMap::new()))),
        );
        let program = program_with_root(root_block(vec![tool]));
        assert_eq!(first_unsupported(&program), None);
    }

    #[test]
    fn declared_prompts_refuses_at_the_document_level() {
        let mut program = program_with_root(root_block(vec![]));
        program
            .prompts
            .insert("greeting".to_string(), "hi".to_string());
        let refusal = first_unsupported(&program).unwrap();
        assert_eq!(refusal.reason, RefusalReason::DeclaredPrompts);
    }

    #[test]
    fn a_nested_use_effect_is_found_by_path() {
        let use_path = EffectPath::root().push_name("sub");
        let use_op = leaf_op(
            use_path.clone(),
            LeafKind::Use(crate::effects::UseOp {
                source: crate::effects::UseSource::Path("child.yml".to_string()),
                inputs: None,
                outputs: None,
                validate: true,
                retries: RetryPolicy::default(),
                expect: None,
                description: None,
            }),
        );
        let program = program_with_root(root_block(vec![use_op]));
        let refusal = first_unsupported(&program).unwrap();
        assert_eq!(refusal.reason, RefusalReason::Use);
        assert_eq!(refusal.path, use_path);
    }

    #[test]
    fn model_mode_if_is_refused() {
        let if_op = Op {
            path: EffectPath::root().push_name("gate"),
            name: Some("gate".to_string()),
            kind: NodeKind::Control(Region::If {
                cond: Condition::Model {
                    template: TemplateText::new("ok?", false, Escape::None),
                },
                then_: Box::new(Region::Block {
                    ops: Vec::new(),
                    overlay: true,
                }),
                else_: None,
                threshold: 0.5,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = program_with_root(root_block(vec![if_op]));
        let refusal = first_unsupported(&program).unwrap();
        assert_eq!(refusal.reason, RefusalReason::ModelCondition);
    }

    #[test]
    fn cel_if_with_nested_use_is_found_inside_the_branch() {
        let use_path = EffectPath::root().push_name("gate").push_name("sub");
        let use_op = leaf_op(
            use_path.clone(),
            LeafKind::Use(crate::effects::UseOp {
                source: crate::effects::UseSource::Path("child.yml".to_string()),
                inputs: None,
                outputs: None,
                validate: true,
                retries: RetryPolicy::default(),
                expect: None,
                description: None,
            }),
        );
        let if_op = Op {
            path: EffectPath::root().push_name("gate"),
            name: Some("gate".to_string()),
            kind: NodeKind::Control(Region::If {
                cond: Condition::Cel {
                    expr: "true".to_string(),
                    strict: false,
                },
                then_: Box::new(Region::Block {
                    ops: vec![use_op],
                    overlay: true,
                }),
                else_: None,
                threshold: 0.5,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = program_with_root(root_block(vec![if_op]));
        let refusal = first_unsupported(&program).unwrap();
        assert_eq!(refusal.reason, RefusalReason::Use);
        assert_eq!(refusal.path, use_path);
    }

    #[test]
    fn a_loop_is_refused_even_with_an_otherwise_supported_body() {
        let loop_op = Op {
            path: EffectPath::root().push_name("each"),
            name: Some("each".to_string()),
            kind: NodeKind::Control(Region::Loop {
                spec: crate::region::LoopSpec::Each {
                    in_path: "input.items".to_string(),
                    as_name: "item".to_string(),
                    truncate: false,
                },
                body: Box::new(Region::Block {
                    ops: Vec::new(),
                    overlay: true,
                }),
                flow: crate::region::LoopFlow::Chain,
                max_concurrency: None,
                max_iterations: None,
                min_iterations: 0,
                collect: None,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = program_with_root(root_block(vec![loop_op]));
        let refusal = first_unsupported(&program).unwrap();
        assert_eq!(refusal.reason, RefusalReason::Loop);
    }

    #[test]
    fn a_tool_expect_in_model_mode_is_refused() {
        let mut op = tool_op(ParamNode::Map(IndexMap::new()));
        op.expect = Some(ExpectCondition::Model {
            template: TemplateText::new("ok?", false, Escape::None),
        });
        let tool = leaf_op(EffectPath::root().push_name("fetch"), LeafKind::Tool(op));
        let program = program_with_root(root_block(vec![tool]));
        let refusal = first_unsupported(&program).unwrap();
        assert_eq!(refusal.reason, RefusalReason::ModelExpect);
    }

    #[test]
    fn an_unexpanded_partial_in_tool_params_is_refused() {
        let mut params = IndexMap::new();
        params.insert(
            electricity_value::Value::Str("cmd".to_string()),
            ParamNode::Template(TemplateText::new("{{> greeting}}", true, Escape::Html)),
        );
        let tool = leaf_op(
            EffectPath::root().push_name("fetch"),
            LeafKind::Tool(tool_op(ParamNode::Map(params))),
        );
        let program = program_with_root(root_block(vec![tool]));
        let refusal = first_unsupported(&program).unwrap();
        assert_eq!(refusal.reason, RefusalReason::PartialReference);
    }

    #[test]
    fn a_prompt_effect_is_refused_before_a_later_use_effect() {
        let prompt = leaf_op(
            EffectPath::root().push_name("ask"),
            LeafKind::Prompt(PromptOp {
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
            }),
        );
        let use_op = leaf_op(
            EffectPath::root().push_name("sub"),
            LeafKind::Use(crate::effects::UseOp {
                source: crate::effects::UseSource::Path("child.yml".to_string()),
                inputs: None,
                outputs: None,
                validate: true,
                retries: RetryPolicy::default(),
                expect: None,
                description: None,
            }),
        );
        let program = program_with_root(root_block(vec![prompt, use_op]));
        let refusal = first_unsupported(&program).unwrap();
        assert_eq!(refusal.reason, RefusalReason::Prompt);
    }
}
