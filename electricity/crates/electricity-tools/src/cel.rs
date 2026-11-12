//! `plugins/cel.py::CelPlugin`, ported in name only (issue #723): the
//! `cel` tool's expression is evaluated by its own dispatching effect
//! (`electricity_vm::exec::tool::execute_tool`), against the bindings
//! an `if` at the same position sees -- this crate has no access to
//! that live scope, so [`CelPlugin::execute`] is never actually called
//! on the real dispatch path, exactly as cof's own `CelPlugin.execute`
//! (`plugins/cel.py`) never is: `core/tool.py::ToolRuntime.execute`
//! special-cases `isinstance(plugin, CelPlugin)` and calls
//! `plugin.evaluate(params=..., ctx=...)` instead. This stub exists
//! only so [`crate::registry::build_plugin`] (and the generic retry/
//! concurrency machinery built around it) succeeds for `cel` the same
//! way it does for every other provider.

use crate::{CheckResult, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::Value;

pub struct CelPlugin;

#[async_trait(?Send)]
impl ToolPlugin for CelPlugin {
    fn name(&self) -> &str {
        "cel"
    }

    async fn execute(&self, _params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        Err(ToolError::message(
            "cel: the expression is evaluated by its tool effect, with its bindings.",
        ))
    }

    fn check(&self) -> CheckResult {
        CheckResult {
            ok: true,
            ..Default::default()
        }
    }
}
