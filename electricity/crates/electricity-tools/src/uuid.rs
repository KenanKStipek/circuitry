//! `plugins/uuid.py::UuidPlugin`, ported -- signature only (M1 gate
//! seam round 2, item S9); lane D1 fills the body.

use crate::{CheckResult, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::Value;

pub struct UuidPlugin;

#[async_trait(?Send)]
impl ToolPlugin for UuidPlugin {
    fn name(&self) -> &str {
        "uuid"
    }

    async fn execute(&self, _params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        Err(ToolError::message(
            "uuid: not implemented yet (lane D1)".to_string(),
        ))
    }

    fn check(&self) -> CheckResult {
        CheckResult {
            ok: false,
            missing: Vec::new(),
            message: Some("uuid: not implemented yet (lane D1)".to_string()),
        }
    }
}
