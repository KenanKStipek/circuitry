//! `run_tool`: the VM's own tool-dispatch entry point -- looks a `tool`
//! effect's `provider:` up in an [`electricity_tools::ToolRegistry`] and
//! runs it. Real, not a stub: once lane C registers `json` into a
//! registry, this already works end to end -- the seam issue #431's gate
//! lane exists to create (lane B/C never edit this file to wire a new
//! provider in; they only build the registry they pass to it).

use crate::VmError;
use electricity_tools::ToolRegistry;
use electricity_value::Value;

/// Runs *provider* (a tool effect's own `provider:`) against *registry*,
/// with *params* already rendered (lane B/C's own `params.rs` -- `{from:
/// ...}` resolved, templates rendered, `params_json` deep-merged; out of
/// scope for this function) and *timeout_ms* converted to the whole
/// seconds [`electricity_tools::ToolPlugin::execute`] takes (rounded up,
/// floored at 1 -- never 0, which every tool plugin would otherwise read
/// as "no timeout" or "already expired" depending on its own
/// implementation).
///
/// `Err` is [`VmError::ToolNotFound`] for an unregistered *provider*, or
/// the plugin's own [`electricity_tools::ToolError`] wrapped the same
/// way.
pub async fn run_tool(
    registry: &ToolRegistry,
    provider: &str,
    params: Value,
    timeout_ms: Option<u64>,
) -> Result<electricity_tools::ToolResult, VmError> {
    let plugin = registry
        .get(provider)
        .ok_or_else(|| VmError::ToolNotFound(provider.to_string()))?;
    let timeout_seconds = timeout_ms
        .map(|ms| ms.div_ceil(1000).max(1) as u32)
        .unwrap_or(300);
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
        let err = run_tool(&registry, "json", Value::None, None)
            .await
            .unwrap_err();
        assert_eq!(err, VmError::ToolNotFound("json".to_string()));
    }

    #[tokio::test]
    async fn a_registered_providers_own_error_is_wrapped() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(electricity_tools::json::JsonTool));
        let err = run_tool(&registry, "json", Value::None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, VmError::Tool(_)));
    }
}
