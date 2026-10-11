//! `plugins/shell.py::ShellPlugin`, ported. Not a shell interpreter:
//! runs a single allowlisted binary, no shell metacharacters, no
//! pipes/redirection/substitution/globbing -- `plugins/shell.py`'s own
//! module doc comment, the full allowlist/host-pin-intersection rule
//! included.

use crate::subprocess_tool::{as_bool_param, run_binary};
use crate::{CheckResult, ErrorCause, PyExc, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::Value;
use electricity_value::pycompat::py_strip;
use std::collections::BTreeSet;

// `#[path]`, not a `mod generated;` line in `lib.rs` (review finding 9
// on PR #467): this is the only module in the crate that needs
// `is_py_alnum`, so the declaration lives here instead of adding a
// line to a file `vm-lanes.md` calls final and complete for every
// lane but the one that owns it.
#[path = "generated/mod.rs"]
mod generated;

const DEFAULT_ALLOWED: [&str; 8] = ["ls", "cat", "head", "tail", "wc", "echo", "pwd", "date"];

/// `plugins/shell.py::ShellPlugin` -- the default/pinned allowlist
/// intersection, the per-arg shell-metacharacter/null-byte/newline
/// rejection, and the `run_tracked` dispatch.
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

    async fn execute(&self, params: Value, call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        let dict = params.as_dict();
        let get = |key: &str| dict.and_then(|d| d.get(&Value::Str(key.to_string())));

        let command = match get("command") {
            Some(Value::Str(s)) if !py_strip(s).is_empty() => py_strip(s).to_string(),
            _ => return Err(value_error("shell requires params['command'] as a string.")),
        };

        if !is_bare_command(&command) {
            return Err(value_error(format!(
                "shell: command {} must be alphanumeric (with optional -_); use args for \
                 everything else.",
                Value::Str(command.clone()).py_repr()
            )));
        }

        let allowed_set: BTreeSet<String> = match get("allowed_commands") {
            None | Some(Value::None) => DEFAULT_ALLOWED.iter().map(|s| s.to_string()).collect(),
            Some(Value::List(items)) => {
                let mut set = BTreeSet::new();
                for item in items {
                    match item {
                        Value::Str(s) => {
                            set.insert(s.clone());
                        }
                        _ => {
                            return Err(value_error(
                                "shell: params['allowed_commands'] must be a list of strings.",
                            ));
                        }
                    }
                }
                set
            }
            _ => {
                return Err(value_error(
                    "shell: params['allowed_commands'] must be a list of strings.",
                ));
            }
        };

        let effect_allowed_set = allowed_set.clone();
        let effective: BTreeSet<String> = match &self.pinned_allowed_commands {
            Some(pin) => {
                let pin_set: BTreeSet<String> = pin.iter().cloned().collect();
                allowed_set.intersection(&pin_set).cloned().collect()
            }
            None => allowed_set,
        };

        if !effective.contains(&command) {
            if let Some(pin) = &self.pinned_allowed_commands {
                if effect_allowed_set.contains(&command) {
                    // `sorted(pin)` -- a Python *list* sort, which keeps
                    // duplicates (review finding 7 on PR #467); the
                    // pin itself is a list, never deduplicated into a
                    // set the way the effective allowlist below is.
                    let mut pin_sorted = pin.clone();
                    pin_sorted.sort();
                    return Err(ToolError {
                        py_class: PyExc::PermissionError,
                        message: format!(
                            "shell: command {} is blocked by the host's \
                             runtime.plugins.shell.allowed_commands pin {}; adding it to \
                             params['allowed_commands'] cannot widen past the pin.",
                            Value::Str(command.clone()).py_repr(),
                            list_repr(&pin_sorted),
                        ),
                        cause: ErrorCause::None,
                    });
                }
            }
            return Err(ToolError {
                py_class: PyExc::PermissionError,
                message: format!(
                    "shell: command {} not in allowlist {}. Add it via \
                     params['allowed_commands'] or pick a different command.",
                    Value::Str(command.clone()).py_repr(),
                    set_repr(&effective),
                ),
                cause: ErrorCause::None,
            });
        }

        let args: &[Value] = match get("args") {
            Some(Value::List(items)) => items,
            // `params.get("args") or []` -- any *falsy* value (`None`,
            // `""`, `false`, `0`, `{}`...), not just a missing/`None`
            // key, runs with no arguments (review finding 5 on PR #467);
            // only a *truthy* non-list value is an error.
            None => &[],
            Some(v) if !crate::subprocess_tool::is_truthy(v) => &[],
            _ => return Err(value_error("shell: params['args'] must be a list.")),
        };
        for (i, a) in args.iter().enumerate() {
            let s = a.py_str();
            for bad in ['\0', '\n'] {
                if s.contains(bad) {
                    return Err(value_error(format!(
                        "shell: args[{i}] contains forbidden char {}.",
                        Value::Str(bad.to_string()).py_repr()
                    )));
                }
            }
        }

        let binary = match crate::process::resolve_binary(&[&command]) {
            Some(binary) => binary,
            None => {
                return Err(ToolError {
                    py_class: PyExc::RuntimeError,
                    message: format!(
                        "shell: command {} not found on PATH.",
                        Value::Str(command.clone()).py_repr()
                    ),
                    cause: ErrorCause::None,
                });
            }
        };

        let cwd = get("cwd").and_then(Value::as_str);
        let stdin = get("stdin").and_then(Value::as_str);
        let allow_nonzero = as_bool_param(get("allow_nonzero"));

        run_binary(
            &binary,
            args,
            cwd,
            stdin,
            // `shell` passes `env=None`, i.e. the full inherited
            // environment -- there is no `runtime.plugins.shell.env`.
            None,
            call.timeout_seconds,
            allow_nonzero,
            "override the plugin's binary path",
            call.armed,
            call.token,
        )
        .await
    }

    fn check(&self) -> CheckResult {
        // The shell plugin itself has no binary -- its allowlist is
        // checked at execute time. Report ok; preflight surfaces nothing.
        CheckResult {
            ok: true,
            missing: Vec::new(),
            message: Some(
                "shell allowlist defaults to read-only commands; pass \
                 params['allowed_commands'] to broaden per-effect."
                    .to_string(),
            ),
        }
    }
}

/// `plugins/factory.py::_build_shell` -- the host-pin's own build-time
/// validation, raised per attempt (from the builder, inside the retry
/// loop) rather than once up front.
pub fn make_plugin(cfg: &Value) -> Result<Box<dyn ToolPlugin>, ToolError> {
    let pinned = match cfg
        .as_dict()
        .and_then(|d| d.get(&Value::Str("allowed_commands".to_string())))
    {
        None | Some(Value::None) => None,
        Some(Value::List(items)) => {
            let mut commands = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::Str(s) => commands.push(s.clone()),
                    _ => {
                        return Err(value_error(
                            "runtime.plugins.shell.allowed_commands must be a list of strings.",
                        ));
                    }
                }
            }
            Some(commands)
        }
        Some(_) => {
            return Err(value_error(
                "runtime.plugins.shell.allowed_commands must be a list of strings.",
            ));
        }
    };
    Ok(Box::new(ShellPlugin {
        pinned_allowed_commands: pinned,
    }))
}

/// `command.replace("-", "").replace("_", "").isalnum()` -- CPython's
/// own Unicode `str.isalnum()` (true iff every character is alphabetic,
/// decimal, digit, or numeric, and there's at least one -- `"".isalnum()`
/// is `False`). [`generated::is_py_alnum`], not Rust's own,
/// broader `char::is_alphanumeric` -- see that function's own doc
/// comment for the confirmed gap between the two.
fn is_bare_command(command: &str) -> bool {
    let filtered: String = command.chars().filter(|&c| c != '-' && c != '_').collect();
    !filtered.is_empty() && filtered.chars().all(generated::is_py_alnum)
}

fn set_repr(items: &BTreeSet<String>) -> String {
    Value::List(items.iter().map(|s| Value::from(s.as_str())).collect()).py_repr()
}

fn list_repr(items: &[String]) -> String {
    Value::List(items.iter().map(|s| Value::from(s.as_str())).collect()).py_repr()
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
    use electricity_value::{CancellationToken, Dict};

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

    #[tokio::test]
    async fn a_default_allowlist_command_runs() {
        let token = CancellationToken::new();
        let plugin = ShellPlugin {
            pinned_allowed_commands: None,
        };
        let result = plugin
            .execute(
                params(vec![
                    ("command", Value::from("echo")),
                    ("args", Value::List(vec![Value::from("hi")])),
                ]),
                &call(&token),
            )
            .await
            .unwrap();
        assert_eq!(result.value, Value::from("hi\n"));
    }

    #[tokio::test]
    async fn a_command_outside_the_default_allowlist_is_denied() {
        let token = CancellationToken::new();
        let plugin = ShellPlugin {
            pinned_allowed_commands: None,
        };
        let err = plugin
            .execute(params(vec![("command", Value::from("rm"))]), &call(&token))
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "shell: command 'rm' not in allowlist ['cat', 'date', 'echo', 'head', 'ls', \
             'pwd', 'tail', 'wc']. Add it via params['allowed_commands'] or pick a \
             different command."
        );
        assert_eq!(err.py_class, PyExc::PermissionError);
    }

    #[tokio::test]
    async fn the_host_pin_narrows_past_the_effects_own_allowlist() {
        let token = CancellationToken::new();
        let plugin = ShellPlugin {
            pinned_allowed_commands: Some(vec!["ls".to_string()]),
        };
        let err = plugin
            .execute(
                params(vec![
                    ("command", Value::from("echo")),
                    ("allowed_commands", Value::List(vec![Value::from("echo")])),
                ]),
                &call(&token),
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "shell: command 'echo' is blocked by the host's \
             runtime.plugins.shell.allowed_commands pin ['ls']; adding it to \
             params['allowed_commands'] cannot widen past the pin."
        );
        assert_eq!(err.py_class, PyExc::PermissionError);
    }

    #[tokio::test]
    async fn a_non_alphanumeric_command_is_rejected_before_any_lookup() {
        let token = CancellationToken::new();
        let plugin = ShellPlugin {
            pinned_allowed_commands: None,
        };
        let err = plugin
            .execute(
                params(vec![("command", Value::from("ls; rm -rf /"))]),
                &call(&token),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("must be alphanumeric"));
        assert_eq!(err.py_class, PyExc::ValueError);
    }

    #[tokio::test]
    async fn a_forbidden_char_in_args_is_rejected() {
        let token = CancellationToken::new();
        let plugin = ShellPlugin {
            pinned_allowed_commands: None,
        };
        let err = plugin
            .execute(
                params(vec![
                    ("command", Value::from("echo")),
                    ("args", Value::List(vec![Value::from("a\nb")])),
                ]),
                &call(&token),
            )
            .await
            .unwrap_err();
        assert_eq!(err.message, "shell: args[0] contains forbidden char '\\n'.");
    }

    #[tokio::test]
    async fn a_falsy_non_list_args_value_runs_with_no_arguments() {
        // `params.get("args") or []` (review finding 5 on PR #467): an
        // empty string, `false`, `0` and `{}` are all falsy, so each
        // runs with no arguments, same as an absent/`None` `args`.
        let token = CancellationToken::new();
        for falsy in [
            Value::from(""),
            Value::Bool(false),
            Value::from(0i64),
            Value::Dict(Dict::new()),
        ] {
            let plugin = ShellPlugin {
                pinned_allowed_commands: None,
            };
            let result = plugin
                .execute(
                    params(vec![("command", Value::from("echo")), ("args", falsy.clone())]),
                    &call(&token),
                )
                .await
                .unwrap_or_else(|err| panic!("falsy args={falsy:?} must not error: {err}"));
            assert_eq!(result.value, Value::from("\n"));
        }
    }

    #[tokio::test]
    async fn a_truthy_non_list_args_value_is_still_rejected() {
        let token = CancellationToken::new();
        let plugin = ShellPlugin {
            pinned_allowed_commands: None,
        };
        let err = plugin
            .execute(
                params(vec![
                    ("command", Value::from("echo")),
                    ("args", Value::from("nonempty")),
                ]),
                &call(&token),
            )
            .await
            .unwrap_err();
        assert_eq!(err.message, "shell: params['args'] must be a list.");
    }

    #[tokio::test]
    async fn the_pin_in_the_denial_message_keeps_duplicates_like_pythons_own_sorted_list() {
        let token = CancellationToken::new();
        let plugin = ShellPlugin {
            pinned_allowed_commands: Some(vec!["ls".to_string(), "ls".to_string()]),
        };
        let err = plugin
            .execute(
                params(vec![
                    ("command", Value::from("echo")),
                    ("allowed_commands", Value::List(vec![Value::from("echo")])),
                ]),
                &call(&token),
            )
            .await
            .unwrap_err();
        assert!(
            err.message.contains("pin ['ls', 'ls']"),
            "message={}",
            err.message
        );
    }

    #[derive(serde::Deserialize)]
    struct ShellValidationCase {
        params: std::collections::BTreeMap<String, serde_json::Value>,
        pin: Option<Vec<String>>,
        expected_error_class: Option<String>,
        expected_error_message: Option<String>,
    }

    #[derive(serde::Deserialize)]
    struct ProcessToolsCorpus {
        shell_validation_cases: Vec<ShellValidationCase>,
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

    #[tokio::test]
    async fn shell_pre_spawn_validation_matches_circuitrys_own_messages() {
        let corpus: ProcessToolsCorpus =
            serde_json::from_str(include_str!("../tests/golden/process_tools_corpus.json"))
                .expect("process_tools_corpus.json must parse");
        let token = CancellationToken::new();
        for case in corpus.shell_validation_cases {
            let mut dict = Dict::new();
            for (k, v) in &case.params {
                dict.insert(Value::from(k.as_str()), json_to_value(v));
            }
            let plugin = ShellPlugin {
                pinned_allowed_commands: case.pin.clone(),
            };
            let result = plugin.execute(Value::Dict(dict), &call(&token)).await;
            match (case.expected_error_class, case.expected_error_message) {
                (Some(class), Some(message)) => {
                    let err = result.unwrap_err();
                    assert_eq!(err.message, message, "params={:?}", case.params);
                    let got_class = match err.py_class {
                        PyExc::ValueError => "ValueError",
                        PyExc::PermissionError => "PermissionError",
                        PyExc::RuntimeError => "RuntimeError",
                        other => {
                            panic!("unexpected py_class {other:?} for params={:?}", case.params)
                        }
                    };
                    assert_eq!(got_class, class, "params={:?}", case.params);
                }
                (None, None) => assert!(result.is_ok(), "params={:?}", case.params),
                _ => panic!("inconsistent corpus case for params={:?}", case.params),
            }
        }
    }

    #[test]
    fn check_is_always_ok() {
        let plugin = ShellPlugin {
            pinned_allowed_commands: None,
        };
        let result = plugin.check();
        assert!(result.ok);
        assert!(result.missing.is_empty());
        // Review finding 8 on PR #467: the acceptance criterion is
        // `check()` matching `ShellPlugin.check` field for field, not
        // just `ok`/`missing` -- the exact message text too.
        assert_eq!(
            result.message,
            Some(
                "shell allowlist defaults to read-only commands; pass \
                 params['allowed_commands'] to broaden per-effect."
                    .to_string()
            )
        );
    }
}
