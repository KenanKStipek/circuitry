//! electricity-config: `config.json` -> effective settings for an
//! electricity run (issue #431's Scope section, lane D). This crate is a
//! **lane A stub**: every type and function here is this crate's final
//! public signature, but every body either returns the one honest value
//! lane A can produce (an empty/unmerged shape) or an [`ConfigError`]
//! naming the lane that still has to fill it in -- never `unimplemented!`
//! (issue #431's gate lane, "Seams" section 5). Lane D replaces each
//! body; no signature here should need to change for that.
//!
//! # What lane D ports here
//!
//! - [`resolve_config`]: `cli/config.py::resolve_config` -- `SANE_DEFAULTS`
//!   deep-merged with the named file, then the `CIRCUITRY_*` environment
//!   overlays (`_apply_env_vars`).
//! - [`merge_runtime`]: `cli/effective_settings.py`'s own `_merge_runtime`
//!   -- a document's `runtime:` block deep-merged over the config's, with
//!   ceiling re-intersection for a numeric concurrency bound.
//! - [`effective_settings`]: `resolve_effective_settings` -- model/
//!   adapter/out/plugins/runtime and `sources` (`_record_complexity_
//!   sources` included).
//! - [`validate_complexity_and_persistence`][]: `resolve_complexity_
//!   settings` (`cli/complexity_config.py`) and `build_persistence_
//!   backend` (`core/store/persistence.py`)'s own validation --
//!   `electricity-compiler`'s `pipeline::pre_state_checks` calls this as
//!   its own documented hook (run-wiring steps 6/9, issue #431's Scope
//!   section); a lane A stub always succeeds, which is exactly
//!   `electricity-compiler`'s own already-documented "Known divergences"
//!   gap (a malformed `runtime.complexity`/`runtime.persistence` block
//!   passes `check_for_run` here where `cof run` would fail) --
//!   unchanged by this crate's existence until lane D fills it in.

use electricity_value::Value;
use std::collections::HashMap;
use std::fmt;
use std::path::Path;

/// A loaded `config.json`, already deep-merged over `cli/config.py::
/// SANE_DEFAULTS` and the `CIRCUITRY_*` environment overlays (issue
/// #431's run-wiring step 1). Lane A reserves the shape; lane D fills in
/// every field `resolve_effective_settings` actually reads off it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CircuitryConfig {
    /// The config file's own `runtime:` block, after the deep merge --
    /// the one piece of a `CircuitryConfig` `electricity-compiler`'s
    /// `CheckOptions::config_runtime` already carries today (pre-#431),
    /// so this field's shape can't change out from under that caller.
    pub runtime: Option<Value>,
}

/// `resolve_config`'s own error text (`cli/config.py::ConfigError`) --
/// a missing, unreadable, or unparseable config file. Also
/// [`validate_complexity_and_persistence`]'s own error shape (a
/// malformed `runtime.complexity`/`runtime.persistence` block raises the
/// same way a bad config file does: a run-ending, un-wrapped message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ConfigError {}

/// `cli/config.py::resolve_config(explicit_path, env=os.environ)` --
/// `SANE_DEFAULTS` deep-merged with *explicit_path*'s own JSON, then
/// *env*'s `CIRCUITRY_*` overlays (`_apply_env_vars`). *env* is passed
/// explicitly rather than read from the process environment, so a test
/// (and the real CLI's own call site) controls it exactly -- the same
/// reason [`electricity_compiler::CheckOptions::inputs`] is an explicit
/// field rather than this crate reading `std::env::args` itself.
///
/// Lane A stub: always `Err`, naming this function, so a caller that
/// reaches it before lane D lands fails loudly instead of silently
/// running with an empty config (`electricity`'s own lib crate does not
/// call this yet -- it still reads a config file's bare `runtime:` key
/// itself, pre-#431 behaviour, until lane D switches it over).
pub fn resolve_config(
    explicit_path: &Path,
    env: &HashMap<String, String>,
) -> Result<CircuitryConfig, ConfigError> {
    let _ = (explicit_path, env);
    Err(ConfigError(
        "electricity-config::resolve_config is not implemented yet (lane D, issue #431)"
            .to_string(),
    ))
}

/// `cli/effective_settings.py`'s own `_merge_runtime(config_runtime,
/// document_runtime)`: the document's `runtime:` block deep-merged over
/// the config's, then a numeric concurrency ceiling (`max_concurrency`,
/// `concurrency_groups.*.max_concurrency`) re-intersected to the
/// stricter of the two sides rather than simply overwritten.
///
/// Lane A stub: a plain top-level-key merge (the document's own key wins
/// outright, config's key otherwise) -- `electricity_compiler::pipeline`'s
/// own pre-#431 `merged_runtime_block` behaviour, not yet Circuitry's own
/// deep merge or ceiling re-intersection. `electricity_compiler::pipeline`
/// does not call this yet; lane D switches it over once this does the
/// real merge.
pub fn merge_runtime(
    config_runtime: Option<&Value>,
    document_runtime: Option<&Value>,
) -> Option<Value> {
    match (config_runtime, document_runtime) {
        (None, None) => None,
        (Some(config), None) => Some(config.clone()),
        (None, Some(document)) => Some(document.clone()),
        (Some(config), Some(document)) => {
            let (Value::Dict(config_dict), Value::Dict(document_dict)) = (config, document) else {
                return Some(document.clone());
            };
            let mut merged = config_dict.clone();
            for (key, value) in document_dict {
                merged.insert(key.clone(), value.clone());
            }
            Some(Value::Dict(merged))
        }
    }
}

/// `resolve_effective_settings`'s own result shape: the model/adapter/
/// `--out`/plugins/runtime a run actually uses, plus `sources` (which
/// config layer supplied each one -- `_record_complexity_sources`
/// included).
///
/// Lane A stub: a bare, empty shape -- every field lane D's real
/// `resolve_effective_settings` port fills in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectiveSettings {
    pub model: Option<String>,
    pub adapter: Option<String>,
    pub runtime: Option<Value>,
    /// `sources`: a dotted-settings-path -> provenance-label map
    /// (`"config"`/`"document"`/`"env"`/`"default"`, `cli/
    /// effective_settings.py`'s own `sources` dict).
    pub sources: HashMap<String, String>,
}

/// `resolve_effective_settings(config, document)` -- merges
/// [`CircuitryConfig`] and the document's own top-level settings the way
/// `cli/effective_settings.py` does, recording each field's `sources`
/// provenance.
///
/// Lane A stub: always the all-`None`, empty-`sources` default --
/// nothing yet reads *config*/*document* at all.
pub fn effective_settings(config: &CircuitryConfig, document: &Value) -> EffectiveSettings {
    let _ = (config, document);
    EffectiveSettings::default()
}

/// Lane D's own hook (issue #431's run-wiring steps 6/9): validates a
/// document/config's `runtime.complexity` (`cli/complexity_config.py::
/// resolve_complexity_settings`) and `runtime.persistence`
/// (`core/store/persistence.py::build_persistence_backend`) blocks,
/// raising Circuitry's own text for a malformed one -- called from
/// `electricity_compiler::pipeline::pre_state_checks`, between the
/// concurrency-configuration check and `check_interface_inputs`, the same
/// position `cof run` validates both in (steps 6 and 9 of issue #431's
/// run-wiring table).
///
/// Lane A stub: always `Ok(())` -- `electricity-compiler`'s own already-
/// documented divergence (a malformed `runtime.complexity`/`runtime.
/// persistence` block passes `check_for_run` where `cof run` would fail)
/// stays exactly as it is today; this function only gives lane D a single
/// place to remove that gap from, instead of two new checks threaded
/// into `electricity-compiler` itself.
pub fn validate_complexity_and_persistence(
    document: &Value,
    effective_runtime: Option<&Value>,
) -> Result<(), ConfigError> {
    let _ = (document, effective_runtime);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    #[test]
    fn resolve_config_is_a_lane_d_stub() {
        let result = resolve_config(Path::new("config.json"), &HashMap::new());
        assert!(result.is_err());
    }

    #[test]
    fn merge_runtime_prefers_the_document_key_over_the_configs() {
        let mut config = Dict::new();
        config.insert(Value::Str("model".to_string()), Value::Str("a".to_string()));
        let mut document = Dict::new();
        document.insert(Value::Str("model".to_string()), Value::Str("b".to_string()));
        let merged = merge_runtime(Some(&Value::Dict(config)), Some(&Value::Dict(document)));
        assert_eq!(
            merged,
            Some(Value::Dict({
                let mut expected = Dict::new();
                expected.insert(Value::Str("model".to_string()), Value::Str("b".to_string()));
                expected
            }))
        );
    }

    #[test]
    fn merge_runtime_with_neither_side_is_none() {
        assert_eq!(merge_runtime(None, None), None);
    }

    #[test]
    fn effective_settings_is_a_lane_d_stub() {
        let settings = effective_settings(&CircuitryConfig::default(), &Value::Dict(Dict::new()));
        assert_eq!(settings, EffectiveSettings::default());
    }

    #[test]
    fn validate_complexity_and_persistence_is_a_no_op_for_now() {
        let mut document = Dict::new();
        document.insert(
            Value::Str("runtime".to_string()),
            Value::Str("not even an object".to_string()),
        );
        assert!(validate_complexity_and_persistence(&Value::Dict(document), None).is_ok());
    }
}
