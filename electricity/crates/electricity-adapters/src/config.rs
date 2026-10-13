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

/// `cfg.get(key) or default`, kept **raw** -- for `base_url`/
/// `endpoint`, the two keys every adapter in this crate eventually
/// calls `.rstrip("/")` on (`adapters/ollama.py`/`openai.py`/
/// `anthropic.py`/`replicate.py`/`watsonx.py`'s own direct call, or
/// `_openai_compat.py::chat_completion`'s shared one for the whole
/// OpenAI-compatible family). Python's own `cfg.get(key) or default`
/// keeps a truthy non-string value as-is -- it is `self.base_url.
/// rstrip("/")`, called *later* (inside `generate()`/`check()`, after
/// whatever earlier checks that specific adapter runs first -- an API
/// key check, an IAM token exchange, ...), that actually raises
/// `AttributeError: '<type>' object has no attribute 'rstrip'` (issue
/// #515's own new finding, review finding F5: raising this at *build*
/// time instead, unconditionally, refuses preflight for an adapter
/// whose own `check()` never looks at `base_url` at all, and runs a
/// watsonx IAM exchange in the wrong order relative to it). Builders
/// store this raw value and each adapter calls [`rstrip_base_url`] on
/// it at its own Python call site instead.
pub fn cfg_value_or(cfg: &Value, key: &str, default: &str) -> Value {
    match cfg_get(cfg, key) {
        Some(v) if value_is_truthy(v) => v.clone(),
        _ => Value::Str(default.to_string()),
    }
}

/// `<value>.rstrip("/")` -- *value* is a [`cfg_value_or`] result (or
/// any other raw config value an adapter calls `.rstrip("/")` on). A
/// non-string raises the same `AttributeError` Python's own call
/// raises, by *type name* (`'int' object has no attribute 'rstrip'`).
pub fn rstrip_base_url(value: &Value) -> Result<String, AdapterError> {
    match value {
        Value::Str(s) => Ok(s.trim_end_matches('/').to_string()),
        other => Err(AdapterError {
            message: format!("'{}' object has no attribute 'rstrip'", other.type_name()),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::AttributeError,
        }),
    }
}

/// `int(cfg.get(key) or default)` -- the eager `int()` coercion
/// `adapters/factory.py`'s own builders apply to a handful of numeric
/// config fields (`cyberdiner`'s `poll_interval_ms`/`timeout_seconds`/
/// `max_in_flight`, `anthropic`'s `max_tokens`) *at build time*,
/// unlike [`rstrip_base_url`] above -- Python's own `int(x)` builtin
/// fails immediately when the factory function runs, not later, so
/// raising it here (rather than deferring like `base_url`) matches.
/// [`electricity_value::pycompat::py_int`] is CPython's own `int()`
/// coercion (accepts `bool`/`int`/`float`/a numeric `str`, raises
/// `TypeError`/`ValueError` with CPython's own text otherwise).
pub fn cfg_int_or(cfg: &Value, key: &str, default: i64) -> Result<i64, AdapterError> {
    let raw = match cfg_get(cfg, key) {
        Some(v) if value_is_truthy(v) => v.clone(),
        _ => Value::from(default),
    };
    electricity_value::pycompat::py_int(&raw).map_err(|e| AdapterError {
        message: e.message,
        retry: RetryInfo::not_retryable(),
        py_class: e.py_class,
    })
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
    fn cfg_value_or_falls_back_on_absence_or_falsy_values() {
        let cfg = dict(&[("base_url", Value::Str("".to_string()))]);
        assert_eq!(
            cfg_value_or(&cfg, "base_url", "default"),
            Value::Str("default".to_string())
        );
        assert_eq!(
            cfg_value_or(&cfg, "missing", "default"),
            Value::Str("default".to_string())
        );
        assert_eq!(
            cfg_value_or(&Value::None, "base_url", "default"),
            Value::Str("default".to_string())
        );
    }

    #[test]
    fn cfg_value_or_keeps_a_truthy_value_raw() {
        let cfg = dict(&[("base_url", Value::Str("http://x".to_string()))]);
        assert_eq!(
            cfg_value_or(&cfg, "base_url", "default"),
            Value::Str("http://x".to_string())
        );
        let cfg = dict(&[("base_url", Value::from(5i64))]);
        assert_eq!(cfg_value_or(&cfg, "base_url", "default"), Value::from(5i64));
    }

    #[test]
    fn rstrip_base_url_strips_a_trailing_slash() {
        assert_eq!(
            rstrip_base_url(&Value::Str("http://x///".to_string())).unwrap(),
            "http://x"
        );
    }

    #[test]
    fn rstrip_base_url_fails_like_python_on_a_non_string() {
        // `runtime.adapters.<name>.base_url: 5` -- Python's own
        // `cfg.get("base_url") or default` keeps the int, which then
        // raises `AttributeError: 'int' object has no attribute
        // 'rstrip'` the first time `self.base_url.rstrip("/")` runs.
        let err = rstrip_base_url(&Value::from(5i64)).unwrap_err();
        assert_eq!(err.message, "'int' object has no attribute 'rstrip'");
        assert_eq!(err.py_class, PyExc::AttributeError);
        assert!(!err.retry.retryable);
    }

    #[test]
    fn cfg_int_or_coerces_a_numeric_string_and_a_float() {
        let cfg = dict(&[("poll_interval_ms", Value::Str("250".to_string()))]);
        assert_eq!(cfg_int_or(&cfg, "poll_interval_ms", 500).unwrap(), 250);
        let cfg = dict(&[("timeout_seconds", Value::Float(12.9))]);
        assert_eq!(cfg_int_or(&cfg, "timeout_seconds", 30).unwrap(), 12);
    }

    #[test]
    fn cfg_int_or_falls_back_for_a_falsy_value() {
        let cfg = dict(&[("max_in_flight", Value::from(0i64))]);
        assert_eq!(cfg_int_or(&cfg, "max_in_flight", 7).unwrap(), 7);
        assert_eq!(cfg_int_or(&Value::None, "max_in_flight", 7).unwrap(), 7);
    }

    #[test]
    fn cfg_int_or_fails_like_pythons_int_builtin() {
        let cfg = dict(&[("timeout_seconds", Value::Str("x".to_string()))]);
        let err = cfg_int_or(&cfg, "timeout_seconds", 30).unwrap_err();
        assert_eq!(err.message, "invalid literal for int() with base 10: 'x'");
        assert_eq!(err.py_class, PyExc::ValueError);
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
