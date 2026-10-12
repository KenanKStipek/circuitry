//! `plugins/_subprocess.py::GenericSubprocessTool`, ported -- the one
//! pass-through [`crate::ToolPlugin`] shape `awk`/`imagemagick`/
//! `ffmpeg` (and every other binary-wrapping plugin Circuitry has)
//! share: forward `params.args` to a named binary, `params.cwd`/
//! `params.stdin`/`params.allow_nonzero` as the only other knobs.
//! `binary`/`env` come from `runtime.plugins.<name>` config
//! (`plugin_binary_override`/`plugin_env_override`), never `params` --
//! a machine-specific executable path or thread-limit variable belongs
//! in config, not in orchestration YAML.

use crate::process::{self, ProcError, RunOpts};
use crate::{CheckResult, ErrorCause, PyExc, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::pycompat::py_strip;
use electricity_value::{CancellationToken, Dict, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// One instance per binary-wrapping plugin file (`awk.rs`,
/// `imagemagick.rs`, `ffmpeg.rs`, and lane C's own later additions),
/// each supplying its own *name*/*binary_candidates* -- `plugins/
/// _subprocess.py::GenericSubprocessTool`'s own four fields.
pub struct GenericSubprocessTool {
    pub name: String,
    pub binary_candidates: Vec<String>,
    pub binary: Option<String>,
    pub env: Option<HashMap<String, String>>,
}

#[async_trait(?Send)]
impl ToolPlugin for GenericSubprocessTool {
    fn name(&self) -> &str {
        &self.name
    }

    async fn execute(&self, params: Value, call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        let dict = params.as_dict();
        let args = match dict.and_then(|d| d.get(&Value::Str("args".to_string()))) {
            Some(Value::List(items)) => items.as_slice(),
            _ => {
                return Err(value_error(format!(
                    "{}: params['args'] must be a list of strings.",
                    self.name
                )));
            }
        };
        let candidates: Vec<&str> = self.binary_candidates.iter().map(String::as_str).collect();
        let binary = resolve_plugin_binary(&self.name, &candidates, self.binary.as_deref())?;
        let cwd = dict
            .and_then(|d| d.get(&Value::Str("cwd".to_string())))
            .and_then(Value::as_str);
        let stdin = dict
            .and_then(|d| d.get(&Value::Str("stdin".to_string())))
            .and_then(Value::as_str);
        let allow_nonzero =
            as_bool_param(dict.and_then(|d| d.get(&Value::Str("allow_nonzero".to_string()))));
        let env = merged_env(self.env.as_ref());
        let hint = format!("set runtime.plugins.{}.binary", self.name);

        run_binary(
            &binary,
            args,
            cwd,
            stdin,
            env,
            call.timeout_seconds,
            allow_nonzero,
            &hint,
            call.armed,
            call.token,
        )
        .await
    }

    fn check(&self) -> CheckResult {
        let candidates: Vec<&str> = self.binary_candidates.iter().map(String::as_str).collect();
        check_binary(
            &candidates,
            Some(&self.name),
            self.binary.as_deref(),
            Some(&self.name),
        )
    }
}

/// `plugins/_subprocess.py::plugin_binary_override` -- `cfg.get
/// ("binary")`; `~` expansion is deferred to resolution time
/// ([`resolve_plugin_binary`]).
pub fn plugin_binary_override(cfg: &Value) -> Option<String> {
    let value = cfg.as_dict()?.get(&Value::Str("binary".to_string()))?;
    if is_truthy(value) {
        Some(value.py_str())
    } else {
        None
    }
}

/// `plugins/_subprocess.py::plugin_env_override` -- the mapping-shape
/// validation and its exact error text.
pub fn plugin_env_override(
    cfg: &Value,
    plugin_name: &str,
) -> Result<Option<HashMap<String, String>>, ToolError> {
    let Some(value) = cfg
        .as_dict()
        .and_then(|d| d.get(&Value::Str("env".to_string())))
    else {
        return Ok(None);
    };
    if !is_truthy(value) {
        return Ok(None);
    }
    let Value::Dict(dict) = value else {
        return Err(ToolError {
            py_class: PyExc::ValueError,
            message: format!(
                "runtime.plugins.{plugin_name}.env must be a mapping of environment variable \
                 names to values, got {}.",
                value.type_name()
            ),
            cause: ErrorCause::None,
        });
    };
    let mut map = HashMap::with_capacity(dict.len());
    for (k, v) in dict {
        map.insert(k.py_str(), v.py_str());
    }
    Ok(Some(map))
}

/// `plugins/_subprocess.py::merged_env` -- `{**os.environ, **
/// overrides}`, or inherit unchanged (`None`) when there are no
/// configured overrides.
pub(crate) fn merged_env(
    overrides: Option<&HashMap<String, String>>,
) -> Option<HashMap<String, String>> {
    let overrides = overrides?;
    if overrides.is_empty() {
        return None;
    }
    // `vars_os()`, not `vars()` -- `std::env::vars()` panics while
    // iterating the moment any inherited variable isn't valid UTF-8
    // (review finding 4 on PR #467), reachable whenever a
    // `runtime.plugins.{awk,imagemagick,ffmpeg}.env` override is
    // configured at all, on a process whose own environment holds any
    // such variable (CPython copes by decoding with surrogateescape;
    // a variable that round-trip only matters if a plugin's own
    // argv/output ever needed to reproduce its *exact* bytes, which
    // none of this lane's four tools do). A variable that still isn't
    // valid UTF-8 after this is simply dropped from the inherited set
    // rather than panicking -- safe to skip: the overwhelming common
    // case (every variable is valid UTF-8) is unaffected, and the
    // configured *overrides* themselves (already `String`, never
    // `OsString`) are merged in afterwards regardless.
    let mut merged: HashMap<String, String> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect();
    merged.extend(overrides.iter().map(|(k, v)| (k.clone(), v.clone())));
    Some(merged)
}

/// `plugins/_subprocess.py::_expand_and_validate` -- `~` expansion plus
/// the absolute/executable validation; returns `(resolved_path, None)`
/// when it's fine to run, else `(resolved_path, Some(reason))`.
fn expand_and_validate(path_str: &str) -> (String, Option<&'static str>) {
    let expanded = expand_user(path_str);
    let path = Path::new(&expanded);
    if !path.is_absolute() {
        return (expanded, Some("must be an absolute path"));
    }
    if !(path.is_file() && is_executable(path)) {
        return (expanded, Some("does not exist or is not executable"));
    }
    (expanded, None)
}

fn expand_user(path_str: &str) -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return path_str.to_string();
    };
    if path_str == "~" {
        return home.to_string_lossy().into_owned();
    }
    if let Some(rest) = path_str.strip_prefix("~/") {
        let mut p = PathBuf::from(home);
        p.push(rest);
        return p.to_string_lossy().into_owned();
    }
    path_str.to_string()
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

/// `plugins/_subprocess.py::resolve_plugin_binary` -- a configured
/// `binary` overrides the `PATH` search entirely and must exist and be
/// executable, or this fails naming the setting and the path; unset,
/// falls back to the first of *candidates* found on `PATH`.
pub(crate) fn resolve_plugin_binary(
    plugin_name: &str,
    candidates: &[&str],
    configured: Option<&str>,
) -> Result<String, ToolError> {
    if let Some(configured) = configured {
        let (path, error) = expand_and_validate(configured);
        return match error {
            None => Ok(path),
            Some(error) => Err(ToolError {
                py_class: PyExc::RuntimeError,
                message: format!(
                    "{plugin_name}: configured runtime.plugins.{plugin_name}.binary={} \
                     {error} (resolved: {path}).",
                    Value::Str(configured.to_string()).py_repr()
                ),
                cause: ErrorCause::None,
            }),
        };
    }
    match process::resolve_binary(candidates) {
        Some(binary) => Ok(binary),
        None => Err(ToolError {
            py_class: PyExc::RuntimeError,
            message: format!(
                "{plugin_name}: none of {} found on PATH. Install one of them, or set \
                 runtime.plugins.{plugin_name}.binary.",
                str_list_repr(candidates)
            ),
            cause: ErrorCause::None,
        }),
    }
}

/// `plugins/_subprocess.py::check_binary` -- standard preflight: report
/// `binary:<first-candidate>` missing when none of *candidates* are
/// available on `PATH`, or -- when a `binary` override is configured --
/// when that path doesn't exist or isn't executable.
pub(crate) fn check_binary(
    candidates: &[&str],
    label: Option<&str>,
    configured: Option<&str>,
    plugin_name: Option<&str>,
) -> CheckResult {
    let primary = label.unwrap_or_else(|| candidates.first().copied().unwrap_or("?"));
    if let Some(configured) = configured {
        let (path, error) = expand_and_validate(configured);
        return match error {
            None => CheckResult {
                ok: true,
                missing: Vec::new(),
                message: None,
            },
            Some(error) => CheckResult {
                ok: false,
                missing: vec![format!("binary:{primary}")],
                message: Some(format!(
                    "configured runtime.plugins.{primary}.binary={} {error} (resolved: {path}).",
                    Value::Str(configured.to_string()).py_repr()
                )),
            },
        };
    }
    if process::resolve_binary(candidates).is_some() {
        return CheckResult {
            ok: true,
            missing: Vec::new(),
            message: None,
        };
    }
    let message = if let Some(plugin_name) = plugin_name {
        Some(format!(
            "none of {} found on PATH. Install one of them, or set \
             runtime.plugins.{plugin_name}.binary.",
            str_list_repr(candidates)
        ))
    } else if candidates.len() > 1 {
        Some(format!(
            "none of {} found on PATH. Install one of them.",
            str_list_repr(candidates)
        ))
    } else {
        None
    };
    CheckResult {
        ok: false,
        missing: vec![format!("binary:{primary}")],
        message,
    }
}

fn str_list_repr(items: &[&str]) -> String {
    Value::List(items.iter().map(|s| Value::from(*s)).collect()).py_repr()
}

/// `subprocess args[{i}] contains null byte.` -- `plugins/
/// _subprocess.py::_validate_args`'s own `str(a)` conversion plus
/// null-byte rejection.
fn validate_args(args: &[Value]) -> Result<Vec<String>, ToolError> {
    let mut out = Vec::with_capacity(args.len());
    for (i, a) in args.iter().enumerate() {
        let s = a.py_str();
        if s.contains('\0') {
            return Err(value_error(format!(
                "subprocess args[{i}] contains null byte."
            )));
        }
        out.push(s);
    }
    Ok(out)
}

/// `plugins/_subprocess.py::run_binary` -- shared by `GenericSubprocessTool`
/// and `ShellPlugin` (`shell.py` calls this same function directly).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_binary(
    binary: &str,
    args: &[Value],
    cwd: Option<&str>,
    stdin: Option<&str>,
    env: Option<HashMap<String, String>>,
    timeout_seconds: u32,
    allow_nonzero: bool,
    not_found_hint: &str,
    armed: bool,
    token: &CancellationToken,
) -> Result<ToolResult, ToolError> {
    let validated_args = validate_args(args)?;
    let mut cmd = Vec::with_capacity(validated_args.len() + 1);
    cmd.push(binary.to_string());
    cmd.extend(validated_args.iter().cloned());

    let opts = RunOpts {
        timeout: Some(Duration::from_secs(u64::from(timeout_seconds))),
        input: stdin.map(|s| s.as_bytes().to_vec()),
        cwd: cwd.map(PathBuf::from),
        env,
        new_session: armed,
        env_remove: Vec::new(),
    };

    let completed = match process::run_tracked(&cmd, &opts, token).await {
        Ok(completed) => completed,
        Err(ProcError::NotFound(_)) => {
            return Err(ToolError {
                py_class: PyExc::RuntimeError,
                message: format!(
                    "binary not found: {} (install it or {not_found_hint}).",
                    Value::Str(binary.to_string()).py_repr()
                ),
                cause: ErrorCause::None,
            });
        }
        Err(err @ ProcError::Timeout { .. }) => {
            return Err(ToolError {
                py_class: PyExc::RuntimeError,
                message: err.to_string(),
                cause: ErrorCause::None,
            });
        }
        Err(ProcError::Decode(message)) => {
            return Err(ToolError {
                py_class: PyExc::UnicodeDecodeError,
                message,
                cause: ErrorCause::None,
            });
        }
        Err(ProcError::Cancelled) => {
            return Err(ToolError::message("cancelled"));
        }
        Err(ProcError::Other { py_class, message }) => {
            return Err(ToolError {
                py_class,
                message,
                cause: ErrorCause::None,
            });
        }
    };

    if completed.exit_code != 0 && !allow_nonzero {
        let primary = if !completed.stderr.is_empty() {
            &completed.stderr
        } else {
            &completed.stdout
        };
        let err_text = py_strip(primary);
        return Err(ToolError {
            py_class: PyExc::RuntimeError,
            message: format!("{binary} failed (exit {}): {err_text}", completed.exit_code),
            cause: ErrorCause::None,
        });
    }

    let mut raw = Dict::new();
    raw.insert(
        Value::from("args"),
        Value::List(validated_args.into_iter().map(Value::from).collect()),
    );
    raw.insert(
        Value::from("cwd"),
        cwd.map(Value::from).unwrap_or(Value::None),
    );
    raw.insert(Value::from("binary"), Value::from(binary));

    Ok(ToolResult {
        value: Value::from(completed.stdout.clone()),
        raw: Value::Dict(raw),
        stdout: Some(completed.stdout),
        stderr: Some(completed.stderr),
        exit_code: Some(completed.exit_code),
        ok: true,
    })
}

/// `plugins/base.py::_as_bool` -- string-aware boolean coercion for a
/// tool param (Mustache renders a templated `false` as the literal
/// string `"False"`, and Python's `bool("False")` is `True`).
pub(crate) fn as_bool_param(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::None) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Str(s)) => {
            let normalized = py_strip(s).to_lowercase();
            !matches!(normalized.as_str(), "false" | "0" | "no" | "off" | "n" | "")
        }
        Some(other) => is_truthy(other),
    }
}

/// Python truthiness (`bool(value)`), as far as this module's own
/// config-reading helpers need it.
pub(crate) fn is_truthy(value: &Value) -> bool {
    match value {
        Value::None => false,
        Value::Bool(b) => *b,
        Value::Int(i) => !i.is_zero(),
        Value::Float(f) => *f != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::Bytes(b) => !b.is_empty(),
        Value::List(items) => !items.is_empty(),
        Value::Dict(d) => !d.is_empty(),
        Value::Date(_) | Value::DateTime(..) => true,
    }
}

fn value_error(message: impl Into<String>) -> ToolError {
    ToolError {
        py_class: PyExc::ValueError,
        message: message.into(),
        cause: ErrorCause::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict as DictType;

    #[test]
    fn plugin_binary_override_reads_the_configured_binary() {
        let mut cfg = DictType::new();
        cfg.insert(Value::from("binary"), Value::from("/usr/bin/awk"));
        assert_eq!(
            plugin_binary_override(&Value::Dict(cfg)),
            Some("/usr/bin/awk".to_string())
        );
    }

    #[test]
    fn plugin_binary_override_is_none_for_an_absent_or_falsy_value() {
        assert_eq!(plugin_binary_override(&Value::Dict(DictType::new())), None);
        let mut cfg = DictType::new();
        cfg.insert(Value::from("binary"), Value::from(""));
        assert_eq!(plugin_binary_override(&Value::Dict(cfg)), None);
    }

    #[test]
    fn plugin_env_override_rejects_a_non_mapping() {
        let mut cfg = DictType::new();
        cfg.insert(Value::from("env"), Value::from("nope"));
        let err = plugin_env_override(&Value::Dict(cfg), "awk").unwrap_err();
        assert_eq!(
            err.message,
            "runtime.plugins.awk.env must be a mapping of environment variable names to \
             values, got str."
        );
        assert_eq!(err.py_class, PyExc::ValueError);
    }

    #[test]
    fn plugin_env_override_converts_keys_and_values_with_str() {
        let mut env = DictType::new();
        env.insert(Value::from("A"), Value::from(1i64));
        let mut cfg = DictType::new();
        cfg.insert(Value::from("env"), Value::Dict(env));
        let overrides = plugin_env_override(&Value::Dict(cfg), "awk")
            .unwrap()
            .unwrap();
        assert_eq!(overrides.get("A"), Some(&"1".to_string()));
    }

    #[test]
    fn as_bool_param_treats_the_templated_string_false_as_false() {
        assert!(!as_bool_param(Some(&Value::from("False"))));
        assert!(!as_bool_param(Some(&Value::from("no"))));
        assert!(as_bool_param(Some(&Value::from("yes"))));
        assert!(!as_bool_param(None));
    }

    #[test]
    fn resolve_plugin_binary_reports_a_bad_configured_path() {
        let err = resolve_plugin_binary("awk", &["awk"], Some("relative/path")).unwrap_err();
        assert!(err.message.contains("must be an absolute path"));
    }

    #[test]
    fn resolve_plugin_binary_reports_none_found_on_path() {
        let err =
            resolve_plugin_binary("awk", &["definitely-not-a-real-binary"], None).unwrap_err();
        assert_eq!(
            err.message,
            "awk: none of ['definitely-not-a-real-binary'] found on PATH. Install one of \
             them, or set runtime.plugins.awk.binary."
        );
    }

    #[derive(serde::Deserialize)]
    struct PluginEnvOverrideCase {
        cfg: std::collections::BTreeMap<String, serde_json::Value>,
        expected_error: Option<String>,
        expected_env: Option<std::collections::BTreeMap<String, String>>,
    }

    #[derive(serde::Deserialize)]
    struct ResolvePluginBinaryCase {
        plugin_name: String,
        candidates: Vec<String>,
        configured: Option<String>,
        expected_error: String,
    }

    #[derive(serde::Deserialize)]
    struct CheckBinaryCase {
        candidates: Vec<String>,
        label: String,
        configured: Option<String>,
        plugin_name: String,
        expected_ok: bool,
        expected_missing: Vec<String>,
        expected_message: Option<String>,
    }

    #[derive(serde::Deserialize)]
    struct StrConversionCase {
        value: serde_json::Value,
        expected: String,
    }

    #[derive(serde::Deserialize)]
    struct ProcessToolsCorpus {
        plugin_env_override_cases: Vec<PluginEnvOverrideCase>,
        resolve_plugin_binary_cases: Vec<ResolvePluginBinaryCase>,
        check_binary_cases: Vec<CheckBinaryCase>,
        str_conversion_cases: Vec<StrConversionCase>,
    }

    fn corpus() -> ProcessToolsCorpus {
        serde_json::from_str(include_str!("../tests/golden/process_tools_corpus.json"))
            .expect("process_tools_corpus.json must parse")
    }

    fn json_to_value(value: &serde_json::Value) -> Value {
        match value {
            serde_json::Value::Null => Value::None,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::from(i)
                } else {
                    Value::Float(n.as_f64().unwrap_or_default())
                }
            }
            serde_json::Value::String(s) => Value::from(s.as_str()),
            serde_json::Value::Array(items) => {
                Value::List(items.iter().map(json_to_value).collect())
            }
            serde_json::Value::Object(map) => {
                let mut dict = Dict::new();
                for (k, v) in map {
                    dict.insert(Value::from(k.as_str()), json_to_value(v));
                }
                Value::Dict(dict)
            }
        }
    }

    #[test]
    fn plugin_env_override_matches_circuitrys_own_messages() {
        for case in corpus().plugin_env_override_cases {
            let mut cfg = Dict::new();
            for (k, v) in &case.cfg {
                cfg.insert(Value::from(k.as_str()), json_to_value(v));
            }
            let result = plugin_env_override(&Value::Dict(cfg), "awk");
            match case.expected_error {
                Some(expected) => {
                    assert_eq!(result.unwrap_err().message, expected, "cfg={:?}", case.cfg)
                }
                None => {
                    let got: Option<std::collections::BTreeMap<String, String>> =
                        result.unwrap().map(|m| m.into_iter().collect());
                    assert_eq!(got, case.expected_env, "cfg={:?}", case.cfg);
                }
            }
        }
    }

    #[test]
    fn resolve_plugin_binary_matches_circuitrys_own_messages() {
        for case in corpus().resolve_plugin_binary_cases {
            let candidates: Vec<&str> = case.candidates.iter().map(String::as_str).collect();
            let err =
                resolve_plugin_binary(&case.plugin_name, &candidates, case.configured.as_deref())
                    .unwrap_err();
            assert_eq!(err.message, case.expected_error);
        }
    }

    #[test]
    fn str_conversion_matches_pythons_own_str_builtin() {
        // `_validate_args`'s own `str(a)` conversion, ported as
        // `Value::py_str`.
        for case in corpus().str_conversion_cases {
            let value = json_to_value(&case.value);
            assert_eq!(value.py_str(), case.expected, "value={:?}", case.value);
        }
    }

    #[test]
    fn check_binary_matches_circuitrys_own_messages() {
        for case in corpus().check_binary_cases {
            let candidates: Vec<&str> = case.candidates.iter().map(String::as_str).collect();
            let result = check_binary(
                &candidates,
                Some(&case.label),
                case.configured.as_deref(),
                Some(&case.plugin_name),
            );
            assert_eq!(result.ok, case.expected_ok);
            assert_eq!(result.missing, case.expected_missing);
            assert_eq!(result.message, case.expected_message);
        }
    }

    #[tokio::test]
    async fn run_binary_reports_not_found_with_the_given_hint() {
        let token = CancellationToken::new();
        let err = run_binary(
            "definitely-not-a-real-binary",
            &[],
            None,
            None,
            None,
            30,
            false,
            "set runtime.plugins.awk.binary",
            false,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.message,
            "binary not found: 'definitely-not-a-real-binary' (install it or set \
             runtime.plugins.awk.binary)."
        );
    }

    #[tokio::test]
    async fn run_binary_reports_a_not_a_directory_os_error_for_a_cwd_that_is_a_file() {
        // Review finding 3 on PR #467: a spawn failure other than a
        // missing binary (here, `cwd` naming a plain file, not a
        // directory -- `ENOTDIR`) must surface as its own `OSError`
        // text/class, never collapse into the same "cancelled" text as
        // an actually-cancelled run.
        let token = CancellationToken::new();
        let tmp = std::env::temp_dir().join(format!(
            "electricity-tools-not-a-dir-test-{}",
            std::process::id()
        ));
        std::fs::write(&tmp, "").unwrap();
        let cwd = tmp.to_str().unwrap().to_string();
        // `armed: false` -- this must hold for an unarmed/embedded call
        // too, not only a CLI-driven one: `run_tracked` installs its
        // own `pre_exec` unconditionally, forcing Rust's std off the
        // `posix_spawn(2)` fast path, which (confirmed directly on this
        // platform) can only report a generic `NotFound`/`ENOENT` for
        // *any* `current_dir` failure rather than the real
        // `NotADirectoryError`/`ENOTDIR`.
        let err = run_binary(
            "echo",
            &[],
            Some(&cwd),
            None,
            None,
            30,
            false,
            "set runtime.plugins.awk.binary",
            false,
            &token,
        )
        .await
        .unwrap_err();
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(err.py_class, PyExc::NotADirectoryError);
        assert!(
            err.message.contains("Not a directory") && err.message.contains(&cwd),
            "message={}",
            err.message
        );
        assert_ne!(err.message, "cancelled");
    }

    #[tokio::test]
    async fn run_binary_rejects_a_null_byte_in_args() {
        let token = CancellationToken::new();
        let err = run_binary(
            "true",
            &[Value::from("a\0b")],
            None,
            None,
            None,
            30,
            false,
            "set runtime.plugins.awk.binary",
            false,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(err.message, "subprocess args[0] contains null byte.");
    }

    #[tokio::test]
    async fn run_binary_reports_nonzero_exit_using_stderr_then_stdout() {
        let token = CancellationToken::new();
        let err = run_binary(
            "sh",
            &[Value::from("-c"), Value::from("printf oops 1>&2; exit 1")],
            None,
            None,
            None,
            30,
            false,
            "set runtime.plugins.awk.binary",
            false,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(err.message, "sh failed (exit 1): oops");
    }

    #[test]
    #[cfg(unix)]
    fn merged_env_does_not_panic_on_a_non_utf8_inherited_variable() {
        // Review finding 4 on PR #467: `std::env::vars()` panics while
        // iterating the moment *any* inherited variable isn't valid
        // UTF-8, reachable whenever a `runtime.plugins.<name>.env`
        // override is configured at all.
        use std::os::unix::ffi::OsStrExt;
        // SAFETY: a single-threaded test; `remove_var` on teardown
        // undoes this before any other test can observe it.
        unsafe {
            std::env::set_var(
                "ELECTRICITY_TOOLS_TEST_BAD_UTF8",
                std::ffi::OsStr::from_bytes(&[0xff, 0xfe]),
            );
        }
        let mut overrides = HashMap::new();
        overrides.insert("EXTRA".to_string(), "1".to_string());
        let result = std::panic::catch_unwind(|| merged_env(Some(&overrides)));
        unsafe {
            std::env::remove_var("ELECTRICITY_TOOLS_TEST_BAD_UTF8");
        }
        let merged = result
            .expect("merged_env must not panic on a non-UTF-8 env var")
            .unwrap();
        assert_eq!(merged.get("EXTRA"), Some(&"1".to_string()));
        assert!(!merged.contains_key("ELECTRICITY_TOOLS_TEST_BAD_UTF8"));
    }

    #[tokio::test]
    async fn run_binary_captures_the_result_shape_on_success() {
        let token = CancellationToken::new();
        let result = run_binary(
            "echo",
            &[Value::from("hi")],
            None,
            None,
            None,
            30,
            false,
            "set runtime.plugins.awk.binary",
            false,
            &token,
        )
        .await
        .unwrap();
        assert_eq!(result.value, Value::from("hi\n"));
        assert_eq!(result.exit_code, Some(0));
        let raw = result.raw.as_dict().unwrap();
        assert_eq!(
            raw.keys().map(Value::py_str).collect::<Vec<_>>(),
            vec!["args", "cwd", "binary"]
        );
    }

    #[tokio::test]
    async fn run_binary_reports_nonzero_exit_falling_back_to_stdout_when_stderr_is_empty() {
        // `err = (stderr or stdout or "").strip()` -- the fallback half
        // (issue #459's own test plan: "the stderr-first message and
        // the fallback to stdout"); the existing
        // `run_binary_reports_nonzero_exit_using_stderr_then_stdout`
        // test only ever exercises the stderr-present half.
        let token = CancellationToken::new();
        let err = run_binary(
            "sh",
            &[Value::from("-c"), Value::from("printf oops; exit 1")],
            None,
            None,
            None,
            30,
            false,
            "set runtime.plugins.awk.binary",
            false,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(err.message, "sh failed (exit 1): oops");
    }

    #[tokio::test]
    async fn run_binary_reports_not_found_for_a_missing_cwd() {
        // `plugins/_subprocess.py::run_binary`'s own "a FileNotFoundError
        // from spawning (missing binary **or a missing cwd**) gives
        // `binary not found: ...`" bullet (issue #459) -- a missing
        // `cwd` is `ENOENT`, the same `FileNotFoundError` a missing
        // binary itself raises, unlike `cwd` naming a *file*
        // (`ENOTDIR`, review finding 3 on PR #467, its own distinct
        // `NotADirectoryError` test elsewhere in this module).
        let token = CancellationToken::new();
        let missing_dir = std::env::temp_dir().join(format!(
            "electricity-tools-missing-cwd-test-{}",
            std::process::id()
        ));
        let cwd = missing_dir.to_str().unwrap().to_string();
        let err = run_binary(
            "echo",
            &[],
            Some(&cwd),
            None,
            None,
            30,
            false,
            "set runtime.plugins.awk.binary",
            false,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.message,
            "binary not found: 'echo' (install it or set runtime.plugins.awk.binary)."
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn run_binary_with_allow_nonzero_reports_a_signal_killed_exit_code_as_negative() {
        // `Completed::exit_code` is `-signum` for a signal-killed child
        // (issue #459's own "Exit status" bullet) -- `allow_nonzero`
        // lets this reach the caller instead of raising, the only way
        // to observe it directly from `run_binary` itself.
        let token = CancellationToken::new();
        let result = run_binary(
            "sh",
            &[Value::from("-c"), Value::from("kill -TERM $$; sleep 5")],
            None,
            None,
            None,
            30,
            true,
            "set runtime.plugins.awk.binary",
            false,
            &token,
        )
        .await
        .unwrap();
        assert_eq!(result.exit_code, Some(-libc::SIGTERM));
    }
}
