//! `plugins/shell.py::ShellPlugin`, ported -- signature only (M1 gate
//! seam round 2, item S9); lane C fills the body. Not a shell
//! interpreter: runs a single allowlisted binary, no shell
//! metacharacters, no pipes/redirection/substitution/globbing --
//! `plugins/shell.py`'s own module doc comment, the full allowlist/
//! host-pin-intersection rule included.

use crate::{CheckResult, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::Value;

/// `plugins/shell.py::ShellPlugin` -- signature only; lane C fills the
/// body (the default/pinned allowlist intersection, the per-arg
/// shell-metacharacter/null-byte/newline rejection, and the
/// `run_tracked` dispatch).
pub struct ShellPlugin {
    /// `runtime.plugins.shell.allowed_commands`, already validated as
    /// a list of strings by [`crate::registry::build_plugin`]'s own
    /// caller -- the host's own ceiling, intersected with the effect's
    /// own `params.allowed_commands` (or the default allowlist) at
    /// dispatch time, never replaced by it.
    pub pinned_allowed_commands: Option<Vec<String>>,
}

#[async_trait(?Send)]
impl ToolPlugin for ShellPlugin {
    fn name(&self) -> &str {
        "shell"
    }

    async fn execute(&self, _params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        Err(ToolError::message(
            "shell: not implemented yet (lane C)".to_string(),
        ))
    }

    fn check(&self) -> CheckResult {
        CheckResult {
            ok: false,
            missing: Vec::new(),
            message: Some("shell: not implemented yet (lane C)".to_string()),
        }
    }
}
