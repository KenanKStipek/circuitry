//! `cli/allowlist.py`'s own static `enabled_adapters`/`enabled_tools`
//! enforcement, and the two `runtime_config` entries `cli/runtime_shim.
//! py::run` installs from a resolved [`CircuitryConfig`]
//! (`allowlist_gate.py::install_allowlists`, and the run's own
//! `_capability_allow`) -- the seam issue #431's lane table leaves for
//! this lane to shape, documented in `electricity/docs/spec/
//! vm-lanes.md`.
//!
//! **The shape picked.** [`check_allowlist`] walks *only* the document's
//! own top-level `adapter:`/tool-`provider:`/prompt-`provider:` text --
//! not `use:` children (`cli/allowlist.py::check_allowlist`'s own
//! `iter_use_children` loop). `use` is unsupported content in M0-H
//! (issue #431's Scope section) and is refused outright once the
//! structural/compile checks run; a `use` child's own adapter/tool
//! references are therefore never reachable through *any* M0-H run, so
//! walking them here would check text no run can ever act on. The one
//! observable narrowing from Circuitry's own ordering: a document whose
//! *only* violation is inside a `use` child's provider (the parent
//! document's own text is otherwise clean) is rejected by the later
//! "is a preview and cannot run orchestrations yet" refusal instead of
//! this allowlist's own denial message -- still refused, with a
//! different, less specific reason. [`walk_orchestration_refs`] still
//! ports every other branch of Circuitry's own walk (`prompt`/`tool`/
//! `dynamic`/`if`/`conditional`/`loop`/`reflector`), even where M0-H's
//! own VM never executes one (`loop`/`reflector`/`prompt` effects are
//! themselves refused) -- this function only reads document *text*, so
//! there is no extra cost to keeping it complete and no VM-side
//! behaviour it could get out of sync with.

use crate::config::CircuitryConfig;
use electricity_value::{Dict, Value};
use std::collections::BTreeSet;

/// `allowlist_gate.py::adapter_denial`.
pub fn adapter_denial(name: &str, allowed: Option<&[String]>) -> Option<String> {
    match allowed {
        None => None,
        Some(list) if list.iter().any(|a| a == name) => None,
        Some(list) => Some(format!(
            "adapter '{name}' not in enabled_adapters allowlist (enabled: {})",
            format_list(list)
        )),
    }
}

/// `allowlist_gate.py::tool_denial`.
pub fn tool_denial(name: &str, allowed: Option<&[String]>) -> Option<String> {
    match allowed {
        None => None,
        Some(list) if list.iter().any(|a| a == name) => None,
        Some(list) => Some(format!(
            "tool '{name}' not in enabled_tools allowlist (enabled: {})",
            format_list(list)
        )),
    }
}

/// Python `repr(list_of_str)` -- `allowed`'s own `{enabled!r}`-style
/// interpolation (`f"... (enabled: {allowed})"`, where `allowed` is a
/// Python `list[str]`, formatted with `str()`, which for a list defers
/// to each element's own `repr()`).
fn format_list(items: &[String]) -> String {
    let inner = items
        .iter()
        .map(|s| Value::Str(s.clone()).py_repr())
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{inner}]")
}

/// `allowlist.py::_provider_token_to_adapter`.
fn provider_token_to_adapter(token: &str) -> Option<String> {
    let parsed = token.trim();
    if parsed.is_empty() {
        return None;
    }
    match parsed.split_once(':') {
        Some((head, _)) => {
            let head = head.trim();
            if head.is_empty() {
                None
            } else {
                Some(head.to_string())
            }
        }
        None => Some(parsed.to_string()),
    }
}

fn get<'a>(dict: &'a Dict, key: &str) -> Option<&'a Value> {
    dict.get(&Value::Str(key.to_string()))
}

fn effect_list<'a>(effect: &'a Dict, key: &str) -> Option<&'a [Value]> {
    get(effect, key).and_then(Value::as_list)
}

/// `allowlist.py::_walk_effects`.
fn walk_effects(
    effects: Option<&Value>,
    adapters: &mut BTreeSet<String>,
    tools: &mut BTreeSet<String>,
) {
    let Some(Value::List(effects)) = effects else {
        return;
    };
    for effect in effects {
        let Some(effect) = effect.as_dict() else {
            continue;
        };
        let etype = get(effect, "type").and_then(Value::as_str).unwrap_or("");
        match etype {
            "prompt" => {
                if let Some(primary) = get(effect, "provider").and_then(Value::as_str) {
                    if let Some(name) = provider_token_to_adapter(primary) {
                        adapters.insert(name);
                    }
                }
                for token in effect_list(effect, "provider_fallbacks").unwrap_or(&[]) {
                    if let Some(token) = token.as_str() {
                        if let Some(name) = provider_token_to_adapter(token) {
                            adapters.insert(name);
                        }
                    }
                }
            }
            "tool" => {
                if let Some(provider) = get(effect, "provider").and_then(Value::as_str) {
                    let provider = provider.trim();
                    if !provider.is_empty() {
                        tools.insert(provider.to_string());
                    }
                }
            }
            "dynamic" => {
                let children = get(effect, "effects").or_else(|| get(effect, "steps"));
                walk_effects(children, adapters, tools);
                walk_effects(get(effect, "finally"), adapters, tools);
            }
            "if" | "conditional" => {
                walk_effects(get(effect, "then"), adapters, tools);
                walk_effects(get(effect, "else"), adapters, tools);
            }
            "loop" => {
                walk_effects(get(effect, "body"), adapters, tools);
            }
            "reflector" => {
                let children = get(effect, "effects").or_else(|| get(effect, "steps"));
                walk_effects(children, adapters, tools);
            }
            _ => {}
        }
    }
}

/// `allowlist.py::walk_orchestration_refs`.
pub fn walk_orchestration_refs(
    document: &Value,
    include_document_adapter: bool,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut adapters = BTreeSet::new();
    let mut tools = BTreeSet::new();
    if let Some(orch) = document.as_dict() {
        if include_document_adapter {
            if let Some(top_adapter) = get(orch, "adapter").and_then(Value::as_str) {
                let top_adapter = top_adapter.trim();
                if !top_adapter.is_empty() {
                    adapters.insert(top_adapter.to_string());
                }
            }
        }
        let effects = get(orch, "effects").or_else(|| get(orch, "steps"));
        walk_effects(effects, &mut adapters, &mut tools);
        walk_effects(get(orch, "finally"), &mut adapters, &mut tools);
    }
    (adapters, tools)
}

/// `allowlist.py::orchestration_denials`, narrowed to a top-level
/// document (no `use` walk -- see this module's own doc comment).
fn orchestration_denials(
    document: &Value,
    enabled_adapters: Option<&[String]>,
    enabled_tools: Option<&[String]>,
) -> Vec<String> {
    let (adapter_refs, tool_refs) = walk_orchestration_refs(document, true);
    let mut errors: Vec<String> = adapter_refs
        .iter()
        .filter_map(|name| adapter_denial(name, enabled_adapters))
        .collect();
    errors.extend(
        tool_refs
            .iter()
            .filter_map(|name| tool_denial(name, enabled_tools)),
    );
    errors
}

/// `allowlist.py::check_allowlist`, narrowed as this module's own doc
/// comment describes.
pub fn check_allowlist(document: &Value, config: &CircuitryConfig) -> Vec<String> {
    orchestration_denials(
        document,
        config.enabled_adapters.as_deref(),
        config.enabled_tools.as_deref(),
    )
}

/// `allowlist_gate.py::ALLOWLISTS_KEY`'s own value shape
/// (`{"adapters": [...] | None, "tools": [...] | None}`) --
/// `install_allowlists`'s own run-time write, here as a typed pair
/// issue #431's lane D (run wiring) installs into its own
/// `runtime_config` bag.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Allowlists {
    pub adapters: Option<Vec<String>>,
    pub tools: Option<Vec<String>>,
}

/// `allowlist_gate.py::install_allowlists`'s own resolved value, read
/// off a [`CircuitryConfig`] directly (electricity has no separate
/// `enabled_adapters`/`enabled_tools` CLI override to rank above it).
pub fn allowlists(config: &CircuitryConfig) -> Allowlists {
    Allowlists {
        adapters: config.enabled_adapters.clone(),
        tools: config.enabled_tools.clone(),
    }
}

/// `runtime_config["_capability_allow"]`: always empty for electricity
/// -- there is no `--allow-capabilities` flag in M0-H's own CLI usage
/// (issue #431's "CLI" section), so there is nothing a document's own
/// capability-consent gate could ever be pre-approved with. A function,
/// not a bare `Vec::new()` at the call site, so issue #431's lane D
/// (run wiring) has one named seam to widen if a later milestone adds
/// the flag, instead of a silent empty literal scattered at the call
/// site.
pub fn capability_allow() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_from_pairs(pairs: Vec<(&str, Value)>) -> Value {
        let mut dict = Dict::new();
        for (k, v) in pairs {
            dict.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(dict)
    }

    #[test]
    fn a_default_open_allowlist_denies_nothing() {
        assert_eq!(adapter_denial("openai", None), None);
        assert_eq!(tool_denial("shell", None), None);
    }

    #[test]
    fn a_strict_allowlist_denies_an_unlisted_name() {
        let allowed = vec!["ollama".to_string()];
        let denial = adapter_denial("openai", Some(&allowed)).unwrap();
        assert_eq!(
            denial,
            "adapter 'openai' not in enabled_adapters allowlist (enabled: ['ollama'])"
        );
    }

    #[test]
    fn walk_collects_a_tool_effects_provider() {
        let mut tool_effect = Dict::new();
        tool_effect.insert(Value::Str("type".to_string()), Value::from("tool"));
        tool_effect.insert(Value::Str("provider".to_string()), Value::from("json"));
        let document = doc_from_pairs(vec![(
            "effects",
            Value::List(vec![Value::Dict(tool_effect)]),
        )]);
        let (adapters, tools) = walk_orchestration_refs(&document, true);
        assert!(adapters.is_empty());
        assert_eq!(tools, BTreeSet::from(["json".to_string()]));
    }

    #[test]
    fn walk_recurses_through_dynamic_if_and_loop() {
        let mut inner_tool = Dict::new();
        inner_tool.insert(Value::Str("type".to_string()), Value::from("tool"));
        inner_tool.insert(Value::Str("provider".to_string()), Value::from("shell"));
        let mut if_effect = Dict::new();
        if_effect.insert(Value::Str("type".to_string()), Value::from("if"));
        if_effect.insert(
            Value::Str("then".to_string()),
            Value::List(vec![Value::Dict(inner_tool)]),
        );
        let mut dynamic = Dict::new();
        dynamic.insert(Value::Str("type".to_string()), Value::from("dynamic"));
        dynamic.insert(
            Value::Str("effects".to_string()),
            Value::List(vec![Value::Dict(if_effect)]),
        );
        let document = doc_from_pairs(vec![("effects", Value::List(vec![Value::Dict(dynamic)]))]);
        let (_, tools) = walk_orchestration_refs(&document, true);
        assert_eq!(tools, BTreeSet::from(["shell".to_string()]));
    }

    #[test]
    fn check_allowlist_denies_an_unlisted_tool() {
        let mut tool_effect = Dict::new();
        tool_effect.insert(Value::Str("type".to_string()), Value::from("tool"));
        tool_effect.insert(Value::Str("provider".to_string()), Value::from("shell"));
        let document = doc_from_pairs(vec![(
            "effects",
            Value::List(vec![Value::Dict(tool_effect)]),
        )]);
        let config = CircuitryConfig {
            enabled_tools: Some(vec!["json".to_string()]),
            ..CircuitryConfig::default()
        };
        let errors = check_allowlist(&document, &config);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("tool 'shell'"));
    }

    #[test]
    fn capability_allow_is_always_empty() {
        assert_eq!(capability_allow(), Vec::<String>::new());
    }
}
