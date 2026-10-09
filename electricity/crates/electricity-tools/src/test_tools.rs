//! `test-tools` feature only: blocking `sleep`/`fail` tools for the
//! signal-handling test suite (issue #431's acceptance criteria --
//! "Signals ... `test-tools` feature, not an early `shell` tool"). Never
//! compiled into a release build (`electricity-cli`'s own `Cargo.toml`
//! never enables this feature).
//!
//! Lane A stub: both tools are registered under their final names, but
//! [`SleepTool::execute`]/[`FailTool::execute`] error rather than
//! actually blocking/failing -- lane D's own signal tests need a real
//! blocking sleep to interrupt.

use crate::{CheckResult, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::Value;

/// Blocks for `params.seconds` -- lane D's own signal tests dispatch
/// this inside a `finally:`/tree branch to prove a queued branch never
/// starts, or that a second SIGINT during a blocking `finally` exits
/// immediately.
pub struct SleepTool;

#[async_trait(?Send)]
impl ToolPlugin for SleepTool {
    fn name(&self) -> &str {
        "sleep"
    }

    async fn execute(&self, params: Value, _timeout_seconds: u32) -> Result<ToolResult, ToolError> {
        let _ = params;
        Err(ToolError(
            "electricity-tools::test_tools::SleepTool::execute is not implemented yet (lane D, issue #431)"
                .to_string(),
        ))
    }

    fn check(&self) -> CheckResult {
        CheckResult {
            ok: true,
            missing: Vec::new(),
            message: None,
        }
    }
}

/// Always fails -- lane D's own `on_error`/`retries` signal-adjacent
/// tests need a tool that deterministically raises, without `shell`.
pub struct FailTool;

#[async_trait(?Send)]
impl ToolPlugin for FailTool {
    fn name(&self) -> &str {
        "fail"
    }

    async fn execute(&self, params: Value, _timeout_seconds: u32) -> Result<ToolResult, ToolError> {
        let _ = params;
        Err(ToolError(
            "electricity-tools::test_tools::FailTool::execute is not implemented yet (lane D, issue #431)"
                .to_string(),
        ))
    }

    fn check(&self) -> CheckResult {
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
    fn both_test_tools_report_their_own_names() {
        assert_eq!(SleepTool.name(), "sleep");
        assert_eq!(FailTool.name(), "fail");
    }
}
