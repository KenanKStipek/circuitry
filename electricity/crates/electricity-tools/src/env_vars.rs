//! `plugins/env_vars.py::EnvVarsPlugin`, ported -- `get`/`list`, exact
//! error texts, the secret-masking pattern in `list`.

use crate::fs::{as_bool, get, py_truthy, value_error};
use crate::{CheckResult, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::{Dict, Value};

/// `_SECRET_PATTERN = re.compile(r"(?:API[_-]?KEY|TOKEN|PASSWORD|
/// PASSWD|SECRET|CREDENTIAL|PRIVATE[_-]?KEY)", re.IGNORECASE)`'s own
/// case-insensitive substring search, without pulling in a regex
/// engine for seven fixed alternatives: everything but the two
/// optional-separator alternatives is a plain substring check; `API`/
/// `PRIVATE` each optionally take a single `_`/`-` before `KEY`.
fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    const PLAIN: &[&str] = &["TOKEN", "PASSWORD", "PASSWD", "SECRET", "CREDENTIAL"];
    if PLAIN.iter().any(|needle| upper.contains(needle)) {
        return true;
    }
    contains_optional_sep(&upper, "API", "KEY") || contains_optional_sep(&upper, "PRIVATE", "KEY")
}

fn contains_optional_sep(haystack: &str, prefix: &str, suffix: &str) -> bool {
    haystack.contains(&format!("{prefix}{suffix}"))
        || haystack.contains(&format!("{prefix}_{suffix}"))
        || haystack.contains(&format!("{prefix}-{suffix}"))
}

pub struct EnvVarsPlugin;

#[async_trait(?Send)]
impl ToolPlugin for EnvVarsPlugin {
    fn name(&self) -> &str {
        "env_vars"
    }

    async fn execute(&self, params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        // `mode = str(params.get("mode") or "get").lower()` -- any
        // falsy value (absent key, explicit `null`, `""`, `0`, ...)
        // defaults to `"get"`.
        let mode_raw = get(&params, "mode").cloned().unwrap_or(Value::None);
        let mode = if py_truthy(&mode_raw) {
            mode_raw.py_str().to_lowercase()
        } else {
            "get".to_string()
        };

        if mode == "get" {
            let name = match get(&params, "name") {
                Some(Value::Str(s)) if !s.is_empty() => s.clone(),
                _ => return Err(value_error("env_vars: get requires params['name'].")),
            };
            let present = std::env::var_os(&name).is_some();
            let value = if present {
                // `std::env::var` panics on a non-UTF-8 value (`cof`'s
                // own `os.environ.get` applies `surrogateescape`,
                // never raising); `to_string_lossy` never panics --
                // issue #460's own fix pass.
                Value::Str(
                    std::env::var_os(&name)
                        .map(|v| v.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                )
            } else {
                get(&params, "default").cloned().unwrap_or(Value::None)
            };
            let mut raw = Dict::new();
            raw.insert(Value::Str("name".to_string()), Value::Str(name));
            raw.insert(Value::Str("present".to_string()), Value::from(present));
            return Ok(ToolResult::new(value, Value::Dict(raw)));
        }

        if mode == "list" {
            let prefix = match get(&params, "prefix") {
                Some(Value::Str(s)) if !s.is_empty() => Some(s.clone()),
                _ => None,
            };
            let include_secrets = as_bool(get(&params, "include_secrets"), false);
            let mut items = Dict::new();
            // `std::env::vars()` panics on a non-UTF-8 key or value;
            // `vars_os()` + `to_string_lossy()` never does (issue
            // #460's own fix pass -- `cof run`'s own
            // `os.environ.items()` decodes with `surrogateescape` and
            // carries on, never raising).
            for (name_os, value_os) in std::env::vars_os() {
                let name = name_os.to_string_lossy().into_owned();
                let value = value_os.to_string_lossy().into_owned();
                if let Some(prefix) = &prefix {
                    if !name.starts_with(prefix.as_str()) {
                        continue;
                    }
                }
                let masked = if !include_secrets && is_secret_name(&name) {
                    "***".to_string()
                } else {
                    value
                };
                items.insert(Value::Str(name), Value::Str(masked));
            }
            let mut raw = Dict::new();
            raw.insert(
                Value::Str("count".to_string()),
                Value::from(items.len() as i64),
            );
            raw.insert(
                Value::Str("include_secrets".to_string()),
                Value::from(include_secrets),
            );
            return Ok(ToolResult::new(Value::Dict(items), Value::Dict(raw)));
        }

        Err(value_error(format!(
            "env_vars: unknown mode {}",
            Value::Str(mode).py_repr()
        )))
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
    use electricity_value::CancellationToken;

    fn dict(pairs: Vec<(&str, Value)>) -> Value {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(d)
    }

    async fn run(params: Value) -> Result<ToolResult, ToolError> {
        let token = CancellationToken::new();
        let call = ToolCall {
            timeout_seconds: 30,
            config: &Value::None,
            token: &token,
            armed: false,
        };
        EnvVarsPlugin.execute(params, &call).await
    }

    #[tokio::test]
    async fn get_of_an_unset_name_returns_the_default() {
        let result = run(dict(vec![
            ("mode", Value::from("get")),
            ("name", Value::from("ELECTRICITY_D1_TEST_UNSET_VAR")),
            ("default", Value::from("fallback")),
        ]))
        .await
        .unwrap();
        assert_eq!(result.value, Value::from("fallback"));
        let raw = result.raw.as_dict().unwrap();
        assert_eq!(
            raw.get(&Value::Str("present".to_string())),
            Some(&Value::from(false))
        );
    }

    #[tokio::test]
    async fn get_requires_a_non_empty_name() {
        let err = run(dict(vec![("mode", Value::from("get"))]))
            .await
            .unwrap_err();
        assert_eq!(err.message, "env_vars: get requires params['name'].");
    }

    #[tokio::test]
    async fn mode_defaults_to_get_when_absent_or_falsy() {
        let result = run(dict(vec![
            ("name", Value::from("ELECTRICITY_D1_TEST_UNSET_VAR")),
            ("default", Value::from(1i64)),
        ]))
        .await
        .unwrap();
        assert_eq!(result.value, Value::from(1i64));
    }

    #[tokio::test]
    async fn unknown_mode_reports_its_own_repr() {
        let err = run(dict(vec![("mode", Value::from("bogus"))]))
            .await
            .unwrap_err();
        assert_eq!(err.message, "env_vars: unknown mode 'bogus'");
    }

    #[tokio::test]
    async fn list_mode_never_panics_on_a_non_utf8_value() {
        // `std::env::vars()`/`std::env::var()` panic on a non-UTF-8
        // key or value (`library/std/src/env.rs`'s own `.unwrap()`);
        // `cof run`'s own `os.environ` decodes with `surrogateescape`
        // instead, never raising -- issue #460's own fix pass.
        // `get`'s own `std::env::var_os` + `to_string_lossy` fix is
        // covered by this same code path (both modes share it).
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let key = "ELECTRICITY_D1_TEST_NON_UTF8_VAR";
        let bad_value = OsStr::from_bytes(&[0x66, 0x6f, 0xff, 0x6f]); // b"fo\xffo"
        // SAFETY: a uniquely-named variable this test alone sets and
        // removes, read back only from this same (single) call below
        // -- no other test in this binary touches this name.
        unsafe { std::env::set_var(key, bad_value) };
        let result = run(dict(vec![
            ("mode", Value::from("list")),
            ("prefix", Value::from(key)),
        ]))
        .await;
        unsafe { std::env::remove_var(key) };
        let result = result.unwrap();
        let items = result.value.as_dict().unwrap();
        assert_eq!(
            items.get(&Value::Str(key.to_string())),
            Some(&Value::Str("fo\u{fffd}o".to_string()))
        );
    }

    #[test]
    fn is_secret_name_matches_the_boundary_case_from_tools_7_case_12() {
        // electricity/docs/spec/tools-adapters-plugins.md §7 case 12:
        // `API[_-]?KEY` is a plain substring search, no word boundary --
        // `MY_APIKEYLESS_VAR` contains `APIKEY` as a literal substring,
        // so it's a secret too, same as Python's own regex.
        assert!(is_secret_name("MY_API_KEY_SUFFIX"));
        assert!(is_secret_name("MY_APIKEYLESS_VAR"));
        assert!(is_secret_name("PRIVATE-KEY"));
        assert!(!is_secret_name("MY_NAME"));
    }
}
