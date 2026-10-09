//! `run_tool`: the VM's own tool-dispatch entry point -- looks a `tool`
//! effect's `provider:` up in an [`electricity_tools::ToolRegistry`] and
//! runs it. The dispatch seam itself is final (lane B/C never edit this
//! file to wire a new provider in; they only build the registry they
//! pass to it) -- but *provider* normalization and the exact "unknown
//! provider" wording still diverge from `plugins/factory.py::
//! build_plugin` (no `.strip().lower()`, and this crate's own
//! [`crate::VmError::ToolNotFound`] text isn't `build_plugin`'s own
//! `"Unknown plugin: 'x'. Supported plugins: ..."`), so lane C may still
//! change either once it has the full `electricity_tools::ToolRegistry`
//! populated to word the supported-list half of that message from.

use crate::VmError;
use electricity_tools::ToolRegistry;
use electricity_value::Value;

/// Runs *provider* (a tool effect's own `provider:`) against *registry*,
/// with *params* already rendered (lane B/C's own `params.rs` -- `{from:
/// ...}` resolved, templates rendered, `params_json` deep-merged; out of
/// scope for this function) and *timeout_seconds* already resolved by
/// the caller (`core/tool.py::ToolRuntime._resolve_timeout_seconds`'s
/// own `timeout_ms`-vs-`runtime.tools.timeout_seconds`-vs-default
/// policy needs the merged runtime config this function doesn't take --
/// [`crate::RunContext::runtime_config`] is where lane B's own caller
/// reads it from before calling this).
///
/// `Err` is [`VmError::ToolNotFound`] for an unregistered *provider*, or
/// the plugin's own [`electricity_tools::ToolError`] wrapped the same
/// way.
pub async fn run_tool(
    registry: &ToolRegistry,
    provider: &str,
    params: Value,
    timeout_seconds: u32,
) -> Result<electricity_tools::ToolResult, VmError> {
    let normalized = provider.trim().to_lowercase();
    let plugin = registry
        .get(&normalized)
        .ok_or_else(|| VmError::ToolNotFound(provider.to_string()))?;
    plugin
        .execute(params, timeout_seconds)
        .await
        .map_err(|err| VmError::Tool(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unregistered_provider_is_reported_by_name() {
        let registry = ToolRegistry::new();
        let err = run_tool(&registry, "json", Value::None, 300)
            .await
            .unwrap_err();
        assert_eq!(err, VmError::ToolNotFound("json".to_string()));
    }

    #[tokio::test]
    async fn a_provider_name_is_normalized_before_lookup() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(electricity_tools::json::JsonTool));
        let err = run_tool(&registry, "  JSON ", Value::None, 300)
            .await
            .unwrap_err();
        assert!(matches!(err, VmError::Tool(_)));
    }

    #[tokio::test]
    async fn a_registered_providers_own_error_is_wrapped() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(electricity_tools::json::JsonTool));
        let err = run_tool(&registry, "json", Value::None, 300)
            .await
            .unwrap_err();
        assert!(matches!(err, VmError::Tool(_)));
    }
}
