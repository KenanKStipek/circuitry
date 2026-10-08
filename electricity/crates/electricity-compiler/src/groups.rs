//! Lane C: ports `core/compiler.py`'s `unknown_concurrency_group_errors`
//! -- an effect's `group:` reference checked against the resolved
//! `runtime.concurrency_groups` names (`core/concurrency.py`'s
//! `UnknownConcurrencyGroupError`).
//!
//! The `max_concurrency`/`concurrency_groups` *configuration* errors
//! (`core/concurrency.py`'s `parse_max_concurrency`/
//! `parse_concurrency_groups`) are lane B's, in `pipeline.rs`: they
//! validate the merged runtime config itself, before a document even
//! compiles, not an effect's reference into it -- see that module's
//! `concurrency_config_errors`.
//!
//! Called from `pipeline.rs` (lane B), after a document compiles, in
//! both `check_for_run` and `check_report` -- mirroring
//! `cli/runtime_shim.py`'s own two calls to `unknown_concurrency_group_errors`
//! against the merged config's `runtime.concurrency_groups` names:
//! `run(RunRequest(..., validate_only=True))` (`cli/runtime_shim.py:756`)
//! and `validate()` (`cli/runtime_shim.py:1358`). Not a step of
//! `compile::compile_document` itself: unknown group references belong
//! to the pipeline surface, which knows the merged runtime config, not
//! to compilation, which doesn't.

use electricity_bytecode::{LeafKind, NodeKind, Op, Program, Region};
use electricity_value::Value;
use std::collections::BTreeSet;

/// Every `group:` name set on a tool/prompt effect anywhere under *op*,
/// ported from `core/compiler.py::collect_effect_groups`. Does not
/// descend into a `use` effect's child -- the IR never represents one
/// (`effects::UseOp` has no nested ops of its own), so nothing extra is
/// needed to honor that part of the port.
fn collect_effect_groups(op: &Op, groups: &mut BTreeSet<String>) {
    match &op.kind {
        NodeKind::Leaf(leaf) => match leaf.as_ref() {
            LeafKind::Tool(tool) => {
                if let Some(group) = &tool.group {
                    groups.insert(group.clone());
                }
            }
            LeafKind::Prompt(prompt) => {
                if let Some(group) = &prompt.group {
                    groups.insert(group.clone());
                }
            }
            LeafKind::Use(_) | LeafKind::Yield(_) => {}
            LeafKind::Reflector(reflector) => collect_region_groups(&reflector.inner, groups),
        },
        NodeKind::Control(region) => collect_region_groups(region, groups),
    }
}

fn collect_region_groups(region: &Region, groups: &mut BTreeSet<String>) {
    match region {
        Region::Block { ops, .. } => {
            for op in ops {
                collect_effect_groups(op, groups);
            }
        }
        Region::Parallel { branches, .. } => {
            for op in branches {
                collect_effect_groups(op, groups);
            }
        }
        Region::If { then_, else_, .. } => {
            collect_region_groups(then_, groups);
            if let Some(else_) = else_ {
                collect_region_groups(else_, groups);
            }
        }
        Region::Loop { body, .. } => collect_region_groups(body, groups),
        Region::TryFinally { body, finally } => {
            collect_region_groups(body, groups);
            collect_region_groups(finally, groups);
        }
    }
}

/// Every `group:` reference under *program*'s root that *known_groups*
/// -- `runtime.concurrency_groups`'s own keys -- doesn't define,
/// porting `core/compiler.py`'s `unknown_concurrency_group_errors`
/// (and the `collect_effect_groups` walk it calls) field for field.
pub(crate) fn unknown_group_errors(
    program: &Program,
    known_groups: &BTreeSet<String>,
) -> Vec<String> {
    let mut used = BTreeSet::new();
    collect_effect_groups(&program.root, &mut used);

    let unknown: Vec<&String> = used.difference(known_groups).collect();
    if unknown.is_empty() {
        return Vec::new();
    }
    let known_desc = if known_groups.is_empty() {
        "(none configured)".to_string()
    } else {
        known_groups.iter().cloned().collect::<Vec<_>>().join(", ")
    };
    unknown
        .into_iter()
        .map(|name| {
            format!(
                "group {} is not defined in runtime.concurrency_groups — known groups: {known_desc}.",
                Value::Str(name.clone()).py_repr()
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::unknown_group_errors;
    use electricity_bytecode::effects::{PromptContent, PromptOp, PromptType, RetryPolicy, ToolOp};
    use electricity_bytecode::param::ParamNode;
    use electricity_bytecode::path::EffectPath;
    use electricity_bytecode::template::{Escape, TemplateText};
    use electricity_bytecode::{LeafKind, NodeKind, OnError, Op, Program, Region};
    use indexmap::IndexMap;
    use std::collections::BTreeSet;

    fn tool_op(name: &str, group: Option<&str>) -> Op {
        Op {
            path: EffectPath::root().push_name(name),
            name: Some(name.to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
                provider: "shell".to_string(),
                params: ParamNode::Map(IndexMap::new()),
                params_json: None,
                prompt: None,
                model: None,
                timeout_ms: None,
                retries: RetryPolicy::default(),
                expect: None,
                description: None,
                group: group.map(|s| s.to_string()),
            }))),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        }
    }

    fn prompt_op(name: &str, group: Option<&str>) -> Op {
        Op {
            path: EffectPath::root().push_name(name),
            name: Some(name.to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Prompt(PromptOp {
                content: PromptContent::Template(TemplateText::new("hi", true, Escape::None)),
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
                group: group.map(|s| s.to_string()),
            }))),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        }
    }

    fn root_with(ops: Vec<Op>) -> Program {
        Program {
            root: Op {
                path: EffectPath::root(),
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops,
                    overlay: false,
                }),
                on_error: OnError::Fail,
                labels: None,
                enabled: true,
            },
            prompts: IndexMap::new(),
            effect_names: BTreeSet::new(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        }
    }

    fn names(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_groups_no_errors() {
        let program = root_with(vec![tool_op("a", None)]);
        assert!(unknown_group_errors(&program, &BTreeSet::new()).is_empty());
    }

    #[test]
    fn known_group_passes() {
        let program = root_with(vec![tool_op("a", Some("io"))]);
        assert!(unknown_group_errors(&program, &names(&["io"])).is_empty());
    }

    #[test]
    fn unknown_group_reports_with_known_desc() {
        let program = root_with(vec![tool_op("a", Some("io")), prompt_op("b", Some("llm"))]);
        let errors = unknown_group_errors(&program, &names(&["io"]));
        assert_eq!(
            errors,
            vec![
                "group 'llm' is not defined in runtime.concurrency_groups — known groups: io."
                    .to_string()
            ]
        );
    }

    #[test]
    fn unknown_group_with_no_known_groups_says_none_configured() {
        let program = root_with(vec![tool_op("a", Some("io"))]);
        let errors = unknown_group_errors(&program, &BTreeSet::new());
        assert_eq!(
            errors,
            vec!["group 'io' is not defined in runtime.concurrency_groups — known groups: (none configured).".to_string()]
        );
    }

    #[test]
    fn multiple_unknown_groups_sorted() {
        let program = root_with(vec![
            tool_op("a", Some("zeta")),
            tool_op("b", Some("alpha")),
        ]);
        let errors = unknown_group_errors(&program, &BTreeSet::new());
        assert_eq!(errors.len(), 2);
        assert!(errors[0].contains("'alpha'"));
        assert!(errors[1].contains("'zeta'"));
    }

    #[test]
    fn group_name_repr_uses_python_quoting() {
        let program = root_with(vec![tool_op("a", Some("it's"))]);
        let errors = unknown_group_errors(&program, &BTreeSet::new());
        assert_eq!(
            errors,
            vec!["group \"it's\" is not defined in runtime.concurrency_groups — known groups: (none configured).".to_string()]
        );
    }

    #[test]
    fn nested_group_found_inside_loop_and_if() {
        let inner_if = Op {
            path: EffectPath::root(),
            name: None,
            kind: NodeKind::Control(Region::If {
                cond: electricity_bytecode::Condition::Cel {
                    expr: "true".to_string(),
                    strict: false,
                },
                then_: Box::new(Region::Block {
                    ops: vec![tool_op("deep", Some("nested"))],
                    overlay: true,
                }),
                else_: None,
                threshold: 0.5,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let loop_op = Op {
            path: EffectPath::root().push_name("loop1"),
            name: Some("loop1".to_string()),
            kind: NodeKind::Control(Region::Loop {
                spec: electricity_bytecode::LoopSpec::Each {
                    in_path: "input.items".to_string(),
                    as_name: "item".to_string(),
                    truncate: false,
                },
                body: Box::new(Region::Block {
                    ops: vec![inner_if],
                    overlay: true,
                }),
                flow: electricity_bytecode::LoopFlow::Chain,
                max_concurrency: None,
                max_iterations: None,
                min_iterations: 0,
                collect: None,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = root_with(vec![loop_op]);
        let errors = unknown_group_errors(&program, &BTreeSet::new());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("'nested'"));
    }
}
