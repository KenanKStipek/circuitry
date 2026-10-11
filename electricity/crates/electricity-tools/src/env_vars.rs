//! `plugins/env_vars.py::EnvVarsPlugin`, ported -- signature only (M1
//! gate seam round 2, item S9); lane D1 fills the body.

use crate::{CheckResult, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::Value;

pub struct EnvVarsPlugin;

#[async_trait(?Send)]
impl ToolPlugin for EnvVarsPlugin {
    fn name(&self) -> &str {
        "env_vars"
    }

    async fn execute(&self, _params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        Err(ToolError::message(
            "env_vars: not implemented yet (lane D1)".to_string(),
        ))
    }

    fn check(&self) -> CheckResult {
        CheckResult {
            ok: false,
            missing: Vec::new(),
            message: Some("env_vars: not implemented yet (lane D1)".to_string()),
        }
    }
}
