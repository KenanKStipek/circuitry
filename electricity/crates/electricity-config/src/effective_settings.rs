//! `cli/effective_settings.py::resolve_effective_settings`, narrowed to
//! the tiers electricity's own CLI has: there is no `--model`/
//! `--adapter`/`--plugins`/`--scoring`/`--routing`/`--decompose` flag,
//! no profile, and no `--resume` (`out` has no electricity-side
//! equivalent either: `--out` is a run-level CLI flag, not part of the
//! config/document merge at all) -- so the `cli`/`profile`/`resume`
//! precedence tiers `resolve_effective_settings` has are simply never
//! populated here, and every call is as if every one of its optional
//! parameters were left at its default.
//!
//! The one precedence tier that *is* always populated: `trust_document`
//! is always `true`. Every document electricity runs is named by path
//! on its own command line (`electricity <config.json> <doc>`), exactly
//! `cof run ./my.yml`'s own case (`cli/app.py::_names_a_file`) -- there
//! is no library-name/remote-source run for electricity to treat
//! differently (`effective_document_trust`'s own cache-path override
//! has no electricity-side library registry to narrow against either).
//! So this module only ever takes `resolve_effective_settings`'s
//! trusted branch: `_split_orchestration_runtime`/
//! `_split_orchestration_plugins`'s untrusted branches (dropped keys,
//! their own warnings) are dead code here and are not ported.

use crate::complexity::{RoutingSettings, resolve_complexity_settings};
use crate::config::{CircuitryConfig, ConfigError};
use crate::merge::merge_runtime;
use crate::util::get;
use electricity_value::{Dict, Value};
use indexmap::IndexMap;
use std::collections::HashSet;
use std::path::PathBuf;

/// `cli/effective_settings.py::EffectiveSettings`, narrowed to the
/// fields this module's own callers need: no `complexity`/`model_locked`
/// fields of their own (M0-H runs no prompt effect, so nothing routes a
/// model or reads the resolved band table -- [`resolve_complexity_
/// settings`] still *validates* the block, and still feeds the model-
/// routing precedence below, even though its own typed result is
/// discarded once validation passes).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectiveSettings {
    pub model: Option<String>,
    pub adapter: Option<String>,
    /// Always `None`: `--out` has no config/document-merge precedence
    /// tier of its own in Circuitry either (see this module's own doc
    /// comment) -- kept as a field, not dropped, since issue #431's
    /// lane D (run wiring) still needs somewhere to read a resolved
    /// `out` from once it has its own `--out` CLI flag to rank above
    /// it.
    pub out: Option<PathBuf>,
    pub plugins: Vec<Value>,
    pub runtime: Option<Value>,
    /// Dotted-settings-path -> provenance label
    /// (`"config"`/`"orchestration"`/`"default"`/`"router"`), in the
    /// exact insertion order `resolve_effective_settings` builds it --
    /// `--out`'s own `runtime.effective_settings.sources` writes this
    /// in that order.
    pub sources: IndexMap<String, String>,
    pub warnings: Vec<String>,
}

/// `ORCHESTRATION_RUNTIME_KEYS`.
const ORCHESTRATION_RUNTIME_KEYS: [&str; 2] = ["complexity", "state"];
/// `_NOTICE_KEY_DEPTH`.
const NOTICE_KEY_DEPTH: usize = 2;
/// `_CEILING_LIST_KEYS`.
const CEILING_LIST_KEYS: [(&str, &str, &str); 1] = [("plugins", "shell", "allowed_commands")];

fn dict_truthy(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_dict)
        .is_some_and(|d| !d.is_empty())
}

/// `_key_label`: `str(key)`, `repr()`'d instead when it is not
/// "printable" -- approximated here as "contains no control character",
/// not CPython's full `str.isprintable()` Unicode-category check (every
/// reachable key in practice is an ordinary YAML/JSON scalar, where the
/// two agree).
fn key_label(key: &Value) -> String {
    let text = key.py_str();
    if text.chars().any(|c| c.is_control()) {
        key.py_repr()
    } else {
        text
    }
}

/// `_key_paths`.
fn key_paths(prefix: &str, value: &Value, depth: usize) -> Vec<String> {
    if depth == 0 {
        return vec![prefix.to_string()];
    }
    match value.as_dict() {
        Some(dict) if !dict.is_empty() => dict
            .iter()
            .flat_map(|(key, sub)| {
                key_paths(&format!("{prefix}.{}", key_label(key)), sub, depth - 1)
            })
            .collect(),
        _ => vec![prefix.to_string()],
    }
}

/// `_ceiling_intersected_paths`.
fn ceiling_intersected_paths(cfg: &CircuitryConfig) -> Vec<String> {
    let Some(config_runtime) = cfg.runtime.as_ref().and_then(Value::as_dict) else {
        return Vec::new();
    };
    CEILING_LIST_KEYS
        .iter()
        .filter_map(|(top_key, name, leaf_key)| {
            let host_block = get(config_runtime, top_key)?.as_dict()?;
            let host_cfg = get(host_block, name)?.as_dict()?;
            let host_list = get(host_cfg, leaf_key)?;
            matches!(host_list, Value::List(_))
                .then(|| format!("runtime.{top_key}.{name}.{leaf_key}"))
        })
        .collect()
}

/// `_applied_host_settings_notice`.
fn applied_host_settings_notice(
    orch_runtime: &Dict,
    orch_plugins: &[Value],
    cfg: &CircuitryConfig,
    document_name: Option<&str>,
) -> Vec<String> {
    let ceiling_paths = ceiling_intersected_paths(cfg);
    let mut runtime_paths = Vec::new();
    for (key, value) in orch_runtime {
        let key_str = key.as_str();
        if key_str.is_some_and(|k| ORCHESTRATION_RUNTIME_KEYS.contains(&k)) {
            continue;
        }
        for path in key_paths(
            &format!("runtime.{}", key_label(key)),
            value,
            NOTICE_KEY_DEPTH,
        ) {
            if ceiling_paths.contains(&path) {
                runtime_paths.push(format!("{path} (intersected with host pin)"));
            } else {
                runtime_paths.push(path);
            }
        }
    }

    let host_listed: HashSet<&str> = cfg
        .plugins
        .iter()
        .filter_map(Value::as_str)
        .chain(cfg.enabled_plugins.iter().flatten().map(String::as_str))
        .collect();
    let plugin_ids: Vec<String> = orch_plugins
        .iter()
        .filter_map(Value::as_str)
        .filter(|p| !host_listed.contains(p))
        .map(str::to_string)
        .collect();

    if runtime_paths.is_empty() && plugin_ids.is_empty() {
        return Vec::new();
    }
    let mut parts = runtime_paths;
    if !plugin_ids.is_empty() {
        parts.push(format!("plugins: {}", plugin_ids.join(", ")));
    }
    let source = document_name.unwrap_or("the orchestration");
    vec![format!(
        "Applied host settings from {source}: {}",
        parts.join(", ")
    )]
}

/// `orch.get("plugins") or []`, validated: truthy and not a list ->
/// `"Orchestration 'plugins' must be a list if provided."`.
fn orchestration_plugins(doc_dict: &Dict) -> Result<Vec<Value>, ConfigError> {
    match get(doc_dict, "plugins") {
        None => Ok(Vec::new()),
        Some(value) if is_falsy(value) => Ok(Vec::new()),
        Some(Value::List(items)) => Ok(items.clone()),
        Some(_) => Err(ConfigError(
            "Orchestration 'plugins' must be a list if provided.".to_string(),
        )),
    }
}

/// `orch.get("runtime") or {}`, validated: truthy and not an object ->
/// `"Orchestration 'runtime' must be an object if provided."`.
fn orchestration_runtime(doc_dict: &Dict) -> Result<Dict, ConfigError> {
    match get(doc_dict, "runtime") {
        None => Ok(Dict::new()),
        Some(value) if is_falsy(value) => Ok(Dict::new()),
        Some(Value::Dict(dict)) => Ok(dict.clone()),
        Some(_) => Err(ConfigError(
            "Orchestration 'runtime' must be an object if provided.".to_string(),
        )),
    }
}

/// Python truthiness for the handful of shapes `or []`/`or {}` can see
/// here (an empty list/dict/string/zero/`False`/`None` -- `None` is
/// handled by its caller already).
fn is_falsy(value: &Value) -> bool {
    match value {
        Value::None => true,
        Value::Bool(b) => !b,
        Value::Int(i) => i.is_zero(),
        Value::Float(f) => *f == 0.0,
        Value::Str(s) => s.is_empty(),
        Value::Bytes(b) => b.is_empty(),
        Value::List(items) => items.is_empty(),
        Value::Dict(d) => d.is_empty(),
        Value::Date(_) | Value::DateTime(..) => false,
    }
}

fn record_complexity_sources(
    sources: &mut IndexMap<String, String>,
    config_block: Option<&Value>,
    orch_block: Option<&Value>,
) {
    let empty = Dict::new();
    let (complexity_source, winner): (&str, &Dict) = if let Some(Value::Dict(d)) = orch_block {
        ("orchestration", d)
    } else if let Some(Value::Dict(d)) = config_block {
        ("config", d)
    } else {
        ("default", &empty)
    };
    sources.insert("complexity".to_string(), complexity_source.to_string());

    for key in ["scoring", "routing", "decomposition"] {
        let present = get(winner, key).is_some();
        sources.insert(
            format!("complexity.{key}"),
            if present {
                complexity_source
            } else {
                "default"
            }
            .to_string(),
        );
    }

    let field_source = |sub_key: &str, field_key: &str| -> String {
        let has_field = get(winner, sub_key)
            .and_then(Value::as_dict)
            .and_then(|sub| get(sub, field_key))
            .is_some_and(|v| !matches!(v, Value::None));
        if has_field {
            complexity_source
        } else {
            "default"
        }
        .to_string()
    };
    sources.insert(
        "complexity.routing.bands".to_string(),
        field_source("routing", "bands"),
    );
    sources.insert(
        "complexity.decomposition.threshold".to_string(),
        field_source("decomposition", "threshold"),
    );
    sources.insert(
        "complexity.decomposition.max_depth".to_string(),
        field_source("decomposition", "max_depth"),
    );
    sources.insert(
        "complexity.decomposition.on_failure".to_string(),
        field_source("decomposition", "on_failure"),
    );
}

/// `_apply_router_precedence`, with `model_locked` always `false` (there
/// is no `cli`/`profile` tier here for `sources["model"]` to ever equal,
/// so the "respect an explicit pin" branch never applies).
fn apply_router_precedence(
    sources: &mut IndexMap<String, String>,
    model: Option<String>,
    routing: &RoutingSettings,
) -> Option<String> {
    if !routing.enabled || routing.bands.is_empty() {
        return model;
    }
    sources.insert("model".to_string(), "router".to_string());
    // Validation guarantees a non-empty table ends in the catch-all.
    model.or_else(|| routing.bands.last().map(|band| band.model.clone()))
}

/// `resolve_effective_settings`, as narrowed by this module's own doc
/// comment: merges `config` > `document` > built-in default for a
/// single run, recording `sources` and `warnings` the same way.
pub fn effective_settings(
    config: &CircuitryConfig,
    document: &Value,
) -> Result<EffectiveSettings, ConfigError> {
    let empty = Dict::new();
    let doc_dict = document.as_dict().unwrap_or(&empty);
    let mut sources: IndexMap<String, String> = IndexMap::new();
    let mut warnings: Vec<String> = Vec::new();

    // model: orchestration > config > default (no `cli`/`profile` tier).
    let mut model = match get(doc_dict, "model") {
        Some(value) if !matches!(value, Value::None) => {
            sources.insert("model".to_string(), "orchestration".to_string());
            Some(value.py_str())
        }
        _ => match &config.default_model {
            Some(value) => {
                sources.insert("model".to_string(), "config".to_string());
                Some(value.clone())
            }
            None => {
                sources.insert("model".to_string(), "default".to_string());
                None
            }
        },
    };

    // adapter: orchestration > config > default.
    let adapter = match get(doc_dict, "adapter") {
        Some(value) if !matches!(value, Value::None) => {
            sources.insert("adapter".to_string(), "orchestration".to_string());
            Some(value.py_str())
        }
        _ => match &config.default_adapter {
            Some(value) => {
                sources.insert("adapter".to_string(), "config".to_string());
                Some(value.clone())
            }
            None => {
                sources.insert("adapter".to_string(), "default".to_string());
                None
            }
        },
    };

    // `out` has no config/document tier of its own (this module's own
    // doc comment); always the "default" (no file written) entry.
    sources.insert("out".to_string(), "default".to_string());

    let orch_plugins = orchestration_plugins(doc_dict)?;
    let orch_runtime = orchestration_runtime(doc_dict)?;

    // Every document electricity runs is trusted (this module's own doc
    // comment) -- the "Applied host settings" notice, never the dropped-
    // key warnings an untrusted document would add.
    warnings.extend(applied_host_settings_notice(
        &orch_runtime,
        &orch_plugins,
        config,
        None,
    ));

    // plugins: config's own list, then the orchestration's (trusted, so
    // kept whole), deduplicated in that order; every entry must be a
    // string.
    let mut seen: HashSet<String> = HashSet::new();
    let mut plugins: Vec<Value> = Vec::new();
    for candidate in config.plugins.iter().chain(orch_plugins.iter()) {
        let Some(text) = candidate.as_str() else {
            return Err(ConfigError("Plugins must be strings.".to_string()));
        };
        if seen.insert(text.to_string()) {
            plugins.push(candidate.clone());
        }
    }
    sources.insert(
        "plugins".to_string(),
        if !orch_plugins.is_empty() {
            "orchestration"
        } else if !config.plugins.is_empty() {
            "config"
        } else {
            "default"
        }
        .to_string(),
    );

    // runtime: config's own block deep-merged with the orchestration's
    // (trusted, so kept whole).
    let orch_runtime_value = Value::Dict(orch_runtime.clone());
    let runtime = merge_runtime(config.runtime.as_ref(), Some(&orch_runtime_value));
    sources.insert(
        "runtime".to_string(),
        if !orch_runtime.is_empty() {
            "orchestration"
        } else if dict_truthy(config.runtime.as_ref()) {
            "config"
        } else {
            "default"
        }
        .to_string(),
    );

    // adapter timeout_seconds: `runtime.adapters.<adapter>.timeout_seconds`.
    if let Some(adapter_name) = adapter.as_deref().filter(|a| !a.is_empty()) {
        let this_adapter_cfg = runtime
            .as_ref()
            .and_then(Value::as_dict)
            .and_then(|r| get(r, "adapters"))
            .and_then(Value::as_dict)
            .and_then(|a| get(a, adapter_name))
            .and_then(Value::as_dict);
        let has_timeout = this_adapter_cfg
            .and_then(|a| get(a, "timeout_seconds"))
            .is_some_and(|v| !matches!(v, Value::None));
        let key = format!("adapters.{adapter_name}.timeout_seconds");
        if has_timeout {
            let orch_has_timeout = get(&orch_runtime, "adapters")
                .and_then(Value::as_dict)
                .and_then(|a| get(a, adapter_name))
                .and_then(Value::as_dict)
                .and_then(|a| get(a, "timeout_seconds"))
                .is_some_and(|v| !matches!(v, Value::None));
            sources.insert(
                key,
                if orch_has_timeout {
                    "orchestration"
                } else {
                    "config"
                }
                .to_string(),
            );
        } else {
            sources.insert(key, "default".to_string());
        }
    }

    // persistence: only ever recorded when the orchestration or config
    // actually carries an object there -- Circuitry's own function never
    // adds a "default" entry for it either.
    if matches!(get(&orch_runtime, "persistence"), Some(Value::Dict(_))) {
        sources.insert("persistence".to_string(), "orchestration".to_string());
    } else if config
        .runtime
        .as_ref()
        .and_then(Value::as_dict)
        .is_some_and(|r| matches!(get(r, "persistence"), Some(Value::Dict(_))))
    {
        sources.insert("persistence".to_string(), "config".to_string());
    }

    // complexity: no cli override tiers here, so this rides the merged
    // runtime verbatim.
    let complexity = resolve_complexity_settings(runtime.as_ref())?;
    record_complexity_sources(
        &mut sources,
        config
            .runtime
            .as_ref()
            .and_then(Value::as_dict)
            .and_then(|r| get(r, "complexity")),
        get(&orch_runtime, "complexity"),
    );

    model = apply_router_precedence(&mut sources, model, &complexity.routing);

    Ok(EffectiveSettings {
        model,
        adapter,
        out: None,
        plugins,
        runtime,
        sources,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(pairs: Vec<(&str, Value)>) -> Value {
        let mut dict = Dict::new();
        for (k, v) in pairs {
            dict.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(dict)
    }

    #[test]
    fn model_and_adapter_fall_back_through_config_to_default() {
        let config = CircuitryConfig {
            default_model: Some("llama3.1:8b".to_string()),
            default_adapter: Some("ollama".to_string()),
            ..CircuitryConfig::default()
        };
        let settings = effective_settings(&config, &doc(vec![])).unwrap();
        assert_eq!(settings.model, Some("llama3.1:8b".to_string()));
        assert_eq!(settings.adapter, Some("ollama".to_string()));
        assert_eq!(settings.sources.get("model").unwrap(), "config");
        assert_eq!(settings.sources.get("adapter").unwrap(), "config");
    }

    #[test]
    fn a_document_model_outranks_the_config_default() {
        let config = CircuitryConfig {
            default_model: Some("llama3.1:8b".to_string()),
            ..CircuitryConfig::default()
        };
        let settings =
            effective_settings(&config, &doc(vec![("model", Value::from("gpt-4"))])).unwrap();
        assert_eq!(settings.model, Some("gpt-4".to_string()));
        assert_eq!(settings.sources.get("model").unwrap(), "orchestration");
    }

    #[test]
    fn sources_are_recorded_in_insertion_order() {
        let config = CircuitryConfig::default();
        let settings = effective_settings(&config, &doc(vec![])).unwrap();
        let keys: Vec<&str> = settings.sources.keys().map(String::as_str).collect();
        assert_eq!(
            &keys[..5],
            ["model", "adapter", "out", "plugins", "runtime"]
        );
        assert!(keys.contains(&"complexity"));
        assert!(keys.contains(&"complexity.scoring"));
        assert!(!keys.contains(&"persistence"));
    }

    #[test]
    fn non_string_plugins_are_rejected() {
        let config = CircuitryConfig::default();
        let err = effective_settings(
            &config,
            &doc(vec![("plugins", Value::List(vec![Value::from(1i64)]))]),
        )
        .unwrap_err();
        assert_eq!(err.0, "Plugins must be strings.");
    }

    #[test]
    fn applied_host_settings_notice_names_dotted_runtime_paths() {
        let config = CircuitryConfig::default();
        let mut runtime = Dict::new();
        runtime.insert(Value::Str("max_concurrency".to_string()), Value::from(2i64));
        let settings =
            effective_settings(&config, &doc(vec![("runtime", Value::Dict(runtime))])).unwrap();
        assert_eq!(
            settings.warnings,
            vec![
                "Applied host settings from the orchestration: runtime.max_concurrency".to_string()
            ]
        );
    }

    #[test]
    fn a_malformed_complexity_block_surfaces_as_a_config_error() {
        let config = CircuitryConfig::default();
        let mut routing = Dict::new();
        routing.insert(Value::Str("enabled".to_string()), Value::Bool(true));
        let mut complexity = Dict::new();
        complexity.insert(Value::Str("routing".to_string()), Value::Dict(routing));
        let mut runtime = Dict::new();
        runtime.insert(
            Value::Str("complexity".to_string()),
            Value::Dict(complexity),
        );
        let err =
            effective_settings(&config, &doc(vec![("runtime", Value::Dict(runtime))])).unwrap_err();
        assert!(err.0.contains("no bands are defined"));
    }

    #[test]
    fn persistence_source_is_recorded_only_when_configured() {
        let config = CircuitryConfig::default();
        let settings = effective_settings(&config, &doc(vec![])).unwrap();
        assert!(!settings.sources.contains_key("persistence"));

        let mut persistence = Dict::new();
        persistence.insert(Value::Str("enabled".to_string()), Value::Bool(true));
        persistence.insert(Value::Str("backend".to_string()), Value::from("sqlite"));
        persistence.insert(Value::Str("db_path".to_string()), Value::from("a.db"));
        let mut runtime = Dict::new();
        runtime.insert(
            Value::Str("persistence".to_string()),
            Value::Dict(persistence),
        );
        let settings =
            effective_settings(&config, &doc(vec![("runtime", Value::Dict(runtime))])).unwrap();
        assert_eq!(
            settings.sources.get("persistence").unwrap(),
            "orchestration"
        );
    }

    #[test]
    fn a_catch_all_band_wins_the_model_when_routing_is_enabled_and_none_is_pinned() {
        let config = CircuitryConfig::default();
        let mut scoring = Dict::new();
        scoring.insert(Value::Str("enabled".to_string()), Value::Bool(true));
        let mut catch_all = Dict::new();
        catch_all.insert(Value::Str("model".to_string()), Value::from("router-model"));
        let mut routing = Dict::new();
        routing.insert(Value::Str("enabled".to_string()), Value::Bool(true));
        routing.insert(
            Value::Str("bands".to_string()),
            Value::List(vec![Value::Dict(catch_all)]),
        );
        let mut complexity = Dict::new();
        complexity.insert(Value::Str("scoring".to_string()), Value::Dict(scoring));
        complexity.insert(Value::Str("routing".to_string()), Value::Dict(routing));
        let mut runtime = Dict::new();
        runtime.insert(
            Value::Str("complexity".to_string()),
            Value::Dict(complexity),
        );
        let settings =
            effective_settings(&config, &doc(vec![("runtime", Value::Dict(runtime))])).unwrap();
        assert_eq!(settings.model, Some("router-model".to_string()));
        assert_eq!(settings.sources.get("model").unwrap(), "router");
    }
}
