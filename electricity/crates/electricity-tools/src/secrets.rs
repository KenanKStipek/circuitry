//! `secrets.py`'s own resolution half, ported: `runtime.secrets.<name>`
//! from the Keychain, a command or an environment variable (issue
//! #598/#725), resolved at dispatch, and [`SecretValues`], which
//! replaces a dispatch's own resolved values wherever a result or an
//! error would otherwise echo one. The data-shape half (whether a
//! `Value` reads as a `{secret: ...}` reference at all) is
//! `electricity_bytecode::secrets`, shared with the compiler; this
//! crate only resolves an already-recognized, well-formed one.

use crate::process::{self, ProcError, RunOpts};
use crate::{ErrorCause, PyExc, ToolError};
use electricity_value::{CancellationToken, Value};
use std::time::Duration;

/// `secrets.py::SECRET_COMMAND_TIMEOUT_SECONDS`.
pub const SECRET_COMMAND_TIMEOUT_SECONDS: u64 = 60;

/// `secrets.py::_FORBIDDEN_CHARS`.
fn has_forbidden_char(text: &str) -> bool {
    text.contains('\r') || text.contains('\n') || text.contains('\0')
}

fn runtime_error(message: impl Into<String>) -> ToolError {
    ToolError {
        py_class: PyExc::RuntimeError,
        message: message.into(),
        cause: ErrorCause::None,
    }
}

fn repr(value: &Value) -> String {
    value.py_repr()
}

/// `secrets.py::resolve_secret` -- the value of `runtime.secrets.<name>`.
/// `Err` names the secret, never its value or anything its command
/// printed, exactly as [`secrets.py::SecretError`]'s own messages do.
pub async fn resolve_secret(
    name: &str,
    runtime_config: &Value,
    token: &CancellationToken,
) -> Result<String, ToolError> {
    let secrets_cfg = runtime_config
        .as_dict()
        .and_then(|d| d.get(&Value::Str("secrets".to_string())))
        .and_then(Value::as_dict);
    let entry = secrets_cfg.and_then(|d| d.get(&Value::Str(name.to_string())));
    let Some(entry) = entry else {
        return Err(runtime_error(format!(
            "secret {} is not defined in runtime.secrets.",
            repr(&Value::Str(name.to_string()))
        )));
    };
    let label = format!("runtime.secrets.{name}");
    let Value::Dict(dict) = entry else {
        return Err(wrong_entry_shape(&label));
    };
    if dict.len() != 1 {
        return Err(wrong_entry_shape(&label));
    }
    let (source_key, spec) = dict.iter().next().expect("checked len() == 1 above");
    let Value::Str(source) = source_key else {
        return Err(wrong_entry_shape(&label));
    };
    match source.as_str() {
        "env" => {
            let Value::Str(var) = spec else {
                return Err(runtime_error(format!(
                    "{label}.env must name an environment variable."
                )));
            };
            if var.is_empty() {
                return Err(runtime_error(format!(
                    "{label}.env must name an environment variable."
                )));
            }
            match std::env::var(var) {
                Ok(value) => checked(name, value),
                Err(_) => Err(runtime_error(format!(
                    "secret {}: environment variable {} is not set.",
                    repr(&Value::Str(name.to_string())),
                    repr(&Value::Str(var.clone()))
                ))),
            }
        }
        "keychain" => {
            let Value::Dict(spec_dict) = spec else {
                return Err(runtime_error(format!(
                    "{label}.keychain must be {{service, account?}} with a string service."
                )));
            };
            let service = match spec_dict.get(&Value::Str("service".to_string())) {
                Some(Value::Str(s)) => s.clone(),
                _ => {
                    return Err(runtime_error(format!(
                        "{label}.keychain must be {{service, account?}} with a string service."
                    )));
                }
            };
            let account = match spec_dict.get(&Value::Str("account".to_string())) {
                None | Some(Value::None) => None,
                Some(Value::Str(s)) => Some(s.clone()),
                Some(_) => {
                    return Err(runtime_error(format!(
                        "{label}.keychain.account must be a string."
                    )));
                }
            };
            let mut unknown: Vec<String> = spec_dict
                .keys()
                .filter_map(|key| match key {
                    Value::Str(s) if s != "service" && s != "account" => Some(s.clone()),
                    Value::Str(_) => None,
                    other => Some(other.py_str()),
                })
                .collect();
            unknown.sort();
            if !unknown.is_empty() {
                let quoted = unknown
                    .iter()
                    .map(|k| repr(&Value::Str(k.clone())))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(runtime_error(format!(
                    "{label}.keychain has unknown key(s) {quoted}."
                )));
            }
            let mut argv = vec![
                "security".to_string(),
                "find-generic-password".to_string(),
                "-w".to_string(),
                "-s".to_string(),
                service,
            ];
            if let Some(account) = account {
                argv.push("-a".to_string());
                argv.push(account);
            }
            let output = command_output(name, &argv, "the Keychain lookup", token).await?;
            checked(name, output)
        }
        "command" => {
            let Value::List(items) = spec else {
                return Err(runtime_error(format!(
                    "{label}.command must be a non-empty list of strings."
                )));
            };
            if items.is_empty() {
                return Err(runtime_error(format!(
                    "{label}.command must be a non-empty list of strings."
                )));
            }
            let mut argv = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::Str(s) => argv.push(s.clone()),
                    _ => {
                        return Err(runtime_error(format!(
                            "{label}.command must be a non-empty list of strings."
                        )));
                    }
                }
            }
            let output = command_output(name, &argv, "its command", token).await?;
            checked(name, output)
        }
        _ => Err(wrong_entry_shape(&label)),
    }
}

fn wrong_entry_shape(label: &str) -> ToolError {
    runtime_error(format!(
        "{label} must be exactly one of {{keychain: {{service, account?}}}}, \
         {{command: [argv...]}} or {{env: NAME}}."
    ))
}

async fn command_output(
    name: &str,
    argv: &[String],
    what: &str,
    token: &CancellationToken,
) -> Result<String, ToolError> {
    let opts = RunOpts {
        timeout: Some(Duration::from_secs(SECRET_COMMAND_TIMEOUT_SECONDS)),
        ..RunOpts::default()
    };
    match process::run_tracked(argv, &opts, token).await {
        Ok(completed) => {
            if completed.exit_code != 0 {
                return Err(runtime_error(format!(
                    "secret {}: {what} exited with status {}.",
                    repr(&Value::Str(name.to_string())),
                    completed.exit_code
                )));
            }
            Ok(completed.stdout.trim().to_string())
        }
        Err(ProcError::NotFound(_)) => Err(runtime_error(format!(
            "secret {}: {what} could not start ({} was not found).",
            repr(&Value::Str(name.to_string())),
            repr(&Value::Str(argv[0].clone()))
        ))),
        Err(ProcError::Timeout {
            timeout_seconds, ..
        }) => Err(runtime_error(format!(
            "secret {}: {what} took longer than {timeout_seconds}s.",
            repr(&Value::Str(name.to_string()))
        ))),
        Err(ProcError::Cancelled) => Err(ToolError::message("cancelled")),
        Err(ProcError::Decode(_)) => Err(runtime_error(format!(
            "secret {}: {what} could not start ({}).",
            repr(&Value::Str(name.to_string())),
            PyExc::UnicodeDecodeError.name()
        ))),
        Err(ProcError::Other { py_class, .. }) => Err(runtime_error(format!(
            "secret {}: {what} could not start ({}).",
            repr(&Value::Str(name.to_string())),
            py_class.name()
        ))),
    }
}

fn checked(name: &str, value: String) -> Result<String, ToolError> {
    if value.is_empty() {
        return Err(runtime_error(format!(
            "secret {} resolved to an empty value.",
            repr(&Value::Str(name.to_string()))
        )));
    }
    if has_forbidden_char(&value) {
        return Err(runtime_error(format!(
            "secret {} holds a line break or a NUL, which it cannot carry.",
            repr(&Value::Str(name.to_string()))
        )));
    }
    Ok(value)
}

/// `secrets.py::SecretValues` -- the secrets one tool-effect dispatch
/// resolved, by name, and the replacements [`SecretValues::scrub`]
/// makes for them.
#[derive(Debug, Default)]
pub struct SecretValues {
    values: std::collections::HashMap<String, String>,
    /// `(text, placeholder)` pairs, longest text first, so a prefixed
    /// value is replaced whole before its bare value is.
    replacements: Vec<(String, String)>,
}

impl SecretValues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Resolves *name* (caching by name for the rest of this dispatch),
    /// records both the bare value and *prefix* plus it as
    /// replacements, and returns `prefix` plus the value.
    pub async fn resolve(
        &mut self,
        name: &str,
        prefix: &str,
        runtime_config: &Value,
        token: &CancellationToken,
    ) -> Result<String, ToolError> {
        let value = match self.values.get(name) {
            Some(value) => value.clone(),
            None => {
                let value = resolve_secret(name, runtime_config, token).await?;
                self.values.insert(name.to_string(), value.clone());
                self.add(value.clone(), format!("[secret:{name}]"));
                value
            }
        };
        let with_prefix = format!("{prefix}{value}");
        if !prefix.is_empty() {
            self.add(with_prefix.clone(), format!("{prefix}[secret:{name}]"));
        }
        Ok(with_prefix)
    }

    fn add(&mut self, text: String, placeholder: String) {
        if self
            .replacements
            .iter()
            .any(|(t, p)| t == &text && p == &placeholder)
        {
            return;
        }
        self.replacements.push((text, placeholder));
        self.replacements
            .sort_by_key(|(text, _)| std::cmp::Reverse(text.len()));
    }

    /// *value* with every resolved secret replaced by its placeholder,
    /// in every string at any depth (mapping keys included).
    pub fn scrub(&self, value: &Value) -> Value {
        if self.replacements.is_empty() {
            return value.clone();
        }
        self.scrub_inner(value)
    }

    /// The same substitution directly over a plain string -- for an
    /// error message or a response body excerpt that never became a
    /// [`Value`] at all.
    pub fn scrub_str(&self, text: &str) -> String {
        let mut text = text.to_string();
        for (needle, placeholder) in &self.replacements {
            text = text.replace(needle.as_str(), placeholder.as_str());
        }
        text
    }

    fn scrub_inner(&self, value: &Value) -> Value {
        match value {
            Value::Str(s) => Value::Str(self.scrub_str(s)),
            Value::Dict(dict) => {
                let mut out = electricity_value::Dict::new();
                for (k, v) in dict {
                    out.insert(self.scrub_inner(k), self.scrub_inner(v));
                }
                Value::Dict(out)
            }
            Value::List(items) => Value::List(items.iter().map(|v| self.scrub_inner(v)).collect()),
            other => other.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    fn runtime_with_secret(name: &str, entry: Value) -> Value {
        let mut secrets = Dict::new();
        secrets.insert(Value::Str(name.to_string()), entry);
        let mut runtime = Dict::new();
        runtime.insert(Value::Str("secrets".to_string()), Value::Dict(secrets));
        Value::Dict(runtime)
    }

    fn env_entry(var: &str) -> Value {
        let mut d = Dict::new();
        d.insert(Value::Str("env".to_string()), Value::Str(var.to_string()));
        Value::Dict(d)
    }

    #[tokio::test]
    async fn resolves_from_an_environment_variable() {
        let var = "ELECTRICITY_SECRETS_TEST_ENV_VAR";
        // SAFETY: test-local env var, no other test reads this name.
        unsafe {
            std::env::set_var(var, "value-123");
        }
        let runtime = runtime_with_secret("api", env_entry(var));
        let token = CancellationToken::new();
        let value = resolve_secret("api", &runtime, &token).await.unwrap();
        assert_eq!(value, "value-123");
        unsafe {
            std::env::remove_var(var);
        }
    }

    #[tokio::test]
    async fn an_undefined_secret_names_itself() {
        let runtime = Value::Dict(Dict::new());
        let token = CancellationToken::new();
        let err = resolve_secret("api", &runtime, &token).await.unwrap_err();
        assert_eq!(
            err.message,
            "secret 'api' is not defined in runtime.secrets."
        );
    }

    #[tokio::test]
    async fn an_unset_env_var_names_the_secret_and_the_variable() {
        let runtime = runtime_with_secret("api", env_entry("ELECTRICITY_SECRETS_TEST_UNSET"));
        let token = CancellationToken::new();
        let err = resolve_secret("api", &runtime, &token).await.unwrap_err();
        assert_eq!(
            err.message,
            "secret 'api': environment variable 'ELECTRICITY_SECRETS_TEST_UNSET' is not set."
        );
    }

    #[tokio::test]
    async fn an_empty_value_is_rejected() {
        let var = "ELECTRICITY_SECRETS_TEST_EMPTY";
        unsafe {
            std::env::set_var(var, "");
        }
        let runtime = runtime_with_secret("api", env_entry(var));
        let token = CancellationToken::new();
        let err = resolve_secret("api", &runtime, &token).await.unwrap_err();
        assert_eq!(err.message, "secret 'api' resolved to an empty value.");
        unsafe {
            std::env::remove_var(var);
        }
    }

    #[tokio::test]
    async fn a_command_runs_and_trims_its_stdout() {
        let mut d = Dict::new();
        d.insert(
            Value::Str("command".to_string()),
            Value::List(vec![
                Value::Str("printf".to_string()),
                Value::Str("  token-xyz\\n".to_string()),
            ]),
        );
        let runtime = runtime_with_secret("api", Value::Dict(d));
        let token = CancellationToken::new();
        let value = resolve_secret("api", &runtime, &token).await.unwrap();
        assert_eq!(value, "token-xyz");
    }

    #[test]
    fn secret_values_scrubs_the_bare_value_and_a_prefixed_echo() {
        let mut values = SecretValues::new();
        values
            .values
            .insert("api".to_string(), "sekret".to_string());
        values.add("sekret".to_string(), "[secret:api]".to_string());
        values.add(
            "Bearer sekret".to_string(),
            "Bearer [secret:api]".to_string(),
        );
        assert_eq!(values.scrub_str("token=sekret"), "token=[secret:api]");
        assert_eq!(
            values.scrub_str("Authorization: Bearer sekret"),
            "Authorization: Bearer [secret:api]"
        );
    }

    #[test]
    fn scrub_recurses_through_dicts_and_lists() {
        let mut values = SecretValues::new();
        values
            .values
            .insert("api".to_string(), "sekret".to_string());
        values.add("sekret".to_string(), "[secret:api]".to_string());
        let mut inner = Dict::new();
        inner.insert(
            Value::Str("k".to_string()),
            Value::Str("sekret".to_string()),
        );
        let value = Value::List(vec![Value::Dict(inner)]);
        let scrubbed = values.scrub(&value);
        let Value::List(ref items) = scrubbed else {
            panic!()
        };
        let Value::Dict(dict) = &items[0] else {
            panic!()
        };
        assert_eq!(
            dict.get(&Value::Str("k".to_string())),
            Some(&Value::Str("[secret:api]".to_string()))
        );
    }
}
