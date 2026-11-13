//! `circuitry.generated_doc`, ported -- the generated-document ceiling
//! (#772). A document produced while a run executes (a reflector's
//! plan, or a `use` whose `inline:` is rendered from state) can carry a
//! tool effect that looks exactly like a document the author wrote by
//! hand. This module tells the two apart and enforces the ceiling, with
//! the same wording Python's own `circuitry.generated_doc` uses.
//!
//! See that module's own doc comment for the rule in full; ported
//! function-for-function here so a change to one is a visible diff
//! against the other.

use electricity_value::{Dict, Value};
use std::collections::BTreeSet;

/// `runtime_config` key holding the current document's own, statically
/// referenced tool-plugin provider names -- `circuitry.generated_doc.
/// GENERATED_DOCUMENT_PROVIDERS_KEY`. Not read from electricity's own
/// `runtime_config` today (that value is a single, run-wide, immutable
/// reference -- `RunContext::runtime_config`'s own doc comment); the
/// per-document ceiling instead rides `Frame::document_tool_providers`,
/// which is threaded the same way `Frame::use_call_stack` is. Kept here,
/// unused, only so a reader comparing the two modules finds the matching
/// constant in both.
#[allow(dead_code)]
pub const GENERATED_DOCUMENT_PROVIDERS_KEY: &str = "_document_tool_providers";

fn get<'a>(dict: &'a Dict, key: &str) -> Option<&'a Value> {
    dict.get(&Value::Str(key.to_string()))
}

fn effects_of(effect: &Dict) -> Option<&Value> {
    get(effect, "effects").or_else(|| get(effect, "steps"))
}

/// Best-effort YAML parse, or `None` on any failure -- used only to look
/// for literal structure, never to execute or trust the result as a
/// real document.
fn best_effort_yaml(text: &str) -> Option<Value> {
    electricity_yaml::load_yaml(text).ok()
}

fn walk_tool_providers(effects: Option<&Value>, providers: &mut BTreeSet<String>) {
    let Some(Value::List(effects)) = effects else {
        return;
    };
    for effect in effects {
        let Some(effect) = effect.as_dict() else {
            continue;
        };
        let etype = get(effect, "type").and_then(Value::as_str).unwrap_or("");
        match etype {
            "tool" => {
                if let Some(provider) = get(effect, "provider").and_then(Value::as_str) {
                    let provider = provider.trim();
                    if !provider.is_empty() {
                        providers.insert(provider.to_string());
                    }
                }
            }
            "dynamic" => {
                walk_tool_providers(effects_of(effect), providers);
                walk_tool_providers(get(effect, "finally"), providers);
            }
            "if" | "conditional" => {
                walk_tool_providers(get(effect, "then"), providers);
                walk_tool_providers(get(effect, "else"), providers);
            }
            "loop" => {
                walk_tool_providers(get(effect, "body"), providers);
            }
            "reflector" => {
                walk_tool_providers(effects_of(effect), providers);
            }
            "use" => {
                if let Some(inline) = get(effect, "inline").and_then(Value::as_str) {
                    if let Some(parsed) = best_effort_yaml(inline) {
                        if let Some(parsed) = parsed.as_dict() {
                            walk_tool_providers(effects_of(parsed), providers);
                            walk_tool_providers(get(parsed, "finally"), providers);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// `circuitry.generated_doc.static_tool_providers` -- every `type: tool`
/// provider this document's own text references, recursing into a
/// literal `use: inline:` child's own text; never follows `path:`.
pub fn static_tool_providers(orch: &Value) -> BTreeSet<String> {
    let mut providers = BTreeSet::new();
    if let Some(dict) = orch.as_dict() {
        walk_tool_providers(effects_of(dict), &mut providers);
        walk_tool_providers(get(dict, "finally"), &mut providers);
    }
    providers
}

/// The root document's own static tool providers (#772), walked off the
/// *compiled* tree rather than raw YAML -- `execute_root`'s own entry
/// point has no raw document in hand (`electricity_bytecode::Program`
/// never carries one, only what compiled from it), unlike a `use`
/// child's own [`LoadedChild::document`](crate::exec::use_), which does
/// and uses [`static_tool_providers`] directly instead. Walks the same
/// shape [`static_tool_providers`] walks over raw YAML: every `LeafKind::
/// Tool` provider, recursing into every control region and a
/// reflector's own inner effects, plus a literal `use: inline:` leaf's
/// own text (best-effort re-parsed) -- never a `path:`/`orchestration:`
/// leaf, a separate file judged on its own.
pub fn static_tool_providers_from_op(op: &electricity_bytecode::Op) -> BTreeSet<String> {
    let mut providers = BTreeSet::new();
    walk_op_tool_providers(op, &mut providers);
    providers
}

fn walk_op_tool_providers(op: &electricity_bytecode::Op, providers: &mut BTreeSet<String>) {
    use electricity_bytecode::{LeafKind, NodeKind, UseSource};
    match &op.kind {
        NodeKind::Leaf(leaf) => match leaf.as_ref() {
            LeafKind::Tool(tool_op) => {
                providers.insert(tool_op.provider.clone());
            }
            LeafKind::Use(use_op) => {
                if let UseSource::Inline(text) = &use_op.source {
                    if let Some(parsed) = best_effort_yaml(&text.source) {
                        if let Some(parsed) = parsed.as_dict() {
                            walk_tool_providers(effects_of(parsed), providers);
                            walk_tool_providers(get(parsed, "finally"), providers);
                        }
                    }
                }
            }
            LeafKind::Reflector(reflector_op) => {
                walk_region_tool_providers(&reflector_op.inner, providers);
            }
            LeafKind::Prompt(_) | LeafKind::Yield(_) | LeafKind::Report(_) => {}
        },
        NodeKind::Control(region) => walk_region_tool_providers(region, providers),
    }
}

fn walk_region_tool_providers(
    region: &electricity_bytecode::Region,
    providers: &mut BTreeSet<String>,
) {
    use electricity_bytecode::Region;
    match region {
        Region::Block { ops, .. } => {
            for op in ops {
                walk_op_tool_providers(op, providers);
            }
        }
        Region::Parallel { branches, .. } => {
            for op in branches {
                walk_op_tool_providers(op, providers);
            }
        }
        Region::If { then_, else_, .. } => {
            walk_region_tool_providers(then_, providers);
            if let Some(else_) = else_ {
                walk_region_tool_providers(else_, providers);
            }
        }
        Region::Loop { body, .. } => walk_region_tool_providers(body, providers),
        Region::TryFinally { body, finally } => {
            walk_region_tool_providers(body, providers);
            walk_region_tool_providers(finally, providers);
        }
    }
}

/// `circuitry.generated_doc._same_shape` -- whether *a* (parsed from the
/// unrendered template) and *b* (the rendered, final document) agree on
/// structure at this node and below. A scalar is always compatible with
/// a scalar; a dict/list is compatible only with a dict/list of the
/// same keys/length, recursively.
fn same_shape(a: &Value, b: &Value) -> bool {
    match (a.as_dict(), b.as_dict()) {
        (Some(_), None) | (None, Some(_)) => return false,
        (Some(a_dict), Some(b_dict)) => {
            let a_keys: BTreeSet<&str> = a_dict.keys().filter_map(Value::as_str).collect();
            let b_keys: BTreeSet<&str> = b_dict.keys().filter_map(Value::as_str).collect();
            if a_dict.len() != a_keys.len() || b_dict.len() != b_keys.len() || a_keys != b_keys {
                return false;
            }
            return a_dict.iter().all(|(key, a_value)| {
                b_dict
                    .get(key)
                    .is_some_and(|b_value| same_shape(a_value, b_value))
            });
        }
        (None, None) => {}
    }
    match (a.as_list(), b.as_list()) {
        (Some(_), None) | (None, Some(_)) => false,
        (Some(a_list), Some(b_list)) => {
            a_list.len() == b_list.len()
                && a_list
                    .iter()
                    .zip(b_list.iter())
                    .all(|(x, y)| same_shape(x, y))
        }
        (None, None) => true,
    }
}

/// `circuitry.generated_doc.is_generated_inline` -- whether a `use:
/// inline:` child counts as generated (#772). `False` means the
/// unrendered template already has the rendered document's exact shape
/// (only scalar values differ): the author's own content, with values
/// templated in. `True` -- including when the unrendered text does not
/// parse as YAML at all, the common case for a template that is just
/// one `{{{...}}}` tag -- means some or all of this document's
/// structure only exists because it was rendered.
pub fn is_generated_inline(raw_template: &str, rendered: &Value) -> bool {
    match best_effort_yaml(raw_template) {
        Some(raw_parsed) if raw_parsed.as_dict().is_some() => !same_shape(&raw_parsed, rendered),
        _ => true,
    }
}

fn as_bool(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::Str(s)) => matches!(
            s.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Some(Value::None) => false,
        Some(_) => true,
        None => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_generated_errors(
    effects: Option<&Value>,
    path: &str,
    allowed_providers: Option<&BTreeSet<String>>,
    known_providers: Option<&BTreeSet<String>>,
    generating_effect_path: &str,
    errors: &mut Vec<String>,
) {
    let Some(Value::List(effects)) = effects else {
        return;
    };
    for effect in effects {
        let Some(effect) = effect.as_dict() else {
            continue;
        };
        let etype = get(effect, "type").and_then(Value::as_str).unwrap_or("");
        let name = get(effect, "name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("<unnamed>");
        let effect_label = if path.is_empty() {
            name.to_string()
        } else {
            format!("{path}.{name}")
        };
        if etype == "tool" {
            let provider_name = get(effect, "provider")
                .and_then(Value::as_str)
                .map(str::trim);
            let empty_params = Dict::new();
            let params = get(effect, "params")
                .and_then(Value::as_dict)
                .unwrap_or(&empty_params);
            if provider_name == Some("shell")
                && params.contains_key(&Value::Str("allowed_commands".to_string()))
            {
                errors.push(format!(
                    "generated effect '{effect_label}' sets params.allowed_commands on a \
                     'shell' tool, generated by {generating_effect_path}; a document \
                     generated at run time can never widen the shell allowlist this way."
                ));
            } else if provider_name == Some("service") && as_bool(get(params, "shell")) {
                errors.push(format!(
                    "generated effect '{effect_label}' sets params.shell: true on a \
                     'service' tool, generated by {generating_effect_path}; a document \
                     generated at run time can never do that."
                ));
            } else if let Some(provider_name) = provider_name {
                // A provider that isn't even a known plugin fails the usual way (an
                // "Unknown plugin" error at dispatch) instead of this one -- adding an
                // unknown name to allow_tools would itself be a check error, so this
                // refusal is only for a real plugin the document that generated it
                // simply doesn't use.
                let is_known = known_providers.is_none_or(|known| known.contains(provider_name));
                if let Some(allowed) = allowed_providers {
                    if is_known && !allowed.contains(provider_name) {
                        errors.push(format!(
                            "generated effect '{effect_label}' uses tool provider \
                             '{provider_name}', which the document that generated it \
                             doesn't use; add '{provider_name}' to allow_tools on \
                             {generating_effect_path} to allow it."
                        ));
                    }
                }
            }
        } else if etype == "dynamic" {
            walk_generated_errors(
                effects_of(effect),
                &effect_label,
                allowed_providers,
                known_providers,
                generating_effect_path,
                errors,
            );
            walk_generated_errors(
                get(effect, "finally"),
                &effect_label,
                allowed_providers,
                known_providers,
                generating_effect_path,
                errors,
            );
        } else if etype == "if" || etype == "conditional" {
            walk_generated_errors(
                get(effect, "then"),
                &effect_label,
                allowed_providers,
                known_providers,
                generating_effect_path,
                errors,
            );
            walk_generated_errors(
                get(effect, "else"),
                &effect_label,
                allowed_providers,
                known_providers,
                generating_effect_path,
                errors,
            );
        } else if etype == "loop" {
            walk_generated_errors(
                get(effect, "body"),
                &effect_label,
                allowed_providers,
                known_providers,
                generating_effect_path,
                errors,
            );
        } else if etype == "reflector" {
            walk_generated_errors(
                effects_of(effect),
                &effect_label,
                allowed_providers,
                known_providers,
                generating_effect_path,
                errors,
            );
        } else if etype == "use" {
            if let Some(inline) = get(effect, "inline").and_then(Value::as_str) {
                if let Some(parsed) = best_effort_yaml(inline) {
                    if let Some(parsed) = parsed.as_dict() {
                        walk_generated_errors(
                            effects_of(parsed),
                            &effect_label,
                            allowed_providers,
                            known_providers,
                            generating_effect_path,
                            errors,
                        );
                        walk_generated_errors(
                            get(parsed, "finally"),
                            &effect_label,
                            allowed_providers,
                            known_providers,
                            generating_effect_path,
                            errors,
                        );
                    }
                }
            }
        }
    }
}

/// `circuitry.generated_doc.generated_document_errors` -- every rule a
/// generated document violates (#772); empty means clean. `None` for
/// *allowed_providers* skips the provider check (rule 3) while still
/// enforcing the shell-specific rules (rule 2) -- a decomposition plan's
/// case. *generating_effect_path* is the dotted state path of the
/// `use`/`reflector` effect that produced this document, named in the
/// provider-outside-the-allowed-set message so the fix is actionable;
/// ignored when *allowed_providers* is `None`.
pub fn generated_document_errors(
    orch: &Value,
    allowed_providers: Option<&BTreeSet<String>>,
    generating_effect_path: &str,
) -> Vec<String> {
    let mut errors = Vec::new();
    let known_providers: Option<BTreeSet<String>> = allowed_providers.map(|_| {
        electricity_tools::registry::supported_names()
            .into_iter()
            .collect()
    });
    if let Some(dict) = orch.as_dict() {
        walk_generated_errors(
            effects_of(dict),
            "",
            allowed_providers,
            known_providers.as_ref(),
            generating_effect_path,
            &mut errors,
        );
        walk_generated_errors(
            get(dict, "finally"),
            "",
            allowed_providers,
            known_providers.as_ref(),
            generating_effect_path,
            &mut errors,
        );
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_yaml::load_yaml;

    fn parse(text: &str) -> Value {
        load_yaml(text).expect("valid yaml")
    }

    #[test]
    fn whole_template_as_one_tag_is_generated() {
        let rendered = parse("effects:\n  - type: tool\n    name: t\n    provider: shell\n");
        assert!(is_generated_inline("{{{prime.gen.value}}}", &rendered));
    }

    #[test]
    fn literal_structure_with_templated_scalar_is_not_generated() {
        let raw = "effects:\n  - type: tool\n    name: step1\n    provider: shell\n    params:\n      command: echo\n      args: ['{{msg}}']\n";
        let rendered = parse(
            "effects:\n  - type: tool\n    name: step1\n    provider: shell\n    params:\n      command: echo\n      args: ['hello']\n",
        );
        assert!(!is_generated_inline(raw, &rendered));
    }

    #[test]
    fn rendered_structure_with_an_extra_effect_is_generated() {
        let raw = "effects:\n  - type: prompt\n    name: a\n    template: hi\n";
        let rendered = parse(
            "effects:\n  - type: prompt\n    name: a\n    template: hi\n  - type: tool\n    name: b\n    provider: shell\n",
        );
        assert!(is_generated_inline(raw, &rendered));
    }

    #[test]
    fn static_tool_providers_collects_direct_and_nested_inline() {
        let orch = parse(
            "effects:\n  - type: tool\n    name: a\n    provider: shell\n  - type: use\n    name: child\n    inline: |\n      effects:\n        - type: tool\n          name: x\n          provider: docker\n",
        );
        let providers = static_tool_providers(&orch);
        assert_eq!(
            providers,
            BTreeSet::from(["shell".to_string(), "docker".to_string()])
        );
    }

    #[test]
    fn allowed_commands_on_generated_shell_effect_is_always_refused() {
        let orch = parse(
            "effects:\n  - type: tool\n    name: t\n    provider: shell\n    params:\n      command: touch\n      allowed_commands: [touch]\n",
        );
        let allowed = BTreeSet::from(["shell".to_string()]);
        let errors = generated_document_errors(&orch, Some(&allowed), "prime.plan_it");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("allowed_commands"));
        assert!(errors[0].contains("prime.plan_it"));
    }

    #[test]
    fn service_shell_true_is_always_refused() {
        let orch = parse(
            "effects:\n  - type: tool\n    name: t\n    provider: service\n    params:\n      command: touch x\n      shell: true\n",
        );
        let allowed = BTreeSet::from(["service".to_string()]);
        let errors = generated_document_errors(&orch, Some(&allowed), "prime.plan_it");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("shell: true"));
        assert!(errors[0].contains("prime.plan_it"));
    }

    #[test]
    fn provider_outside_the_allowed_set_is_refused() {
        let orch = parse("effects:\n  - type: tool\n    name: t\n    provider: awk\n");
        let allowed = BTreeSet::from(["shell".to_string()]);
        let errors = generated_document_errors(&orch, Some(&allowed), "prime.plan_it");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("awk"));
        assert!(errors[0].contains("allow_tools"));
        assert!(errors[0].contains("prime.plan_it"));
        assert!(!errors[0].contains("ceiling"));
    }

    #[test]
    fn an_unknown_provider_is_not_flagged_here() {
        // A provider that isn't a real plugin at all fails the usual way
        // ("Unknown plugin") at dispatch, never this check -- adding an
        // unknown name to allow_tools would itself be a check error.
        let orch = parse("effects:\n  - type: tool\n    name: t\n    provider: nonexistent_tool\n");
        let allowed = BTreeSet::from(["shell".to_string()]);
        assert!(generated_document_errors(&orch, Some(&allowed), "prime.plan_it").is_empty());
    }

    #[test]
    fn provider_within_the_allowed_set_is_clean() {
        let orch = parse(
            "effects:\n  - type: tool\n    name: t\n    provider: shell\n    params:\n      command: echo\n",
        );
        let allowed = BTreeSet::from(["shell".to_string()]);
        assert!(generated_document_errors(&orch, Some(&allowed), "prime.plan_it").is_empty());
    }

    #[test]
    fn no_allowed_set_skips_the_provider_check_but_not_shell_widening() {
        let orch = parse(
            "effects:\n  - type: tool\n    name: t\n    provider: shell\n    params:\n      command: touch\n      allowed_commands: [touch]\n",
        );
        let errors = generated_document_errors(&orch, None, "");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("allowed_commands"));
    }
}
