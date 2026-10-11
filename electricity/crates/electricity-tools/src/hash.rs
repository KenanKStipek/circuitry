//! `plugins/hash.py::HashPlugin`, ported -- signature only (M1 gate
//! seam round 2, item S9); lane D1 fills the body.

use crate::{CheckResult, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::Value;

pub struct HashPlugin;

#[async_trait(?Send)]
impl ToolPlugin for HashPlugin {
    fn name(&self) -> &str {
        "hash"
    }

    async fn execute(&self, _params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        Err(ToolError::message(
            "hash: not implemented yet (lane D1)".to_string(),
        ))
    }

    fn check(&self) -> CheckResult {
        CheckResult {
            ok: false,
            missing: Vec::new(),
            message: Some("hash: not implemented yet (lane D1)".to_string()),
        }
    }
}
