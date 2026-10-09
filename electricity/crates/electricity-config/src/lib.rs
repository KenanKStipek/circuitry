//! electricity-config: `config.json` -> effective settings for an
//! electricity run (issue #431's Scope section, lane D1). Ports:
//!
//! - [`resolve_config`]: `cli/config.py::resolve_config`'s explicit-path
//!   branch -- `SANE_DEFAULTS` deep-merged with the named file, then the
//!   `CIRCUITRY_*` environment overlays (`_apply_env_vars`). Electricity's
//!   positional `<config.json>` is always that explicit path (DESIGN.md
//!   §11): there is no global/project config discovery and no
//!   `CIRCUITRY_CONFIG` env var of its own ([`config`] module docs).
//! - [`merge_runtime`]: `cli/effective_settings.py::_merge_runtime` --
//!   a document's `runtime:` block deep-merged over the config's
//!   (`plugins`/`adapters` one level deeper than the rest), with the
//!   shell `allowed_commands` ceiling re-intersected afterward
//!   regardless of trust ([`merge`] module docs).
//! - [`effective_settings`]: `resolve_effective_settings`, narrowed to
//!   the tiers electricity's own CLI has ([`effective_settings`][mod]
//!   module docs).
//! - [`validate_complexity`]/[`validate_persistence`]: the two
//!   pre-checks `electricity-compiler`'s own crate docs listed as a
//!   known divergence until this lane landed --
//!   [`complexity::resolve_complexity_settings`] and
//!   [`persistence::validate_persistence_block`]'s own validation,
//!   raising Circuitry's own text for a malformed `runtime.complexity`/
//!   `runtime.persistence` block.
//! - [`allowlist::check_allowlist`]/[`allowlist::allowlists`]/
//!   [`allowlist::capability_allow`]: the `enabled_adapters`/
//!   `enabled_tools`/`enabled_plugins` allowlist seam issue #431's lane
//!   table left this lane to shape ([`allowlist`] module docs).
//!
//! [mod]: crate::effective_settings

mod allowlist;
mod complexity;
mod config;
mod effective_settings;
mod merge;
mod persistence;
mod util;

pub use allowlist::{
    Allowlists, adapter_denial, allowlists, capability_allow, check_allowlist, tool_denial,
    walk_orchestration_refs,
};
pub use complexity::{
    ComplexityBand, ComplexitySettings, DecompositionSettings, RoutingSettings, SCORE_MAX,
    SCORE_MIN, ScoringSettings,
};
pub use config::{CircuitryConfig, ConfigError, resolve_config};
pub use effective_settings::{EffectiveSettings, effective_settings};
pub use merge::merge_runtime;

use electricity_value::Value;

/// Issue #431's run-wiring step 6 (inside `resolve_effective_settings`,
/// before the concurrency limiter): `resolve_complexity_settings`'s own
/// validation of *effective_runtime*'s `complexity` block. `document`
/// is accepted (electricity-compiler's own `pipeline::pre_state_checks`
/// calls this with both), but unused -- Circuitry's own
/// `resolve_complexity_settings` reads only the merged `runtime` mapping,
/// never the raw document, and this crate's callers always have that
/// mapping to hand directly.
pub fn validate_complexity(
    document: &Value,
    effective_runtime: Option<&Value>,
) -> Result<(), ConfigError> {
    let _ = document;
    complexity::resolve_complexity_settings(effective_runtime)?;
    Ok(())
}

/// Issue #431's run-wiring step 9 (after the concurrency limiter, before
/// `check_interface_inputs`): `build_persistence_backend`'s own
/// validation of *effective_runtime*'s `persistence` block -- the
/// backend-alias lookup and each backend's own required-field checks,
/// never an actual connection (see [`persistence`] module docs).
/// *document* is accepted for the same reason [`validate_complexity`]
/// accepts it, and is equally unused.
pub fn validate_persistence(
    document: &Value,
    effective_runtime: Option<&Value>,
) -> Result<(), ConfigError> {
    let _ = document;
    persistence::validate_persistence_block(effective_runtime).map_err(ConfigError)
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    #[test]
    fn validate_complexity_raises_the_same_text_resolve_complexity_settings_would() {
        let mut routing = Dict::new();
        routing.insert(Value::Str("enabled".to_string()), Value::Bool(true));
        let mut complexity = Dict::new();
        complexity.insert(Value::Str("routing".to_string()), Value::Dict(routing));
        let mut runtime = Dict::new();
        runtime.insert(
            Value::Str("complexity".to_string()),
            Value::Dict(complexity),
        );
        let err = validate_complexity(&Value::Dict(Dict::new()), Some(&Value::Dict(runtime)))
            .unwrap_err();
        assert!(err.0.contains("no bands are defined"));
    }

    #[test]
    fn validate_complexity_is_ok_when_the_block_is_absent() {
        assert!(validate_complexity(&Value::Dict(Dict::new()), None).is_ok());
    }

    #[test]
    fn validate_persistence_raises_for_an_unsupported_backend() {
        let mut persistence = Dict::new();
        persistence.insert(Value::Str("enabled".to_string()), Value::Bool(true));
        persistence.insert(Value::Str("backend".to_string()), Value::from("dynamodb"));
        let mut runtime = Dict::new();
        runtime.insert(
            Value::Str("persistence".to_string()),
            Value::Dict(persistence),
        );
        let err = validate_persistence(&Value::Dict(Dict::new()), Some(&Value::Dict(runtime)))
            .unwrap_err();
        assert!(err.0.starts_with("Unsupported persistence backend"));
    }

    #[test]
    fn validate_persistence_is_ok_when_the_block_is_absent() {
        assert!(validate_persistence(&Value::Dict(Dict::new()), None).is_ok());
    }
}
