//! Lane J2 (issue #518): a loaded profile's per-effect `model`/
//! `provider`/`enabled`/`routing` overlay, applied directly onto a
//! compiled [`electricity_bytecode::Program`] -- `core/compiler.py`'s
//! `apply_effect_overrides`/`_overlay_effect`/`_disable_subtree`
//! (`:451-587`), ported over mutable IR rather than Python's
//! `dataclasses.replace` rebuild: both end at the same tree, this one
//! without the intermediate immutable copies.
//!
//! Unlike Python, which threads a dotted-path string alongside each
//! `*Definition` it visits (`_overlay_effect`'s own `path`/`own_path`
//! parameters), this walk never builds one of its own: every compiled
//! [`Op`] already carries its own fully-resolved [`EffectPath`]
//! (lane A), so a key is read straight off `op.path` wherever the walk
//! already is.
//!
//! The caller (`electricity`'s own run wiring) only calls
//! [`apply_effect_overrides`] when a loaded profile's `effects:` map is
//! non-empty, right after `post_state_checks` and before the refusal
//! walker (settled question 1, issue #518).

use electricity_bytecode::{
    EffectPath, LeafKind, NodeKind, Op, PathSegment, Program, Region, RoutingOverride,
};
use electricity_value::{Dict, Value};
use indexmap::IndexMap;

/// `_overlay_effect`'s own dotted key for *path*: the leading `prime`
/// segment and every [`PathSegment::Pass`] placeholder dropped, the
/// rest joined with `.` -- e.g. `prime.lp.<pass>.step` becomes
/// `lp.step`. The root's own path (`prime` alone) yields `""`, which
/// never matches a validated profile key (J1 never admits an empty
/// one).
fn override_key(path: &EffectPath) -> String {
    path.0
        .iter()
        .skip(1)
        .filter_map(|segment| match segment {
            PathSegment::Name(name) => Some(name.as_str()),
            PathSegment::Pass(_) => None,
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn dict_get<'a>(dict: &'a Dict, key: &str) -> Option<&'a Value> {
    dict.get(&Value::Str(key.to_string()))
}

/// Applies *override*'s `model`/`provider`/`routing` entries onto *op*,
/// following Python's own `hasattr` field mapping (`core/compiler.py:
/// 500-505`): a field with no counterpart on this op's own leaf type is
/// silently ignored, exactly like Python's `hasattr` guard.
fn apply_fields(op: &mut Op, override_: &Dict) {
    let model = dict_get(override_, "model").and_then(Value::as_str);
    let provider = dict_get(override_, "provider").and_then(Value::as_str);
    let routing = dict_get(override_, "routing");

    let NodeKind::Leaf(leaf) = &mut op.kind else {
        return;
    };
    match leaf.as_mut() {
        LeafKind::Prompt(prompt_op) => {
            if let Some(model) = model {
                prompt_op.model = Some(model.to_string());
            }
            if let Some(provider) = provider {
                prompt_op.provider = Some(provider.to_string());
            }
            if let Some(routing) = routing {
                prompt_op.routing_override = match routing {
                    Value::Bool(b) => Some(RoutingOverride::Bool(*b)),
                    Value::Str(s) => Some(RoutingOverride::Named(s.clone())),
                    _ => prompt_op.routing_override.clone(),
                };
            }
        }
        LeafKind::Tool(tool_op) => {
            if let Some(model) = model {
                tool_op.model = Some(model.to_string());
            }
            if let Some(provider) = provider {
                tool_op.provider = provider.to_string();
            }
        }
        LeafKind::Use(_) | LeafKind::Yield(_) | LeafKind::Reflector(_) | LeafKind::Report(_) => {}
    }
}

/// `_disable_subtree`: *op* and every descendant rebuilt with
/// `enabled = false` -- reaches [`Region::TryFinally`]'s `finally`,
/// [`Region::If`]'s `else`, a [`Region::Loop`]'s body and
/// [`crate::effects::ReflectorOp`]'s own `inner`, same as Python.
fn disable_subtree(op: &mut Op) {
    op.enabled = false;
    match &mut op.kind {
        NodeKind::Leaf(leaf) => {
            if let LeafKind::Reflector(reflector) = leaf.as_mut() {
                disable_region(&mut reflector.inner);
            }
        }
        NodeKind::Control(region) => disable_region(region),
    }
}

fn disable_region(region: &mut Region) {
    match region {
        Region::Block { ops, .. } => ops.iter_mut().for_each(disable_subtree),
        Region::Parallel { branches, .. } => branches.iter_mut().for_each(disable_subtree),
        Region::TryFinally { body, finally } => {
            disable_region(body);
            disable_region(finally);
        }
        Region::If { then_, else_, .. } => {
            disable_region(then_);
            if let Some(else_region) = else_ {
                disable_region(else_region);
            }
        }
        Region::Loop { body, .. } => disable_region(body),
    }
}

/// `_overlay_effect`'s own per-node visit: looks *op* up in *effects* by
/// its own already-resolved key, applies a matched entry's `model`/
/// `provider`/`routing` fields, then recurses into every nested region
/// this op owns (a [`Region::Loop`]/[`Region::If`]/a `dynamic`'s own
/// [`Region::Block`]/[`Region::Parallel`]/[`Region::TryFinally`], or a
/// [`crate::effects::ReflectorOp`]'s `inner`) -- never into a `use`
/// child, which has none of its own at compile time. The children are
/// walked, and any overrides of their own applied, *before* this node's
/// own `enabled: false` (if present) disables the whole subtree,
/// exactly as Python's own `replace(...)` order does.
fn overlay_op(op: &mut Op, effects: &IndexMap<String, Dict>) {
    let own_key = override_key(&op.path);
    let mut disable = false;
    if !own_key.is_empty() {
        if let Some(override_) = effects.get(&own_key) {
            apply_fields(op, override_);
            disable = matches!(dict_get(override_, "enabled"), Some(Value::Bool(false)));
        }
    }

    match &mut op.kind {
        NodeKind::Leaf(leaf) => {
            if let LeafKind::Reflector(reflector) = leaf.as_mut() {
                overlay_region(&mut reflector.inner, effects);
            }
        }
        NodeKind::Control(region) => overlay_region(region, effects),
    }

    if disable {
        disable_subtree(op);
    }
}

fn overlay_region(region: &mut Region, effects: &IndexMap<String, Dict>) {
    match region {
        Region::Block { ops, .. } => {
            for op in ops.iter_mut() {
                overlay_op(op, effects);
            }
        }
        Region::Parallel { branches, .. } => {
            for op in branches.iter_mut() {
                overlay_op(op, effects);
            }
        }
        Region::TryFinally { body, finally } => {
            overlay_region(body, effects);
            overlay_region(finally, effects);
        }
        Region::If { then_, else_, .. } => {
            overlay_region(then_, effects);
            if let Some(else_region) = else_ {
                overlay_region(else_region, effects);
            }
        }
        Region::Loop { body, .. } => overlay_region(body, effects),
    }
}

/// `apply_effect_overrides` (`core/compiler.py:451-482`): applies a
/// loaded profile's own `effects:` map onto *program*'s compiled IR, in
/// place. Walks the regions under `program.root`, never `root` itself
/// (an unnamed document root has no key any profile entry could ever
/// address anyway -- [`override_key`]'s own empty-string result for it).
/// A no-op for an empty *effects* map, matching the caller's own guard
/// (settled question 1, issue #518) -- but harmless either way, since
/// an empty map matches nothing.
pub fn apply_effect_overrides(program: &mut Program, effects: &IndexMap<String, Dict>) {
    if effects.is_empty() {
        return;
    }
    if let NodeKind::Control(region) = &mut program.root.kind {
        overlay_region(region, effects);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DocumentOrigin;
    use crate::compile::compile_document;

    fn compile(yaml: &str) -> Program {
        let document = electricity_yaml::load_yaml(yaml).expect("valid yaml");
        compile_document(&document, &DocumentOrigin::Generated).expect("valid document")
    }

    fn block_ops(region: &Region) -> &[Op] {
        match region {
            Region::Block { ops, .. } => ops,
            _ => panic!("expected Region::Block, got {region:?}"),
        }
    }

    fn find<'a>(ops: &'a [Op], name: &str) -> &'a Op {
        ops.iter()
            .find(|op| op.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("no op named {name}"))
    }

    fn overrides_of(pairs: &[(&str, &[(&str, Value)])]) -> IndexMap<String, Dict> {
        let mut effects = IndexMap::new();
        for (path, fields) in pairs {
            let mut dict = Dict::new();
            for (key, value) in *fields {
                dict.insert(Value::Str(key.to_string()), value.clone());
            }
            effects.insert(path.to_string(), dict);
        }
        effects
    }

    #[test]
    fn the_roots_own_key_is_always_empty() {
        let program =
            compile("effects:\n  - type: tool\n    name: a\n    provider: json\n    params: {}\n");
        assert_eq!(override_key(&program.root.path), "");
    }

    #[test]
    fn a_nested_dynamics_own_child_gets_the_dotted_key() {
        let program = compile(
            "effects:\n  - type: dynamic\n    name: outer\n    effects:\n      - type: tool\n        \
             name: inner\n        provider: json\n        params: {}\n",
        );
        let root_ops = block_ops(match &program.root.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        });
        let outer = find(root_ops, "outer");
        assert_eq!(override_key(&outer.path), "outer");
        let inner_ops = block_ops(match &outer.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        });
        let inner = find(inner_ops, "inner");
        assert_eq!(override_key(&inner.path), "outer.inner");
    }

    #[test]
    fn a_named_loops_body_step_drops_the_pass_placeholder() {
        let program = compile(
            "effects:\n  - type: loop\n    name: lp\n    each: {in: \"prime.x\", as: item}\n    \
             body:\n      - type: tool\n        name: step\n        provider: json\n        params: {}\n",
        );
        let root_ops = block_ops(match &program.root.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        });
        let lp = find(root_ops, "lp");
        assert_eq!(override_key(&lp.path), "lp");
        let Region::Loop { body, .. } = (match &lp.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        }) else {
            panic!("expected a loop region")
        };
        let step = find(block_ops(body), "step");
        assert_eq!(override_key(&step.path), "lp.step");
    }

    #[test]
    fn an_unnamed_if_and_loop_take_their_containers_own_key() {
        let program = compile(
            "effects:\n  - type: if\n    if: {mode: cel, expr: \"true\"}\n    then:\n      - type: \
             tool\n        name: x\n        provider: json\n        params: {}\n  - type: loop\n    each: \
             {in: \"prime.x\", as: item}\n    body:\n      - type: tool\n        name: y\n        provider: \
             json\n        params: {}\n",
        );
        let root_ops = block_ops(match &program.root.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        });
        assert_eq!(root_ops.len(), 2);
        let if_op = &root_ops[0];
        assert!(if_op.name.is_none());
        assert_eq!(override_key(&if_op.path), override_key(&program.root.path));
        let Region::If { then_, .. } = (match &if_op.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        }) else {
            panic!("expected an if region")
        };
        let x = find(block_ops(then_), "x");
        assert_eq!(override_key(&x.path), "x");

        let loop_op = &root_ops[1];
        assert!(loop_op.name.is_none());
        assert_eq!(
            override_key(&loop_op.path),
            override_key(&program.root.path)
        );
        let Region::Loop { body, .. } = (match &loop_op.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        }) else {
            panic!("expected a loop region")
        };
        let y = find(block_ops(body), "y");
        assert_eq!(override_key(&y.path), "y");
    }

    #[test]
    fn a_reflectors_inner_ops_get_the_reflectors_own_key_prefix() {
        let program = compile(
            "effects:\n  - type: reflector\n    name: planit\n    effects:\n      - type: prompt\n        \
             name: inner_step\n        template: go\n",
        );
        let root_ops = block_ops(match &program.root.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        });
        let planit = find(root_ops, "planit");
        assert_eq!(override_key(&planit.path), "planit");
        let NodeKind::Leaf(leaf) = &planit.kind else {
            panic!("expected a leaf")
        };
        let LeafKind::Reflector(reflector) = leaf.as_ref() else {
            panic!("expected a reflector")
        };
        let inner_step = find(block_ops(&reflector.inner), "inner_step");
        assert_eq!(override_key(&inner_step.path), "planit.inner_step");
    }

    #[test]
    fn a_finally_effect_shares_its_dynamics_own_scope() {
        let program = compile(
            "effects:\n  - type: dynamic\n    name: d\n    effects:\n      - type: tool\n        name: \
             body\n        provider: json\n        params: {}\n    finally:\n      - type: tool\n        \
             name: cleanup\n        provider: json\n        params: {}\n",
        );
        let root_ops = block_ops(match &program.root.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        });
        let d = find(root_ops, "d");
        let Region::TryFinally { body, finally } = (match &d.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        }) else {
            panic!("expected a try/finally region")
        };
        assert_eq!(override_key(&find(block_ops(body), "body").path), "d.body");
        assert_eq!(
            override_key(&find(block_ops(finally), "cleanup").path),
            "d.cleanup"
        );
    }

    #[test]
    fn model_provider_and_a_named_routing_pin_apply_to_a_prompt() {
        let mut program = compile("effects:\n  - type: prompt\n    name: p\n    template: hi\n");
        let effects = overrides_of(&[(
            "p",
            &[
                ("model", Value::Str("gpt-4".to_string())),
                ("provider", Value::Str("scripted:m2".to_string())),
                ("routing", Value::Str("fast".to_string())),
            ],
        )]);
        apply_effect_overrides(&mut program, &effects);
        let root_ops = block_ops(match &program.root.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        });
        let p = find(root_ops, "p");
        let NodeKind::Leaf(leaf) = &p.kind else {
            panic!("expected a leaf")
        };
        let LeafKind::Prompt(prompt_op) = leaf.as_ref() else {
            panic!("expected a prompt")
        };
        assert_eq!(prompt_op.model, Some("gpt-4".to_string()));
        assert_eq!(prompt_op.provider, Some("scripted:m2".to_string()));
        assert_eq!(
            prompt_op.routing_override,
            Some(RoutingOverride::Named("fast".to_string()))
        );
    }

    #[test]
    fn routing_false_becomes_a_bool_opt_out() {
        let mut program = compile("effects:\n  - type: prompt\n    name: p\n    template: hi\n");
        let effects = overrides_of(&[("p", &[("routing", Value::Bool(false))])]);
        apply_effect_overrides(&mut program, &effects);
        let root_ops = block_ops(match &program.root.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        });
        let p = find(root_ops, "p");
        let NodeKind::Leaf(leaf) = &p.kind else {
            panic!("expected a leaf")
        };
        let LeafKind::Prompt(prompt_op) = leaf.as_ref() else {
            panic!("expected a prompt")
        };
        assert_eq!(
            prompt_op.routing_override,
            Some(RoutingOverride::Bool(false))
        );
    }

    #[test]
    fn model_and_provider_apply_to_a_tool_which_has_no_routing_field() {
        let mut program =
            compile("effects:\n  - type: tool\n    name: t\n    provider: json\n    params: {}\n");
        let effects = overrides_of(&[(
            "t",
            &[
                ("model", Value::Str("m1".to_string())),
                ("provider", Value::Str("hash".to_string())),
                ("routing", Value::Str("fast".to_string())),
            ],
        )]);
        apply_effect_overrides(&mut program, &effects);
        let root_ops = block_ops(match &program.root.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        });
        let t = find(root_ops, "t");
        let NodeKind::Leaf(leaf) = &t.kind else {
            panic!("expected a leaf")
        };
        let LeafKind::Tool(tool_op) = leaf.as_ref() else {
            panic!("expected a tool")
        };
        assert_eq!(tool_op.model, Some("m1".to_string()));
        assert_eq!(tool_op.provider, "hash".to_string());
    }

    #[test]
    fn a_uses_own_fields_are_left_alone_but_it_can_still_be_disabled() {
        let mut program = compile(
            "effects:\n  - type: use\n    name: u\n    on_error: skip\n    inline: |\n      - \
             just-a-list\n",
        );
        let effects = overrides_of(&[(
            "u",
            &[
                ("model", Value::Str("m1".to_string())),
                ("provider", Value::Str("p1".to_string())),
                ("enabled", Value::Bool(false)),
            ],
        )]);
        apply_effect_overrides(&mut program, &effects);
        let root_ops = block_ops(match &program.root.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        });
        let u = find(root_ops, "u");
        assert!(!u.enabled);
    }

    #[test]
    fn enabled_true_changes_nothing_but_other_fields_still_apply() {
        let mut program =
            compile("effects:\n  - type: tool\n    name: t\n    provider: json\n    params: {}\n");
        let effects = overrides_of(&[(
            "t",
            &[
                ("enabled", Value::Bool(true)),
                ("model", Value::Str("m2".to_string())),
            ],
        )]);
        apply_effect_overrides(&mut program, &effects);
        let root_ops = block_ops(match &program.root.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        });
        let t = find(root_ops, "t");
        assert!(t.enabled);
        let NodeKind::Leaf(leaf) = &t.kind else {
            panic!("expected a leaf")
        };
        let LeafKind::Tool(tool_op) = leaf.as_ref() else {
            panic!("expected a tool")
        };
        assert_eq!(tool_op.model, Some("m2".to_string()));
    }

    #[test]
    fn unknown_override_keys_are_ignored() {
        let mut program =
            compile("effects:\n  - type: tool\n    name: t\n    provider: json\n    params: {}\n");
        let effects = overrides_of(&[("t", &[("nonsense", Value::Str("foo".to_string()))])]);
        apply_effect_overrides(&mut program, &effects);
        let root_ops = block_ops(match &program.root.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        });
        let t = find(root_ops, "t");
        assert!(t.enabled);
        let NodeKind::Leaf(leaf) = &t.kind else {
            panic!("expected a leaf")
        };
        let LeafKind::Tool(tool_op) = leaf.as_ref() else {
            panic!("expected a tool")
        };
        assert_eq!(tool_op.model, None);
    }

    #[test]
    fn disabling_a_dynamic_disables_its_body_and_its_finally() {
        let mut program = compile(
            "effects:\n  - type: dynamic\n    name: d\n    effects:\n      - type: tool\n        name: \
             body\n        provider: json\n        params: {}\n    finally:\n      - type: tool\n        \
             name: cleanup\n        provider: json\n        params: {}\n",
        );
        let effects = overrides_of(&[("d", &[("enabled", Value::Bool(false))])]);
        apply_effect_overrides(&mut program, &effects);
        let root_ops = block_ops(match &program.root.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        });
        let d = find(root_ops, "d");
        assert!(!d.enabled);
        let Region::TryFinally { body, finally } = (match &d.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        }) else {
            panic!("expected a try/finally region")
        };
        assert!(!find(block_ops(body), "body").enabled);
        assert!(!find(block_ops(finally), "cleanup").enabled);
    }

    #[test]
    fn disabling_an_if_disables_both_its_then_and_its_else() {
        let mut program = compile(
            "effects:\n  - type: if\n    name: gate\n    if: {mode: cel, expr: \"true\"}\n    then:\n      \
             - type: tool\n        name: x\n        provider: json\n        params: {}\n    else:\n      - \
             type: tool\n        name: y\n        provider: json\n        params: {}\n",
        );
        let effects = overrides_of(&[("gate", &[("enabled", Value::Bool(false))])]);
        apply_effect_overrides(&mut program, &effects);
        let root_ops = block_ops(match &program.root.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        });
        let gate = find(root_ops, "gate");
        assert!(!gate.enabled);
        let Region::If { then_, else_, .. } = (match &gate.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        }) else {
            panic!("expected an if region")
        };
        assert!(!find(block_ops(then_), "x").enabled);
        assert!(!find(block_ops(else_.as_deref().unwrap()), "y").enabled);
    }

    #[test]
    fn disabling_a_loop_disables_its_body() {
        let mut program = compile(
            "effects:\n  - type: loop\n    name: lp\n    each: {in: \"prime.x\", as: item}\n    body:\n      \
             - type: tool\n        name: step\n        provider: json\n        params: {}\n",
        );
        let effects = overrides_of(&[("lp", &[("enabled", Value::Bool(false))])]);
        apply_effect_overrides(&mut program, &effects);
        let root_ops = block_ops(match &program.root.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        });
        let lp = find(root_ops, "lp");
        assert!(!lp.enabled);
        let Region::Loop { body, .. } = (match &lp.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        }) else {
            panic!("expected a loop region")
        };
        assert!(!find(block_ops(body), "step").enabled);
    }

    #[test]
    fn disabling_a_reflector_disables_its_inner_ops() {
        let mut program = compile(
            "effects:\n  - type: reflector\n    name: planit\n    effects:\n      - type: prompt\n        \
             name: inner_step\n        template: go\n",
        );
        let effects = overrides_of(&[("planit", &[("enabled", Value::Bool(false))])]);
        apply_effect_overrides(&mut program, &effects);
        let root_ops = block_ops(match &program.root.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        });
        let planit = find(root_ops, "planit");
        assert!(!planit.enabled);
        let NodeKind::Leaf(leaf) = &planit.kind else {
            panic!("expected a leaf")
        };
        let LeafKind::Reflector(reflector) = leaf.as_ref() else {
            panic!("expected a reflector")
        };
        assert!(!find(block_ops(&reflector.inner), "inner_step").enabled);
    }

    #[test]
    fn overrides_on_children_apply_before_the_subtree_is_disabled() {
        let mut program = compile(
            "effects:\n  - type: dynamic\n    name: d\n    effects:\n      - type: tool\n        name: \
             body\n        provider: json\n        params: {}\n",
        );
        let effects = overrides_of(&[
            ("d", &[("enabled", Value::Bool(false))]),
            (
                "d.body",
                &[("model", Value::Str("still-applied".to_string()))],
            ),
        ]);
        apply_effect_overrides(&mut program, &effects);
        let root_ops = block_ops(match &program.root.kind {
            NodeKind::Control(region) => region,
            _ => unreachable!(),
        });
        let d = find(root_ops, "d");
        assert!(!d.enabled);
        let body_op = find(
            block_ops(match &d.kind {
                NodeKind::Control(region) => region,
                _ => unreachable!(),
            }),
            "body",
        );
        assert!(!body_op.enabled);
        let NodeKind::Leaf(leaf) = &body_op.kind else {
            panic!("expected a leaf")
        };
        let LeafKind::Tool(tool_op) = leaf.as_ref() else {
            panic!("expected a tool")
        };
        assert_eq!(tool_op.model, Some("still-applied".to_string()));
    }
}
