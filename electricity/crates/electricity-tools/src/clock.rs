//! `plugins/clock.py::ClockPlugin`, ported -- signature only (M1 gate
//! seam round 2, item S9); lane D1 fills the body (platform `strftime`,
//! CPython's own `%z`/`%Z`/`%f` handling in front -- #448's decision
//! 8).

use crate::{CheckResult, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::Value;

pub struct ClockPlugin;

#[async_trait(?Send)]
impl ToolPlugin for ClockPlugin {
    fn name(&self) -> &str {
        "clock"
    }

    async fn execute(&self, _params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        Err(ToolError::message(
            "clock: not implemented yet (lane D1)".to_string(),
        ))
    }

    fn check(&self) -> CheckResult {
        CheckResult {
            ok: false,
            missing: Vec::new(),
            message: Some("clock: not implemented yet (lane D1)".to_string()),
        }
    }
}
