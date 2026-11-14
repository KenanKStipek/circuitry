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

/// `secrets.py::_json_escaped_variants` (issue #600): how *text* can
/// appear inside a JSON string literal a server wrote -- `json.dumps`'s
/// own escaping (which also turns a non-ASCII character into `\uXXXX`),
/// and that same form with `/` additionally escaped to `\/`, a choice
/// `json.dumps` doesn't make but some encoders do. A plain, unescaped
/// match for *text* never matches either form, so a secret a JSON error
/// body echoes this way would otherwise survive the scrub.
fn json_escaped_variants(text: &str) -> [String; 2] {
    let dumped = electricity_json::dumps(
        &Value::Str(text.to_string()),
        electricity_json::WriteMode::COMPACT,
    )
    .expect("a plain string is always JSON-serializable");
    // Strip the surrounding quotes `dumps` always wraps a string in.
    let escaped = dumped[1..dumped.len() - 1].to_string();
    let escaped_slash = escaped.replace('/', "\\/");
    [escaped, escaped_slash]
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
    match process::run_tracked(argv, &opts, token, None, None, None).await {
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

/// `secrets.py::_MIN_TAIL_SECRET_LINE_CHARS`: the shortest line of a
/// multi-line secret [`SecretValues::scrub_live_tail`] looks for.
pub const MIN_TAIL_SECRET_LINE_CHARS: usize = 4;

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
        for variant in std::iter::once(text.clone()).chain(json_escaped_variants(&text)) {
            if !self
                .replacements
                .iter()
                .any(|(t, p)| t == &variant && p == &placeholder)
            {
                self.replacements.push((variant, placeholder.clone()));
            }
        }
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

    /// One of a running tool's live tails (#741) with every line that
    /// contains a line of a resolved secret -- at least
    /// [`MIN_TAIL_SECRET_LINE_CHARS`] characters of it -- dropped, on top
    /// of [`Self::scrub_str`]'s exact-value replacement, which runs first
    /// (`secrets.py::SecretValues.scrub_live_tail`). A tail is bounded in
    /// lines and bytes, so a multi-line secret can reach it only in part,
    /// with no whole value there for `scrub_str` to replace; and a tail
    /// never keeps a fragment of a line cut by its byte cap, so the cap
    /// can't expose part of a secret line either.
    pub fn scrub_live_tail(&self, text: &str) -> String {
        let secret_lines: Vec<&str> = self
            .values
            .values()
            .flat_map(|value| value.split('\n'))
            .filter(|line| line.chars().count() >= MIN_TAIL_SECRET_LINE_CHARS)
            .collect();
        if secret_lines.is_empty() || text.is_empty() {
            return text.to_string();
        }
        text.split('\n')
            .filter(|line| !secret_lines.iter().any(|secret| line.contains(secret)))
            .collect::<Vec<_>>()
            .join("\n")
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

    #[test]
    fn a_live_tail_drops_a_line_holding_part_of_a_multi_line_secret() {
        let mut secrets = SecretValues::new();
        let value = "BEGIN-KEY\nabcd1234\nEND".to_string();
        secrets.values.insert("key".to_string(), value.clone());
        secrets.add(value, "[secret:key]".to_string());
        // Only the first two lines of the secret reached the tail.
        let tail = "start\nBEGIN-KEY\nabcd1234\nok";
        assert_eq!(
            secrets.scrub_live_tail(&secrets.scrub_str(tail)),
            "start\nok"
        );
    }

    #[test]
    fn a_secret_line_under_four_characters_never_drops_a_tail_line() {
        let mut secrets = SecretValues::new();
        secrets
            .values
            .insert("key".to_string(), "abc\nlonger-line".to_string());
        assert_eq!(secrets.scrub_live_tail("xabcx\nfine"), "xabcx\nfine");
    }

    #[test]
    fn a_whole_single_line_secret_in_a_tail_is_replaced_not_dropped() {
        let mut secrets = SecretValues::new();
        secrets
            .values
            .insert("tok".to_string(), "s3cret-value".to_string());
        secrets.add("s3cret-value".to_string(), "[secret:tok]".to_string());
        let tail = "token=s3cret-value\nnext";
        assert_eq!(
            secrets.scrub_live_tail(&secrets.scrub_str(tail)),
            "token=[secret:tok]\nnext"
        );
    }

    #[test]
    fn json_escaped_variants_covers_slash_and_non_ascii() {
        let variants = json_escaped_variants("a/béc");
        assert!(variants.contains(&"a\\/b\\u00e9c".to_string()));
        assert!(variants.contains(&"a/b\\u00e9c".to_string()));
    }

    #[test]
    fn scrub_str_matches_a_secrets_escaped_slash_form() {
        let mut secrets = SecretValues::new();
        secrets
            .values
            .insert("api".to_string(), "sec/ret-600".to_string());
        secrets.add("sec/ret-600".to_string(), "[secret:api]".to_string());
        assert_eq!(
            secrets.scrub_str("bare: sec/ret-600, escaped: sec\\/ret-600"),
            "bare: [secret:api], escaped: [secret:api]"
        );
    }

    #[test]
    fn scrub_str_matches_a_secrets_escaped_non_ascii_form() {
        let mut secrets = SecretValues::new();
        let value = "töken-600".to_string();
        secrets.values.insert("api".to_string(), value.clone());
        secrets.add(value.clone(), "[secret:api]".to_string());
        assert_eq!(
            secrets.scrub_str(&format!("bare: {value}, escaped: t\\u00f6ken-600")),
            "bare: [secret:api], escaped: [secret:api]"
        );
    }

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

    /// A `{keychain: {service, account?}}` entry, resolved through a
    /// fake `security` binary on a temp `PATH` prefix -- this crate's
    /// test suite never runs the real `security find-generic-password`
    /// against the account's own Keychain. The fake records the argv
    /// it was called with (to a side-channel file this test reads
    /// back), so the assertion also proves `-s`/`-a`'s own shape, not
    /// just that *some* "security" ran.
    ///
    /// SAFETY/hazard: this mutates the whole process' own `PATH` for
    /// the width of one `resolve_secret` call, restored before this
    /// test's own assertions run -- a concurrent test in this same
    /// binary that spawns a process by bare name during that narrow
    /// window could see the prefixed `PATH` too (the same pre-existing
    /// hazard this file's other tests already accept for `unsafe {
    /// std::env::set_var }` on a secret value's own env var). `security`
    /// is only ever prepended, never removed from the rest of `PATH`,
    /// so nothing else this crate's own tests spawn by bare name (e.g.
    /// `printf`) is affected either way.
    #[tokio::test]
    async fn resolves_from_the_keychain_through_a_fake_security_binary() {
        let dir = std::env::temp_dir().join(format!(
            "electricity-secrets-keychain-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let argv_log = dir.join("argv.txt");
        let fake = dir.join("security");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" > {}\nprintf 'keychain-value-xyz'\n",
                argv_log.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let original_path = std::env::var_os("PATH");
        let new_path = match &original_path {
            Some(existing) => {
                let mut joined = dir.clone().into_os_string();
                joined.push(":");
                joined.push(existing);
                joined
            }
            None => dir.clone().into_os_string(),
        };
        // SAFETY: restored immediately below, before this test's own
        // assertions run.
        unsafe {
            std::env::set_var("PATH", &new_path);
        }

        let mut spec = Dict::new();
        spec.insert(Value::Str("service".to_string()), Value::from("circuitry"));
        spec.insert(Value::Str("account".to_string()), Value::from("github"));
        let mut entry = Dict::new();
        entry.insert(Value::Str("keychain".to_string()), Value::Dict(spec));
        let runtime = runtime_with_secret("api", Value::Dict(entry));
        let token = CancellationToken::new();
        let result = resolve_secret("api", &runtime, &token).await;

        // SAFETY: restoring the process' own PATH before this test
        // looks at anything else.
        unsafe {
            match &original_path {
                Some(existing) => std::env::set_var("PATH", existing),
                None => std::env::remove_var("PATH"),
            }
        }

        let value = result.unwrap();
        assert_eq!(value, "keychain-value-xyz");
        let argv_text = std::fs::read_to_string(&argv_log).unwrap();
        assert_eq!(
            argv_text.trim(),
            "find-generic-password -w -s circuitry -a github"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
