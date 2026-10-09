//! Lane C: the `json` tool (`plugins/json.py`) -- `parse`/`stringify`/
//! `extract`, with Circuitry's own exact messages (`JsonPlugin: parse
//! mode requires params['input'] as a string.`, `json: unknown mode
//! {mode!r}`) and `raw: {"mode": mode}`.
//!
//! Lane A stub: [`JsonTool`] is registered under the right name and
//! answers [`crate::ToolPlugin::check`] honestly, but
//! [`crate::ToolPlugin::execute`] always errors -- M0-H's only tool has
//! no working mode yet.

use crate::{CheckResult, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::Value;

/// `plugins/json.py::JsonPlugin` -- `mode: parse|stringify|extract`.
pub struct JsonTool;

#[async_trait(?Send)]
impl ToolPlugin for JsonTool {
    fn name(&self) -> &str {
        "json"
    }

    async fn execute(&self, params: Value, _timeout_seconds: u32) -> Result<ToolResult, ToolError> {
        let _ = params;
        Err(ToolError(
            "electricity-tools::json::JsonTool::execute is not implemented yet (lane C, issue #431)"
                .to_string(),
        ))
    }

    fn check(&self) -> CheckResult {
        // `plugins/json.py::JsonPlugin.check` -- no dependency of its
        // own (pure in-process parsing), so always ready.
        CheckResult {
            ok: true,
            missing: Vec::new(),
            message: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_tool_reports_its_own_name_and_is_always_ready() {
        let tool = JsonTool;
        assert_eq!(tool.name(), "json");
        assert!(tool.check().ok);
    }

    #[tokio::test]
    async fn json_tool_execute_is_a_lane_c_stub() {
        let tool = JsonTool;
        let err = tool
            .execute(Value::Dict(Default::default()), 30)
            .await
            .unwrap_err();
        assert!(err.0.contains("lane C"));
    }
}
