//! `plugins/validate_yaml.py::ValidateYamlPlugin`, ported -- signature
//! only (review P2-5 on PR #454's second review, extending M1 gate seam
//! round 2's item S9 to this file too); lane L2 fills the body. Made a
//! native tool by #448's decision 3, because decomposition's own
//! bundled planner calls it -- every other plugin in Circuitry's
//! registry stays refused up front until the M2 plugin bridge.

use crate::{CheckResult, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::Value;

pub struct ValidateYamlPlugin;

#[async_trait(?Send)]
impl ToolPlugin for ValidateYamlPlugin {
    fn name(&self) -> &str {
        "validate_yaml"
    }

    async fn execute(&self, _params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        Err(ToolError::message(
            "validate_yaml: not implemented yet (lane L2)".to_string(),
        ))
    }

    fn check(&self) -> CheckResult {
        CheckResult {
            ok: false,
            missing: Vec::new(),
            message: Some("validate_yaml: not implemented yet (lane L2)".to_string()),
        }
    }
}
