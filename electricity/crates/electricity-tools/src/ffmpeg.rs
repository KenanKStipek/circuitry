//! `plugins/ffmpeg.py`, ported. Unlike `awk`/`imagemagick`
//! ([`crate::subprocess_tool::GenericSubprocessTool`]), `ffmpeg` is its
//! own plugin type: Python's own version rejects shell metacharacters
//! in several params fields before dispatch (`_check_safe`/
//! `_check_filter_safe`) and escapes `drawtext` text, neither of which
//! a plain pass-through wrapper can express.

use crate::process::{self, ProcError, RunOpts};
use crate::subprocess_tool::{merged_env, resolve_plugin_binary};
use crate::{CheckResult, ErrorCause, PyExc, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::pycompat::{py_strip, shlex_quote, shlex_split};
use electricity_value::{Dict, Value};
use std::time::Duration;

const SHELL_METACHARACTERS: [&str; 9] = ["&&", "||", "|", ";", ">", "<", "`", "$(", "\n"];
/// `filter_complex` allows `|` and `;` (ffmpeg filter graph separators)
/// but still blocks shell injection.
const FILTER_METACHARACTERS: [&str; 6] = ["&&", ">", "<", "`", "$(", "\n"];

const DRAWTEXT_OPTION_KEYS: [&str; 13] = [
    "x",
    "y",
    "fontsize",
    "fontfile",
    "fontcolor",
    "box",
    "boxcolor",
    "boxborderw",
    "shadowx",
    "shadowy",
    "shadowcolor",
    "borderw",
    "bordercolor",
];

fn check_safe(value: &str, field: &str) -> Result<(), ToolError> {
    for meta in SHELL_METACHARACTERS {
        if value.contains(meta) {
            return Err(value_error(format!(
                "ffmpeg params['{field}'] contains unsafe shell characters: {}. Only plain \
                 ffmpeg arguments are allowed.",
                Value::Str(meta.to_string()).py_repr()
            )));
        }
    }
    Ok(())
}

fn check_filter_safe(value: &str, field: &str) -> Result<(), ToolError> {
    for meta in FILTER_METACHARACTERS {
        if value.contains(meta) {
            return Err(value_error(format!(
                "ffmpeg params['{field}'] contains unsafe characters: {}. Only ffmpeg filter \
                 graph expressions are allowed.",
                Value::Str(meta.to_string()).py_repr()
            )));
        }
    }
    Ok(())
}

/// `plugins/ffmpeg.py::_escape_drawtext_text`.
fn escape_drawtext_text(text: &str) -> String {
    let collapsed = collapse_runs(text, |c| {
        matches!(
            c,
            '\r' | '\n' | '\x0b' | '\x0c' | '\u{85}' | '\u{2028}' | '\u{2029}'
        )
    });
    let escaped = collapsed
        .replace('\\', "\\\\")
        .replace('%', "%%")
        .replace('\'', "\u{2019}")
        .replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Collapses every run of one-or-more characters matching *is_match*
/// into a single space -- `re.sub(pattern, " ", text)`'s own `+`
/// quantifier.
fn collapse_runs(text: &str, is_match: impl Fn(char) -> bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_run = false;
    for c in text.chars() {
        if is_match(c) {
            if !in_run {
                out.push(' ');
                in_run = true;
            }
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

/// `plugins/ffmpeg.py::_sanitize_drawtext_value`.
fn sanitize_drawtext_value(value: &Value) -> String {
    let stripped = py_strip(&value.py_str()).to_string();
    collapse_runs(&stripped, |c| matches!(c, '\r' | '\n'))
}

/// `plugins/ffmpeg.py::_build_drawtext_filter`.
fn build_drawtext_filter(cfg: &Dict) -> String {
    let mut raw_text = py_strip(
        &cfg.get(&Value::Str("text".to_string()))
            .map(Value::py_str)
            .unwrap_or_default(),
    )
    .to_string();
    if let Some(first) = raw_text.chars().next() {
        if first == '"' || first == '\'' {
            raw_text = raw_text[first.len_utf8()..].to_string();
        }
    }
    if let Some(last) = raw_text.chars().last() {
        if last == '"' || last == '\'' {
            let cut = raw_text.len() - last.len_utf8();
            raw_text.truncate(cut);
        }
    }
    let mut parts = vec![format!("text={}", escape_drawtext_text(&raw_text))];
    for key in DRAWTEXT_OPTION_KEYS {
        if let Some(value) = cfg.get(&Value::Str(key.to_string())) {
            parts.push(format!("{key}={}", sanitize_drawtext_value(value)));
        }
    }
    format!("drawtext={}", parts.join(":"))
}

/// `plugins/ffmpeg.py::FfmpegPlugin` -- the shell-metacharacter
/// rejection, `drawtext` escaping, and the `run_tracked` dispatch.
pub struct FfmpegPlugin {
    pub binary: Option<String>,
    pub env: Option<std::collections::HashMap<String, String>>,
}

#[async_trait(?Send)]
impl ToolPlugin for FfmpegPlugin {
    fn name(&self) -> &str {
        "ffmpeg"
    }

    async fn execute(&self, params: Value, call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        let dict = params.as_dict();
        let get = |key: &str| dict.and_then(|d| d.get(&Value::Str(key.to_string())));

        let input_path = match get("input") {
            Some(Value::Str(s)) if !s.is_empty() => s.clone(),
            _ => {
                return Err(value_error(
                    "FfmpegPlugin requires params['input'] as a non-empty string.",
                ));
            }
        };
        let output_path = match get("output") {
            Some(Value::Str(s)) if !s.is_empty() => s.clone(),
            _ => {
                return Err(value_error(
                    "FfmpegPlugin requires params['output'] as a non-empty string.",
                ));
            }
        };

        let flags = py_strip(&get("flags").map(Value::py_str).unwrap_or_default()).to_string();

        let extra_inputs: Vec<String> = match get("extra_inputs") {
            Some(value) if crate::subprocess_tool::is_truthy(value) => match value {
                Value::List(items) => items.iter().map(Value::py_str).collect(),
                Value::Str(s) => s.chars().map(|c| c.to_string()).collect(),
                Value::Dict(d) => d.keys().map(Value::py_str).collect(),
                other => vec![other.py_str()],
            },
            _ => Vec::new(),
        };

        let vf_drawtext_dict: Option<&Dict> = match get("vf_drawtext") {
            Some(Value::Dict(d)) if !d.is_empty() => Some(d),
            _ => None,
        };

        check_safe(&input_path, "input")?;
        check_safe(&output_path, "output")?;
        if !flags.is_empty() {
            check_safe(&flags, "flags")?;
        }
        for (i, extra) in extra_inputs.iter().enumerate() {
            check_safe(extra, &format!("extra_inputs[{i}]"))?;
        }
        let filter_complex: Option<String> = match get("filter_complex") {
            None | Some(Value::None) => None,
            Some(Value::Str(s)) => {
                check_filter_safe(s, "filter_complex")?;
                Some(s.clone())
            }
            Some(_) => {
                return Err(value_error(
                    "FfmpegPlugin: params['filter_complex'] must be a string.",
                ));
            }
        };

        let mut cmd: Vec<String> = vec![
            "ffmpeg".to_string(),
            "-y".to_string(),
            "-i".to_string(),
            input_path.clone(),
        ];
        for extra in &extra_inputs {
            cmd.push("-i".to_string());
            cmd.push(extra.clone());
        }

        if let Some(fc) = &filter_complex {
            cmd.push("-filter_complex".to_string());
            cmd.push(fc.clone());
        } else if let Some(vfd) = vf_drawtext_dict {
            cmd.push("-vf".to_string());
            cmd.push(build_drawtext_filter(vfd));
        } else if !flags.is_empty() {
            let parts = shlex_split(&flags).map_err(value_error)?;
            cmd.extend(parts);
        }

        if let Some(map_val) = get("map") {
            if crate::subprocess_tool::is_truthy(map_val) {
                cmd.push("-map".to_string());
                cmd.push(map_val.py_str());
            }
        }
        cmd.push(output_path.clone());

        let binary = resolve_plugin_binary("ffmpeg", &["ffmpeg"], self.binary.as_deref())?;
        cmd[0] = binary.clone();

        let opts = RunOpts {
            timeout: Some(Duration::from_secs(u64::from(call.timeout_seconds))),
            input: None,
            cwd: None,
            env: merged_env(self.env.as_ref()),
            new_session: call.armed,
            env_remove: Vec::new(),
        };

        let completed = match process::run_tracked(&cmd, &opts, call.token).await {
            Ok(completed) => completed,
            Err(ProcError::NotFound(_)) => {
                return Err(ToolError {
                    py_class: PyExc::RuntimeError,
                    message: "ffmpeg is not installed or not on PATH. Install ffmpeg, or set \
                              runtime.plugins.ffmpeg.binary."
                        .to_string(),
                    cause: ErrorCause::None,
                });
            }
            Err(ProcError::Timeout { .. }) => {
                return Err(ToolError {
                    py_class: PyExc::RuntimeError,
                    message: format!("ffmpeg timed out after {}s", call.timeout_seconds),
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

        if completed.exit_code != 0 {
            let cmd_str = cmd
                .iter()
                .map(|a| shlex_quote(a).expect("shlex_quote never fails"))
                .collect::<Vec<_>>()
                .join(" ");
            return Err(ToolError {
                py_class: PyExc::RuntimeError,
                message: format!(
                    "ffmpeg failed (exit {})\ncmd: {cmd_str}\nstderr: {}",
                    completed.exit_code,
                    py_strip(&completed.stderr)
                ),
                cause: ErrorCause::None,
            });
        }

        let mut raw = Dict::new();
        raw.insert(Value::from("binary"), Value::from(binary.as_str()));

        Ok(ToolResult {
            value: Value::from(output_path),
            raw: Value::Dict(raw),
            stdout: Some(completed.stdout),
            stderr: Some(completed.stderr),
            exit_code: Some(completed.exit_code),
            ok: true,
        })
    }

    fn check(&self) -> CheckResult {
        crate::subprocess_tool::check_binary(
            &["ffmpeg"],
            Some("ffmpeg"),
            self.binary.as_deref(),
            Some("ffmpeg"),
        )
    }
}

fn value_error(message: impl Into<String>) -> ToolError {
    ToolError {
        py_class: PyExc::ValueError,
        message: message.into(),
        cause: ErrorCause::None,
    }
}

/// `plugins/factory.py::_build_ffmpeg`.
pub fn make_plugin(cfg: &Value) -> Result<Box<dyn ToolPlugin>, ToolError> {
    Ok(Box::new(FfmpegPlugin {
        binary: crate::subprocess_tool::plugin_binary_override(cfg),
        env: crate::subprocess_tool::plugin_env_override(cfg, "ffmpeg")?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::CancellationToken;

    static NONE: Value = Value::None;

    fn call(token: &CancellationToken) -> ToolCall<'_> {
        ToolCall {
            timeout_seconds: 30,
            config: &NONE,
            token,
            armed: false,
        }
    }

    fn params(entries: Vec<(&str, Value)>) -> Value {
        let mut dict = Dict::new();
        for (k, v) in entries {
            dict.insert(Value::from(k), v);
        }
        Value::Dict(dict)
    }

    #[test]
    fn escape_drawtext_text_quotes_and_escapes() {
        assert_eq!(
            escape_drawtext_text("say \"hi\" it's\nme"),
            "\"say \\\"hi\\\" it\u{2019}s me\""
        );
    }

    #[test]
    fn build_drawtext_filter_strips_one_leading_and_trailing_quote() {
        let mut cfg = Dict::new();
        cfg.insert(Value::from("text"), Value::from("\"hi.\""));
        cfg.insert(Value::from("x"), Value::from("10"));
        assert_eq!(build_drawtext_filter(&cfg), "drawtext=text=\"hi.\":x=10");
    }

    #[tokio::test]
    async fn missing_input_is_rejected_before_any_dispatch() {
        let token = CancellationToken::new();
        let plugin = FfmpegPlugin {
            binary: None,
            env: None,
        };
        let err = plugin
            .execute(
                params(vec![("output", Value::from("out.mp4"))]),
                &call(&token),
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "FfmpegPlugin requires params['input'] as a non-empty string."
        );
    }

    #[tokio::test]
    async fn an_unsafe_shell_character_in_flags_is_rejected() {
        let token = CancellationToken::new();
        let plugin = FfmpegPlugin {
            binary: None,
            env: None,
        };
        let err = plugin
            .execute(
                params(vec![
                    ("input", Value::from("in.mp4")),
                    ("output", Value::from("out.mp4")),
                    ("flags", Value::from("-vf scale=100;rm -rf /")),
                ]),
                &call(&token),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("contains unsafe shell characters"));
    }

    #[derive(serde::Deserialize)]
    struct DrawtextEscapeCase {
        text: String,
        expected: String,
    }

    #[derive(serde::Deserialize)]
    struct DrawtextFilterCase {
        cfg: std::collections::BTreeMap<String, serde_json::Value>,
        expected: String,
    }

    #[derive(serde::Deserialize)]
    struct CheckSafeCase {
        value: String,
        field: String,
        expected_error: Option<String>,
    }

    #[derive(serde::Deserialize)]
    struct CheckFilterSafeCase {
        value: String,
        expected_error: Option<String>,
    }

    #[derive(serde::Deserialize)]
    struct ProcessToolsCorpus {
        drawtext_escape_cases: Vec<DrawtextEscapeCase>,
        drawtext_filter_cases: Vec<DrawtextFilterCase>,
        check_safe_cases: Vec<CheckSafeCase>,
        check_filter_safe_cases: Vec<CheckFilterSafeCase>,
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
            _ => panic!("unsupported JSON shape in a drawtext cfg corpus case"),
        }
    }

    #[test]
    fn drawtext_escape_matches_circuitrys_own_function() {
        for case in corpus().drawtext_escape_cases {
            assert_eq!(
                escape_drawtext_text(&case.text),
                case.expected,
                "text={:?}",
                case.text
            );
        }
    }

    #[test]
    fn drawtext_filter_matches_circuitrys_own_function() {
        for case in corpus().drawtext_filter_cases {
            let mut cfg = Dict::new();
            for (k, v) in &case.cfg {
                cfg.insert(Value::from(k.as_str()), json_to_value(v));
            }
            assert_eq!(build_drawtext_filter(&cfg), case.expected);
        }
    }

    #[test]
    fn check_safe_matches_circuitrys_own_function() {
        for case in corpus().check_safe_cases {
            let result = check_safe(&case.value, &case.field);
            match case.expected_error {
                None => assert!(result.is_ok(), "value={:?}", case.value),
                Some(expected) => {
                    assert_eq!(
                        result.unwrap_err().message,
                        expected,
                        "value={:?}",
                        case.value
                    )
                }
            }
        }
    }

    #[test]
    fn check_filter_safe_matches_circuitrys_own_function() {
        for case in corpus().check_filter_safe_cases {
            let result = check_filter_safe(&case.value, "filter_complex");
            match case.expected_error {
                None => assert!(result.is_ok(), "value={:?}", case.value),
                Some(expected) => {
                    assert_eq!(
                        result.unwrap_err().message,
                        expected,
                        "value={:?}",
                        case.value
                    )
                }
            }
        }
    }

    #[tokio::test]
    async fn a_non_string_filter_complex_is_rejected() {
        let token = CancellationToken::new();
        let plugin = FfmpegPlugin {
            binary: None,
            env: None,
        };
        let err = plugin
            .execute(
                params(vec![
                    ("input", Value::from("in.mp4")),
                    ("output", Value::from("out.mp4")),
                    ("filter_complex", Value::from(1i64)),
                ]),
                &call(&token),
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "FfmpegPlugin: params['filter_complex'] must be a string."
        );
    }
}
