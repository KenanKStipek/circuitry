//! `test-tools` feature only: blocking `sleep`/`fail` tools for the
//! signal-handling test suite (issue #431's acceptance criteria --
//! "Signals ... `test-tools` feature, not an early `shell` tool"). Never
//! compiled into a release build (`electricity-cli`'s own `Cargo.toml`
//! never enables this feature).
//!
//! Neither tool checks a cancellation token of its own while it runs --
//! that happens one layer up, between attempts/branches
//! (`electricity_vm::exec::tool::execute_tool`'s own job, issue #431's
//! lane table) -- so a signal during a [`SleepTool`] call behaves
//! exactly like Circuitry's own blocking tool call does: it runs to
//! completion, and cancellation only takes effect at the next point the
//! VM actually checks for it (the next attempt, the next tree branch,
//! `finally:`'s own body). That's deliberate: it's what the signal
//! tests' own "a second SIGINT during a blocking `finally` exits
//! immediately" case depends on -- the first signal must *not*
//! interrupt the call already in flight, or there would be nothing left
//! blocking for a second signal to interrupt.

use crate::{CheckResult, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::{Dict, Value};

/// Blocks for `params.seconds` (a number; defaults to `0` when absent)
/// -- lane D's own signal tests dispatch this inside a `finally:`/tree
/// branch to prove a queued branch never starts, or that a second
/// SIGINT during a blocking `finally` exits immediately.
pub struct SleepTool;

fn seconds_param(params: &Value) -> f64 {
    params
        .as_dict()
        .and_then(|d| d.get(&Value::Str("seconds".to_string())))
        .and_then(|v| match v {
            Value::Int(i) => Some(i.to_f64()),
            Value::Float(f) => Some(*f),
            _ => None,
        })
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or(0.0)
}

#[async_trait(?Send)]
impl ToolPlugin for SleepTool {
    fn name(&self) -> &str {
        "sleep"
    }

    async fn execute(&self, params: Value, _timeout_seconds: u32) -> Result<ToolResult, ToolError> {
        let seconds = seconds_param(&params);
        tokio::time::sleep(std::time::Duration::from_secs_f64(seconds)).await;
        Ok(ToolResult::new(
            Value::Dict(Dict::new()),
            Value::Dict(Dict::new()),
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

/// Always fails, with a fixed, deterministic message -- lane D's own
/// `on_error`/`retries` signal-adjacent tests need a tool that
/// deterministically raises, without `shell`.
pub struct FailTool;

#[async_trait(?Send)]
impl ToolPlugin for FailTool {
    fn name(&self) -> &str {
        "fail"
    }

    async fn execute(&self, params: Value, _timeout_seconds: u32) -> Result<ToolResult, ToolError> {
        let _ = params;
        Err(ToolError("FailTool: deliberate failure".to_string()))
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

    #[tokio::test(start_paused = true)]
    async fn sleep_tool_actually_blocks_for_its_own_params_seconds() {
        let mut params = Dict::new();
        params.insert(Value::Str("seconds".to_string()), Value::from(5i64));
        let started = tokio::time::Instant::now();
        SleepTool.execute(Value::Dict(params), 30).await.unwrap();
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(5));
    }

    #[tokio::test]
    async fn sleep_tool_defaults_to_zero_with_no_seconds_param() {
        let result = SleepTool
            .execute(Value::Dict(Dict::new()), 30)
            .await
            .unwrap();
        assert!(result.ok);
    }

    #[tokio::test]
    async fn fail_tool_always_fails_with_a_fixed_message() {
        let err = FailTool
            .execute(Value::Dict(Dict::new()), 30)
            .await
            .unwrap_err();
        assert_eq!(err.0, "FailTool: deliberate failure");
    }
}
