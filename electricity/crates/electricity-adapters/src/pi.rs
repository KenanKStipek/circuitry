//! `adapters/pi.py::PiAdapter`, ported (issue #520 part B) -- one
//! completion through the `pi` coding-agent CLI and its own login,
//! over `electricity_tools::agent_cli` (part A): `pi -p --mode json
//! --no-session --no-tools --no-approve` with the prompt written to
//! `prompt.md` inside a fresh temporary directory and attached as
//! `@<file>` (pi reads no prompt from stdin, which stays an empty,
//! closed pipe). `model` is pi's own `provider/id`; `thinking` its
//! thinking level. Every [`GenerateOptions`] field is ignored, with
//! one warning naming the set ones; `json_schema` is ignored silently
//! (B3 -- pi has no schema flag of its own).

use crate::agent_cli::{
    build_generate_result, check_binary, check_type_error, io_error_to_adapter,
    parse_error_to_adapter, resolve_model, run_error_to_adapter,
};
use crate::config::{
    EnvPairs, cfg_value_or, env_pairs, require_str, resolve_truthy_str, string_list,
};
use async_trait::async_trait;
use electricity_tools::agent_cli::{
    DEFAULT_UNSET_ENV, PiCommandOpts, TempDirGuard, parse_pi_output, pi_command, run_agent_cli,
};
use electricity_value::Value;
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateResult,
};

/// `adapters/pi.py::PiAdapter`. `binary`/`default_model`/`thinking` are
/// kept as the raw post-`cfg.get(...) or default` [`Value`]
/// (`crate::config`'s own doc comment, B5), not a plain `String`.
#[derive(Debug)]
pub struct PiAdapter {
    /// `pi`, or an alias's own name (`adapters/factory.py::_build_alias`).
    name: String,
    binary: Value,
    default_model: Value,
    thinking: Value,
    unset_env: Vec<String>,
    extra_args: Vec<String>,
    /// `env`, applied to the child last; its `Debug` shows names only.
    env: EnvPairs,
}

#[async_trait(?Send)]
impl Adapter for PiAdapter {
    fn name(&self) -> &str {
        &self.name
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
        let resolved_thinking = resolve_truthy_str(&self.thinking)?;
        let workdir = TempDirGuard::new("circuitry-pi-").map_err(io_error_to_adapter)?;
        let prompt_file = workdir.path().join("prompt.md");
        std::fs::write(&prompt_file, prompt).map_err(io_error_to_adapter)?;
        let cmd = pi_command(
            binary,
            &prompt_file.to_string_lossy(),
            &PiCommandOpts {
                model: resolved_model,
                thinking: resolved_thinking,
                extra_args: self.extra_args.clone(),
                ..Default::default()
            },
        );
        let proc = run_agent_cli(
            &cmd,
            &self.unset_env,
            self.env.as_slice(),
            f64::from(call.timeout_seconds),
            Vec::new(),
            Some(workdir.path().to_path_buf()),
            call.token,
        )
        .await
        .map_err(run_error_to_adapter)?;
        let result = parse_pi_output(&proc).map_err(parse_error_to_adapter)?;
        Ok(build_generate_result(&self.name, result, options))
    }

    async fn check(&self) -> CheckResult {
        match require_str(&self.binary) {
            Ok(binary) => check_binary(&self.name, binary),
            Err(err) => check_type_error(&err),
        }
    }
}

/// The pure half of `adapters/factory.py::_build_pi` -- kept separate
/// from [`build_pi`] so a test can inspect the resolved fields
/// directly (`claude_code.rs`'s own `build`, same reasoning).
fn build(name: &str, cfg: &Value) -> Result<PiAdapter, AdapterError> {
    Ok(PiAdapter {
        name: name.to_string(),
        binary: cfg_value_or(cfg, "binary", "pi"),
        default_model: cfg_value_or(cfg, "default_model", ""),
        thinking: cfg_value_or(cfg, "thinking", ""),
        unset_env: string_list(cfg, "unset_env", name, DEFAULT_UNSET_ENV)?,
        extra_args: string_list(cfg, "extra_args", name, &[])?,
        env: env_pairs(cfg, name)?,
    })
}

/// `adapters/factory.py::_build_pi`.
pub fn build_pi(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    build_pi_named("pi", cfg)
}

/// `adapters/factory.py::_build_pi` under *name*: an alias's own name,
/// which is also what it reports as its `meta.adapter`.
pub fn build_pi_named(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(Box::new(build(name, cfg)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_bytecode::EffectPath;
    use electricity_value::{CancellationToken, Dict};
    use electricity_vm::adapter::PyExc;
    use std::io::Write;

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

    /// A fake `pi` script: reads its own attached `@<file>` argument
    /// (the last argv element before the fixed instruction), echoes a
    /// scripted `message_end` event for an assistant reply, and exits
    /// 0 -- enough to drive [`parse_pi_output`] through a real process.
    fn write_fake_pi(dir: &std::path::Path, script: &str) -> String {
        let path = dir.join("pi");
        // Written by a short-lived child process rather than by this test
        // process: if another test thread forked while this process held
        // the file open for writing, the forked child would hold that
        // handle until its own exec, and exec'ing the script in the
        // meantime fails with ETXTBSY ("Text file busy") on Linux. A
        // child's handle is never inherited by this process's forks.
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("cat > \"$1\" && chmod 700 \"$1\"")
            .arg("sh")
            .arg(&path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success(), "writing the fake failed");
        path.to_string_lossy().into_owned()
    }

    const SUCCESS_SCRIPT: &str = "#!/bin/sh\necho '{\"type\": \"message_end\", \"message\": {\"role\": \"assistant\", \"content\": \"hi\", \"stopReason\": \"end_turn\", \"usage\": {\"input\": 3, \"output\": 5, \"cost\": 0.25}}}'\n";

    /// A fake `pi` that writes its own `cwd` to the path named by a
    /// `--capture-cwd=<path>` argument (an `extra_args` entry, never
    /// interpreted by the Rust side) before doing anything else -- the
    /// `claude_code.rs` tests' own helper, mirrored here (`pi` never
    /// reads stdin, unlike `claude`, so there is no leading `cat
    /// >/dev/null`).
    fn capture_cwd_script(behaviour: &str) -> String {
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do\n  case \"$a\" in\n    --capture-cwd=*) pwd > \"${{a#--capture-cwd=}}\" ;;\n  esac\ndone\n{behaviour}\n"
        )
    }

    const CAPTURE_SUCCESS: &str = "echo '{\"type\": \"message_end\", \"message\": {\"role\": \"assistant\", \"content\": \"hi\"}}'";
    const CAPTURE_FAILURE: &str = "exit 1";
    const CAPTURE_HANG: &str = "sleep 30";

    fn capture_extra_args(path: &std::path::Path) -> Vec<String> {
        vec![format!("--capture-cwd={}", path.display())]
    }

    /// The directory [`capture_cwd_script`] reported, asserted non-empty
    /// and gone -- `TempDirGuard`'s own `Drop` removes it on every way
    /// out of `generate`.
    fn assert_captured_dir_removed(capture: &std::path::Path) {
        let captured = std::fs::read_to_string(capture).unwrap();
        let captured_dir = captured.trim();
        assert!(!captured_dir.is_empty(), "the fake never captured a cwd");
        assert!(
            !std::path::Path::new(captured_dir).exists(),
            "temp dir {captured_dir} must be removed, but still exists"
        );
    }

    #[tokio::test]
    async fn generate_runs_the_fake_binary_end_to_end() {
        let tmp = std::env::temp_dir().join(format!("pi-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let binary = write_fake_pi(&tmp, SUCCESS_SCRIPT);
        let adapter = PiAdapter {
            name: "pi".to_string(),
            env: EnvPairs::default(),
            binary: Value::Str(binary),
            default_model: Value::Str(String::new()),
            thinking: Value::Str(String::new()),
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
        assert_eq!(result.cost_usd, Some(0.25));
        assert_eq!(result.finish_reason, Some("end_turn".to_string()));
        assert!(result.warnings.is_empty());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn generate_ignores_json_schema_silently() {
        let tmp = std::env::temp_dir().join(format!("pi-test-schema-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let binary = write_fake_pi(&tmp, SUCCESS_SCRIPT);
        let adapter = PiAdapter {
            name: "pi".to_string(),
            env: EnvPairs::default(),
            binary: Value::Str(binary),
            default_model: Value::Str(String::new()),
            thinking: Value::Str(String::new()),
            unset_env: Vec::new(),
            extra_args: Vec::new(),
        };
        let options = GenerateOptions {
            json_schema: Some(Value::Dict(Dict::new())),
            ..GenerateOptions::default()
        };
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let result = adapter
            .generate("", "hi", &options, &call_ctx(&path, &token))
            .await
            .unwrap();
        assert!(result.warnings.is_empty());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn build_pi_defaults_binary_model_and_thinking() {
        let adapter = build("pi", &Value::None).unwrap();
        assert_eq!(adapter.binary, Value::Str("pi".to_string()));
        assert_eq!(adapter.default_model, Value::Str(String::new()));
        assert_eq!(adapter.thinking, Value::Str(String::new()));
        assert_eq!(
            adapter.unset_env,
            vec![
                "ANTHROPIC_API_KEY".to_string(),
                "ANTHROPIC_AUTH_TOKEN".to_string()
            ]
        );
    }

    #[test]
    fn build_pi_rejects_a_non_list_extra_args() {
        let cfg = dict(&[("extra_args", Value::from(1i64))]);
        let err = build("pi", &cfg).unwrap_err();
        assert_eq!(
            err.message,
            "runtime.adapters.pi.extra_args must be a list of strings, got 1"
        );
        assert_eq!(err.py_class, PyExc::ValueError);
    }

    #[test]
    fn build_pi_custom_unset_env_replaces_the_default() {
        let cfg = dict(&[(
            "unset_env",
            Value::List(vec![Value::Str("AGENT_CLI_FAKE_DROP".to_string())]),
        )]);
        let adapter = build("pi", &cfg).unwrap();
        assert_eq!(adapter.unset_env, vec!["AGENT_CLI_FAKE_DROP".to_string()]);
    }

    #[test]
    fn a_non_string_thinking_never_falls_back_to_the_default() {
        let cfg = dict(&[("thinking", Value::Bool(true))]);
        let adapter = build("pi", &cfg).unwrap();
        assert_eq!(adapter.thinking, Value::Bool(true));
    }

    #[tokio::test]
    async fn check_reports_missing_for_a_name_that_cannot_exist() {
        let adapter = PiAdapter {
            name: "pi".to_string(),
            env: EnvPairs::default(),
            binary: Value::Str("circuitry-test-missing-pi".to_string()),
            default_model: Value::Str(String::new()),
            thinking: Value::Str(String::new()),
            unset_env: Vec::new(),
            extra_args: Vec::new(),
        };
        let result = adapter.check().await;
        assert!(!result.ok);
        assert_eq!(
            result.missing,
            vec!["binary:circuitry-test-missing-pi".to_string()]
        );
    }

    #[tokio::test]
    async fn check_raises_type_error_text_for_a_non_string_binary() {
        let adapter = PiAdapter {
            name: "pi".to_string(),
            env: EnvPairs::default(),
            binary: Value::Bool(true),
            default_model: Value::Str(String::new()),
            thinking: Value::Str(String::new()),
            unset_env: Vec::new(),
            extra_args: Vec::new(),
        };
        let result = adapter.check().await;
        assert!(!result.ok);
        assert_eq!(
            result.message,
            Some(
                "check() raised TypeError: expected str, bytes or os.PathLike object, not bool"
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn generate_type_errors_on_a_truthy_non_string_thinking() {
        let tmp = std::env::temp_dir().join(format!("pi-test-thinking-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let binary = write_fake_pi(&tmp, SUCCESS_SCRIPT);
        let adapter = PiAdapter {
            name: "pi".to_string(),
            env: EnvPairs::default(),
            binary: Value::Str(binary),
            default_model: Value::Str(String::new()),
            thinking: Value::Bool(true),
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
            "expected str, bytes or os.PathLike object, not bool"
        );
        assert_eq!(err.py_class, PyExc::TypeError);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn generate_removes_the_temp_dir_after_success() {
        let tmp = std::env::temp_dir().join(format!("pi-test-tempdir-ok-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let binary = write_fake_pi(&tmp, &capture_cwd_script(CAPTURE_SUCCESS));
        let capture = tmp.join("cwd.txt");
        let adapter = PiAdapter {
            name: "pi".to_string(),
            env: EnvPairs::default(),
            binary: Value::Str(binary),
            default_model: Value::Str(String::new()),
            thinking: Value::Str(String::new()),
            unset_env: Vec::new(),
            extra_args: capture_extra_args(&capture),
        };
        let token = CancellationToken::new();
        let path = EffectPath::root();
        adapter
            .generate(
                "",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap();
        assert_captured_dir_removed(&capture);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn generate_removes_the_temp_dir_after_a_cli_failure() {
        let tmp = std::env::temp_dir().join(format!("pi-test-fail-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let binary = write_fake_pi(&tmp, &capture_cwd_script(CAPTURE_FAILURE));
        let capture = tmp.join("cwd.txt");
        let adapter = PiAdapter {
            name: "pi".to_string(),
            env: EnvPairs::default(),
            binary: Value::Str(binary),
            default_model: Value::Str(String::new()),
            thinking: Value::Str(String::new()),
            unset_env: Vec::new(),
            extra_args: capture_extra_args(&capture),
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
                .starts_with("pi exited with code 1 without an answer")
        );
        assert_captured_dir_removed(&capture);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn generate_removes_the_temp_dir_after_a_timeout() {
        let tmp = std::env::temp_dir().join(format!("pi-test-timeout-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let binary = write_fake_pi(&tmp, &capture_cwd_script(CAPTURE_HANG));
        let capture = tmp.join("cwd.txt");
        let adapter = PiAdapter {
            name: "pi".to_string(),
            env: EnvPairs::default(),
            binary: Value::Str(binary),
            default_model: Value::Str(String::new()),
            thinking: Value::Str(String::new()),
            unset_env: Vec::new(),
            extra_args: capture_extra_args(&capture),
        };
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let call = CallContext {
            path: &path,
            // Long enough for the fake to start and write its capture
            // file even on a loaded machine; it hangs far longer.
            timeout_seconds: 5,
            token: &token,
        };
        let err = adapter
            .generate("", "hi", &GenerateOptions::default(), &call)
            .await
            .unwrap_err();
        assert!(err.retry.retryable);
        assert_captured_dir_removed(&capture);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn generate_removes_the_temp_dir_when_the_future_is_dropped() {
        let tmp = std::env::temp_dir().join(format!("pi-test-dropped-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let binary = write_fake_pi(&tmp, &capture_cwd_script(CAPTURE_HANG));
        let capture = tmp.join("cwd.txt");
        let adapter = PiAdapter {
            name: "pi".to_string(),
            env: EnvPairs::default(),
            binary: Value::Str(binary),
            default_model: Value::Str(String::new()),
            thinking: Value::Str(String::new()),
            unset_env: Vec::new(),
            extra_args: capture_extra_args(&capture),
        };
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let call = call_ctx(&path, &token);
        let options = GenerateOptions::default();
        let mut fut = Box::pin(adapter.generate("", "hi", &options, &call));
        // Drop the future only once the fake has started and written its
        // capture file: a fixed delay raced the fake's own startup on a
        // loaded machine.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            tokio::select! {
                result = &mut fut => panic!(
                    "must not finish before the capture file is written: {result:?}"
                ),
                () = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
            }
            let written = std::fs::read_to_string(&capture).is_ok_and(|c| !c.trim().is_empty());
            if written {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the fake never wrote its capture file"
            );
        }
        drop(fut);
        assert_captured_dir_removed(&capture);
        std::fs::remove_dir_all(&tmp).ok();
    }
}
