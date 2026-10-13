//! Small helpers every builder in this crate shares for reading
//! `runtime.adapters.<name>` config the way `adapters/factory.py`'s
//! own builders do: `cfg.get(key) or default` -- a `dict.get` lookup,
//! then Python's own truthiness (`None`, `""`, `0`, `0.0`, an empty
//! collection) falling back to *default*, never just a missing key.
//!
//! [`cfg_value_or`]/[`require_str`]/[`resolve_truthy_str`]/[`string_list`]
//! (issue #520 part B) widen this module for the `claude_code`/`pi`
//! adapters' own config rule (B5): a *truthy non-string* config value
//! (`binary: 5`, `thinking: true`) is never replaced by the default
//! the way [`cfg_str_or`] would -- Python keeps it verbatim and fails
//! only once something actually tries to use it as a string (`shutil.
//! which`, a real `Popen`'s own `cmd[0]`), so falling back silently
//! here would start the real CLI instead of reporting that failure.

use electricity_value::Value;
use electricity_vm::adapter::{AdapterError, PyExc};
use electricity_vm::retry::RetryInfo;

/// `cfg.get(key)` -- `None` when *cfg* isn't a dict, or the key is
/// absent. Distinct from [`cfg_str_or`]'s own `or`-defaulting: a
/// handful of builders (`nvidia-nim`'s/`tgi`'s own `api_key_env`) need
/// to tell "the key is absent" apart from "the key is present but
/// `None`/falsy".
pub fn cfg_get<'a>(cfg: &'a Value, key: &str) -> Option<&'a Value> {
    cfg.as_dict()?.get(&Value::Str(key.to_string()))
}

/// Python truthiness (`bool(value)`): `None`, `False`, a zero number,
/// an empty string/bytes/list/dict are all falsy; everything else
/// (including a non-empty collection, and every `Date`/`DateTime`) is
/// truthy.
pub fn value_is_truthy(value: &Value) -> bool {
    match value {
        Value::None => false,
        Value::Bool(b) => *b,
        Value::Int(i) => !i.is_zero(),
        Value::Float(f) => *f != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::Bytes(b) => !b.is_empty(),
        Value::List(l) => !l.is_empty(),
        Value::Dict(d) => !d.is_empty(),
        _ => true,
    }
}

/// `cfg.get(key) or default` for a string field -- the common case
/// among every builder's own base-url/default-model/account-id
/// fields: a config value of `""` (or a non-string value) falls back
/// to *default* exactly as Python's own `or` does.
pub fn cfg_str_or(cfg: &Value, key: &str, default: &str) -> String {
    match cfg_get(cfg, key) {
        Some(Value::Str(s)) if !s.is_empty() => s.clone(),
        _ => default.to_string(),
    }
}

/// `cfg.get(key) or default`, kept as the raw post-`or` [`Value`] --
/// unlike [`cfg_str_or`], a *truthy* value is never coerced to a
/// string: an absent/falsy config value becomes `Value::Str(default)`,
/// but a truthy non-string (`binary: 5`) is kept exactly as configured
/// (B5), to be resolved -- or to raise Python's own `TypeError` -- only
/// where it is actually used ([`require_str`]/[`resolve_truthy_str`]).
pub fn cfg_value_or(cfg: &Value, key: &str, default: &str) -> Value {
    match cfg_get(cfg, key) {
        Some(value) if value_is_truthy(value) => value.clone(),
        _ => Value::Str(default.to_string()),
    }
}

/// Python's own `expected str, bytes or os.PathLike object, not
/// <type>` -- `os.fspath()`'s exact text, confirmed directly against
/// both `shutil.which(5)` and `subprocess.Popen([5])`/
/// `subprocess.Popen(["claude", 5])` under CPython 3.11: the same
/// message either way, so one `AdapterError` shape covers `binary`'s
/// own `check()` use and every call-site use (`cmd[0]`, `--model`,
/// `--thinking`) alike.
pub fn type_error(value: &Value) -> AdapterError {
    AdapterError {
        message: format!(
            "expected str, bytes or os.PathLike object, not {}",
            value.type_name()
        ),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::TypeError,
    }
}

/// A config string that is always used, regardless of truthiness
/// (`binary`, B5) -- *value* is already the result of [`cfg_value_or`],
/// so it is guaranteed non-falsy; a non-`Value::Str` here is therefore
/// always a truthy non-string, and [`type_error`] applies.
pub fn require_str(value: &Value) -> Result<&str, AdapterError> {
    match value {
        Value::Str(s) => Ok(s),
        other => Err(type_error(other)),
    }
}

/// A config string that is used only when truthy (`default_model`,
/// `thinking`, B5): Python's own `if model:`/`if thinking:` inside
/// `claude_command`/`pi_command` -- a falsy value (including a falsy
/// *non*-string: `0`, `False`, `[]`) resolves to `""` (no flag added),
/// and only a truthy non-string raises [`type_error`] (the same
/// `TypeError` a real `Popen` would once the raw value actually
/// reached its argv).
pub fn resolve_truthy_str(value: &Value) -> Result<String, AdapterError> {
    if !value_is_truthy(value) {
        return Ok(String::new());
    }
    match value {
        Value::Str(s) => Ok(s.clone()),
        other => Err(type_error(other)),
    }
}

/// `adapters/factory.py::_string_list` -- `cfg[key]` as a list of
/// strings, *default* when the key is absent or explicitly `null`
/// (never type-checked the way a present value is); a present value
/// that isn't a list, or contains anything but strings, fails with
/// Python's own `ValueError` text, `raw`'s own [`Value::py_repr`].
pub fn string_list(
    cfg: &Value,
    key: &str,
    adapter_name: &str,
    default: &[&str],
) -> Result<Vec<String>, AdapterError> {
    match cfg_get(cfg, key) {
        None | Some(Value::None) => Ok(default.iter().map(|s| (*s).to_string()).collect()),
        Some(Value::List(items)) if items.iter().all(|item| matches!(item, Value::Str(_))) => {
            Ok(items
                .iter()
                .map(|item| match item {
                    Value::Str(s) => s.clone(),
                    _ => unreachable!("already checked above: every item is Value::Str"),
                })
                .collect())
        }
        Some(other) => Err(AdapterError {
            message: format!(
                "runtime.adapters.{adapter_name}.{key} must be a list of strings, got {}",
                other.py_repr()
            ),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::ValueError,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    fn dict(pairs: &[(&str, Value)]) -> Value {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v.clone());
        }
        Value::Dict(d)
    }

    #[test]
    fn cfg_str_or_falls_back_on_absence_or_falsy_values() {
        let cfg = dict(&[("base_url", Value::Str("".to_string()))]);
        assert_eq!(cfg_str_or(&cfg, "base_url", "default"), "default");
        assert_eq!(cfg_str_or(&cfg, "missing", "default"), "default");
        assert_eq!(cfg_str_or(&Value::None, "base_url", "default"), "default");
    }

    #[test]
    fn cfg_str_or_keeps_a_truthy_value() {
        let cfg = dict(&[("base_url", Value::Str("http://x".to_string()))]);
        assert_eq!(cfg_str_or(&cfg, "base_url", "default"), "http://x");
    }

    #[test]
    fn cfg_get_tells_absent_apart_from_explicit_none() {
        let cfg = dict(&[("api_key_env", Value::None)]);
        assert_eq!(cfg_get(&cfg, "api_key_env"), Some(&Value::None));
        assert_eq!(cfg_get(&cfg, "missing"), None);
    }

    #[test]
    fn value_is_truthy_matches_python() {
        assert!(!value_is_truthy(&Value::None));
        assert!(!value_is_truthy(&Value::Str("".to_string())));
        assert!(!value_is_truthy(&Value::from(0i64)));
        assert!(value_is_truthy(&Value::from(1i64)));
        assert!(value_is_truthy(&Value::Str("x".to_string())));
    }

    #[test]
    fn cfg_value_or_falls_back_on_absence_or_falsy_values() {
        let cfg = dict(&[("binary", Value::Str("".to_string()))]);
        assert_eq!(
            cfg_value_or(&cfg, "binary", "claude"),
            Value::Str("claude".to_string())
        );
        assert_eq!(
            cfg_value_or(&cfg, "missing", "claude"),
            Value::Str("claude".to_string())
        );
        assert_eq!(
            cfg_value_or(&Value::None, "binary", "claude"),
            Value::Str("claude".to_string())
        );
    }

    #[test]
    fn cfg_value_or_keeps_a_truthy_non_string_value_verbatim() {
        let cfg = dict(&[("binary", Value::from(5i64))]);
        assert_eq!(cfg_value_or(&cfg, "binary", "claude"), Value::from(5i64));
    }

    #[test]
    fn require_str_keeps_a_string_and_type_errors_on_anything_else() {
        assert_eq!(require_str(&Value::Str("claude".to_string())), Ok("claude"));
        let err = require_str(&Value::from(5i64)).unwrap_err();
        assert_eq!(
            err.message,
            "expected str, bytes or os.PathLike object, not int"
        );
        assert!(!err.retry.retryable);
        assert_eq!(err.py_class, PyExc::TypeError);
    }

    #[test]
    fn require_str_names_bool_the_same_way_python_does() {
        let err = require_str(&Value::Bool(true)).unwrap_err();
        assert_eq!(
            err.message,
            "expected str, bytes or os.PathLike object, not bool"
        );
    }

    #[test]
    fn resolve_truthy_str_treats_a_falsy_non_string_as_empty() {
        assert_eq!(resolve_truthy_str(&Value::from(0i64)).unwrap(), "");
        assert_eq!(resolve_truthy_str(&Value::Bool(false)).unwrap(), "");
        assert_eq!(resolve_truthy_str(&Value::Str("".to_string())).unwrap(), "");
    }

    #[test]
    fn resolve_truthy_str_keeps_a_truthy_string_and_errors_on_truthy_non_string() {
        assert_eq!(
            resolve_truthy_str(&Value::Str("opus".to_string())).unwrap(),
            "opus"
        );
        let err = resolve_truthy_str(&Value::Bool(true)).unwrap_err();
        assert_eq!(
            err.message,
            "expected str, bytes or os.PathLike object, not bool"
        );
        assert_eq!(err.py_class, PyExc::TypeError);
    }

    #[test]
    fn string_list_defaults_on_absence_or_explicit_null() {
        let cfg = dict(&[("unset_env", Value::None)]);
        assert_eq!(
            string_list(&cfg, "unset_env", "pi", &["A", "B"]).unwrap(),
            vec!["A".to_string(), "B".to_string()]
        );
        assert_eq!(
            string_list(&Value::None, "unset_env", "pi", &["A", "B"]).unwrap(),
            vec!["A".to_string(), "B".to_string()]
        );
    }

    #[test]
    fn string_list_keeps_an_explicit_empty_list_empty() {
        let cfg = dict(&[("unset_env", Value::List(vec![]))]);
        assert_eq!(
            string_list(&cfg, "unset_env", "pi", &["A", "B"]).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn string_list_rejects_a_non_list_with_pythons_value_error_text() {
        let cfg = dict(&[("unset_env", Value::Str("ANTHROPIC_API_KEY".to_string()))]);
        let err = string_list(&cfg, "unset_env", "pi", &[]).unwrap_err();
        assert_eq!(
            err.message,
            "runtime.adapters.pi.unset_env must be a list of strings, got 'ANTHROPIC_API_KEY'"
        );
        assert_eq!(err.py_class, PyExc::ValueError);
        assert!(!err.retry.retryable);
    }

    #[test]
    fn string_list_rejects_a_list_containing_a_non_string() {
        let cfg = dict(&[("extra_args", Value::List(vec![Value::from(1i64)]))]);
        let err = string_list(&cfg, "extra_args", "claude_code", &[]).unwrap_err();
        assert_eq!(
            err.message,
            "runtime.adapters.claude_code.extra_args must be a list of strings, got [1]"
        );
    }
}
