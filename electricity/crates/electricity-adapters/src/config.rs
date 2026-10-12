//! Small helpers every builder in this crate shares for reading
//! `runtime.adapters.<name>` config the way `adapters/factory.py`'s
//! own builders do: `cfg.get(key) or default` -- a `dict.get` lookup,
//! then Python's own truthiness (`None`, `""`, `0`, `0.0`, an empty
//! collection) falling back to *default*, never just a missing key.

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
///
/// This is correct for a field Python's own adapter never calls a
/// string method on directly (`default_model`, `account_id`, ...) --
/// a truthy non-string value there just flows through to an f-string
/// or a JSON field, which `str()`s it without ever raising. It is
/// *not* correct for `base_url`/`endpoint`, which every adapter calls
/// `.rstrip("/")` on -- see [`cfg_str_or_rstrip_target`] for those.
pub fn cfg_str_or(cfg: &Value, key: &str, default: &str) -> String {
    match cfg_get(cfg, key) {
        Some(Value::Str(s)) if !s.is_empty() => s.clone(),
        _ => default.to_string(),
    }
}

/// `cfg.get(key) or default`, for `base_url`/`endpoint` -- the two
/// keys every adapter in this crate eventually calls `.rstrip("/")`
/// on (`adapters/ollama.py`/`openai.py`/`anthropic.py`/`replicate.py`/
/// `watsonx.py`'s own direct call, or `_openai_compat.py::
/// chat_completion`'s shared one for the whole OpenAI-compatible
/// family). Python's own `cfg.get(key) or default` keeps a truthy
/// non-string value as-is -- it is `self.base_url.rstrip("/")`, called
/// later, that actually raises `AttributeError: '<type>' object has
/// no attribute 'rstrip'` (issue #515's own new finding). Every
/// adapter here is rebuilt fresh per call and used immediately
/// (`electricity_vm::adapter::AdapterRegistry`'s own doc comment), so
/// raising this at build time, once, rather than only once something
/// later actually calls the string-trim equivalent, reaches the same
/// outcome for every real caller -- [`cfg_str_or`] above must never
/// silently coerce this value to *default* instead.
pub fn cfg_str_or_rstrip_target(
    cfg: &Value,
    key: &str,
    default: &str,
) -> Result<String, AdapterError> {
    match cfg_get(cfg, key) {
        Some(Value::Str(s)) if !s.is_empty() => Ok(s.clone()),
        Some(value) if value_is_truthy(value) => Err(AdapterError {
            message: format!("'{}' object has no attribute 'rstrip'", value.type_name()),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::AttributeError,
        }),
        _ => Ok(default.to_string()),
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
    fn cfg_str_or_rstrip_target_falls_back_on_absence_or_falsy_values() {
        let cfg = dict(&[("base_url", Value::Str("".to_string()))]);
        assert_eq!(
            cfg_str_or_rstrip_target(&cfg, "base_url", "default").unwrap(),
            "default"
        );
        assert_eq!(
            cfg_str_or_rstrip_target(&cfg, "missing", "default").unwrap(),
            "default"
        );
        assert_eq!(
            cfg_str_or_rstrip_target(&Value::None, "base_url", "default").unwrap(),
            "default"
        );
    }

    #[test]
    fn cfg_str_or_rstrip_target_keeps_a_truthy_string() {
        let cfg = dict(&[("base_url", Value::Str("http://x".to_string()))]);
        assert_eq!(
            cfg_str_or_rstrip_target(&cfg, "base_url", "default").unwrap(),
            "http://x"
        );
    }

    #[test]
    fn cfg_str_or_rstrip_target_fails_like_python_on_a_truthy_non_string() {
        // `runtime.adapters.<name>.base_url: 5` -- Python's own
        // `cfg.get("base_url") or default` keeps the int, which then
        // raises `AttributeError: 'int' object has no attribute
        // 'rstrip'` the first time `self.base_url.rstrip("/")` runs.
        let cfg = dict(&[("base_url", Value::from(5i64))]);
        let err = cfg_str_or_rstrip_target(&cfg, "base_url", "default").unwrap_err();
        assert_eq!(err.message, "'int' object has no attribute 'rstrip'");
        assert_eq!(err.py_class, PyExc::AttributeError);
        assert!(!err.retry.retryable);
    }

    #[test]
    fn cfg_str_or_rstrip_target_falls_back_for_a_falsy_non_string() {
        // `0`/`False`/`[]`/... are falsy in Python too -- `or default`
        // picks *default* without ever reaching `.rstrip`, so this
        // must not raise.
        let cfg = dict(&[("base_url", Value::from(0i64))]);
        assert_eq!(
            cfg_str_or_rstrip_target(&cfg, "base_url", "default").unwrap(),
            "default"
        );
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
}
