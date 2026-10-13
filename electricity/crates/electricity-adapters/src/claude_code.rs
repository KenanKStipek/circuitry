//! `adapters/claude_code.py::ClaudeCodeAdapter`, ported (issue #520
//! part B) -- one completion through the Claude Code CLI (`claude`)
//! and its own login, over `electricity_tools::agent_cli` (part A):
//! `claude -p --output-format json --no-session-persistence --tools ""
//! --strict-mcp-config` with the prompt on stdin, in a fresh temporary
//! directory so no project's `CLAUDE.md` is picked up. A JSON prompt
//! with a non-empty `schema` adds `--json-schema` and takes the CLI's
//! `structured_output` as the answer. Every [`GenerateOptions`] field
//! is ignored, with one warning naming the set ones.

use crate::agent_cli::{
    build_generate_result, check_binary, check_type_error, io_error_to_adapter,
    parse_error_to_adapter, resolve_model, run_error_to_adapter,
};
use crate::config::{cfg_value_or, require_str, string_list};
use async_trait::async_trait;
use electricity_tools::agent_cli::{
    ClaudeCommandOpts, DEFAULT_UNSET_ENV, TempDirGuard, claude_command, parse_claude_output,
    run_agent_cli,
};
use electricity_value::Value;
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateResult, PyExc,
};
use electricity_vm::retry::RetryInfo;

/// `adapters/claude_code.py::ClaudeCodeAdapter`. `binary`/`default_model`
/// are kept as the raw post-`cfg.get(...) or default` [`Value`]
/// (`crate::config`'s own doc comment, B5) rather than a plain
/// `String`: a truthy non-string config value must survive to the
/// moment it is actually used as a string, not fall back to the
/// default name and start a real CLI.
#[derive(Debug)]
pub struct ClaudeCodeAdapter {
    binary: Value,
    default_model: Value,
    unset_env: Vec<String>,
    extra_args: Vec<String>,
}

#[async_trait(?Send)]
impl Adapter for ClaudeCodeAdapter {
    fn name(&self) -> &str {
        "claude_code"
    }

    async fn generate(
        &self,
        model: &str,
        prompt: &str,
        options: &GenerateOptions,
        call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        let binary = require_str(&self.binary)?;
        let resolved_model = resolve_model(model, &self.default_model)?;
        let json_schema = options.json_schema.clone();
        let cmd = claude_command(
            binary,
            &ClaudeCommandOpts {
                model: resolved_model,
                json_schema: json_schema.clone(),
                extra_args: self.extra_args.clone(),
                ..Default::default()
            },
        )
        .map_err(|err| AdapterError {
            message: err.to_string(),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::TypeError,
        })?;
        let workdir = TempDirGuard::new("circuitry-claude-").map_err(io_error_to_adapter)?;
        let proc = run_agent_cli(
            &cmd,
            &self.unset_env,
            f64::from(call.timeout_seconds),
            prompt.as_bytes().to_vec(),
            Some(workdir.path().to_path_buf()),
            call.token,
        )
        .await
        .map_err(run_error_to_adapter)?;
        let result =
            parse_claude_output(&proc, json_schema.as_ref()).map_err(parse_error_to_adapter)?;
        Ok(build_generate_result("claude_code", result, options))
    }

    async fn check(&self) -> CheckResult {
        match require_str(&self.binary) {
            Ok(binary) => check_binary("claude_code", binary),
            Err(err) => check_type_error(&err),
        }
    }
}

/// The pure half of `adapters/factory.py::_build_claude_code` -- kept
/// separate from [`build_claude_code`] so a test can inspect the
/// resolved fields directly, the same way `ollama::build_request` is
/// kept callable on its own rather than only reachable through a
/// boxed `dyn Adapter`.
fn build(cfg: &Value) -> Result<ClaudeCodeAdapter, AdapterError> {
    Ok(ClaudeCodeAdapter {
        binary: cfg_value_or(cfg, "binary", "claude"),
        default_model: cfg_value_or(cfg, "default_model", ""),
        unset_env: string_list(cfg, "unset_env", "claude_code", DEFAULT_UNSET_ENV)?,
        extra_args: string_list(cfg, "extra_args", "claude_code", &[])?,
    })
}

/// `adapters/factory.py::_build_claude_code`.
pub fn build_claude_code(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(Box::new(build(cfg)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_bytecode::EffectPath;
    use electricity_value::{CancellationToken, Dict};
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    fn dict(pairs: &[(&str, Value)]) -> Value {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v.clone());
        }
        Value::Dict(d)
    }

    fn call_ctx<'a>(path: &'a EffectPath, token: &'a CancellationToken) -> CallContext<'a> {
        CallContext {
            path,
            timeout_seconds: 5,
            token,
        }
    }

    /// A fake `claude` script: echoes its argv (one per line, prefixed
    /// `ARGV:`), the prompt it read from stdin (`PROMPT:`), its own
    /// `cwd` (`CWD:`), and `result`/`is_error` the shell caller passed
    /// as `--model` (so a test can select a canned behaviour), as
    /// Claude Code's own `--output-format json` result object.
    fn write_fake_claude(dir: &std::path::Path, script: &str) -> String {
        let path = dir.join("claude");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(script.as_bytes()).unwrap();
        drop(file);
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(&path, perms).unwrap();
        path.to_string_lossy().into_owned()
    }

    const SUCCESS_SCRIPT: &str = "#!/bin/sh\ncat >/dev/null\necho '{\"result\": \"hi\", \"is_error\": false, \"usage\": {\"input_tokens\": 3, \"output_tokens\": 5}, \"total_cost_usd\": 0.5}'\n";

    #[tokio::test]
    async fn generate_runs_the_fake_binary_end_to_end() {
        let tmp = std::env::temp_dir().join(format!("claude-code-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let binary = write_fake_claude(&tmp, SUCCESS_SCRIPT);
        let adapter = ClaudeCodeAdapter {
            binary: Value::Str(binary),
            default_model: Value::Str(String::new()),
            unset_env: DEFAULT_UNSET_ENV.iter().map(|s| s.to_string()).collect(),
            extra_args: Vec::new(),
        };
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let result = adapter
            .generate(
                "",
                "hello",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap();
        assert_eq!(result.text, "hi");
        assert_eq!(result.tokens_sent, Some(3));
        assert_eq!(result.tokens_received, Some(5));
        assert_eq!(result.cost_usd, Some(0.5));
        assert!(result.warnings.is_empty());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn build_claude_code_defaults_binary_and_model() {
        let adapter = build_claude_code(&Value::None).unwrap();
        assert_eq!(adapter.name(), "claude_code");
    }

    #[test]
    fn build_claude_code_rejects_a_non_list_unset_env() {
        let cfg = dict(&[("unset_env", Value::Str("ANTHROPIC_API_KEY".to_string()))]);
        let err = build(&cfg).unwrap_err();
        assert_eq!(
            err.message,
            "runtime.adapters.claude_code.unset_env must be a list of strings, got 'ANTHROPIC_API_KEY'"
        );
        assert_eq!(err.py_class, PyExc::ValueError);
    }

    #[test]
    fn build_claude_code_defaults_binary_and_model_with_the_plain_constructor() {
        let adapter = build(&Value::None).unwrap();
        assert_eq!(adapter.binary, Value::Str("claude".to_string()));
        assert_eq!(adapter.default_model, Value::Str(String::new()));
        assert_eq!(
            adapter.unset_env,
            vec![
                "ANTHROPIC_API_KEY".to_string(),
                "ANTHROPIC_AUTH_TOKEN".to_string()
            ]
        );
        assert!(adapter.extra_args.is_empty());
    }

    #[test]
    fn build_claude_code_empty_unset_env_list_stays_empty() {
        let cfg = dict(&[("unset_env", Value::List(vec![]))]);
        let adapter = build(&cfg).unwrap();
        assert!(adapter.unset_env.is_empty());
    }

    #[test]
    fn build_claude_code_null_unset_env_means_the_default() {
        let cfg = dict(&[("unset_env", Value::None)]);
        let adapter = build(&cfg).unwrap();
        assert_eq!(
            adapter.unset_env,
            vec![
                "ANTHROPIC_API_KEY".to_string(),
                "ANTHROPIC_AUTH_TOKEN".to_string()
            ]
        );
    }

    #[test]
    fn a_non_string_binary_never_falls_back_to_the_default() {
        let cfg = dict(&[("binary", Value::from(5i64))]);
        let adapter = build(&cfg).unwrap();
        assert_eq!(adapter.binary, Value::from(5i64));
    }

    #[tokio::test]
    async fn check_reports_missing_for_a_name_that_cannot_exist() {
        let adapter = ClaudeCodeAdapter {
            binary: Value::Str("circuitry-test-missing-claude".to_string()),
            default_model: Value::Str(String::new()),
            unset_env: Vec::new(),
            extra_args: Vec::new(),
        };
        let result = adapter.check().await;
        assert!(!result.ok);
        assert_eq!(
            result.missing,
            vec!["binary:circuitry-test-missing-claude".to_string()]
        );
    }

    #[tokio::test]
    async fn check_raises_type_error_text_for_a_non_string_binary() {
        let adapter = ClaudeCodeAdapter {
            binary: Value::from(5i64),
            default_model: Value::Str(String::new()),
            unset_env: Vec::new(),
            extra_args: Vec::new(),
        };
        let result = adapter.check().await;
        assert!(!result.ok);
        assert_eq!(
            result.message,
            Some(
                "check() raised TypeError: expected str, bytes or os.PathLike object, not int"
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn generate_type_errors_on_a_non_string_binary_rather_than_running_the_default() {
        let adapter = ClaudeCodeAdapter {
            binary: Value::from(5i64),
            default_model: Value::Str(String::new()),
            unset_env: Vec::new(),
            extra_args: Vec::new(),
        };
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let err = adapter
            .generate(
                "",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "expected str, bytes or os.PathLike object, not int"
        );
        assert_eq!(err.py_class, PyExc::TypeError);
        assert!(!err.retry.retryable);
    }

    #[tokio::test]
    async fn generate_ignores_every_set_option_with_one_warning() {
        let tmp =
            std::env::temp_dir().join(format!("claude-code-test-opts-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let binary = write_fake_claude(&tmp, SUCCESS_SCRIPT);
        let adapter = ClaudeCodeAdapter {
            binary: Value::Str(binary),
            default_model: Value::Str(String::new()),
            unset_env: Vec::new(),
            extra_args: Vec::new(),
        };
        let options = GenerateOptions {
            temperature: Some(0.5),
            max_tokens: Some(10),
            ..GenerateOptions::default()
        };
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let result = adapter
            .generate("", "hi", &options, &call_ctx(&path, &token))
            .await
            .unwrap();
        assert_eq!(
            result.warnings,
            vec!["adapter 'claude_code' ignored: temperature; max_tokens".to_string()]
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn generate_removes_the_temp_dir_after_a_cli_failure() {
        let script = "#!/bin/sh\ncat >/dev/null\necho 'not json'\nexit 1\n";
        let tmp =
            std::env::temp_dir().join(format!("claude-code-test-fail-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let binary = write_fake_claude(&tmp, script);
        let adapter = ClaudeCodeAdapter {
            binary: Value::Str(binary),
            default_model: Value::Str(String::new()),
            unset_env: Vec::new(),
            extra_args: Vec::new(),
        };
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let err = adapter
            .generate(
                "",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        assert!(
            err.message
                .starts_with("claude exited with code 1 without an answer")
        );
        assert!(!err.retry.retryable);
        std::fs::remove_dir_all(&tmp).ok();
    }
}
