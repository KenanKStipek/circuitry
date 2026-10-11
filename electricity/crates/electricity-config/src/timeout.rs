//! `adapters/factory.py::configured_timeout_seconds` (issue #495, lane
//! R): `runtime.adapters.<adapter_name>.timeout_seconds` for one
//! adapter, or `None` when nothing usable is configured. Lives in this
//! crate (rather than `electricity` (lib), where the rest of lane R's
//! own driver sits) so lane G can reuse it for its own per-attempt
//! `_attempt_timeout_seconds` -- `electricity-vm` already depends on
//! `electricity-config`.

use electricity_value::Value;

/// `(runtime or {}).get("adapters") or {}`, then `.get(name) or {}`.
fn adapter_config_slice<'a>(
    runtime: Option<&'a Value>,
    name: &str,
) -> Option<&'a electricity_value::Dict> {
    runtime
        .and_then(Value::as_dict)
        .and_then(|r| r.get(&Value::Str("adapters".to_string())))
        .and_then(Value::as_dict)
        .and_then(|a| a.get(&Value::Str(name.to_string())))
        .and_then(Value::as_dict)
}

/// `cfg.get("timeout_seconds")`, falling back to the deprecated
/// `timeout` alias for `litellm` only (`_litellm_timeout_raw`) --
/// electricity never builds `litellm`, but this crate ports the
/// lookup exactly, including that one adapter-specific fallback, so a
/// caller's own text matches Python's byte for byte whichever adapter
/// it asks about.
fn raw_timeout_value<'a>(cfg: &'a electricity_value::Dict, name: &str) -> Option<&'a Value> {
    let direct = cfg.get(&Value::Str("timeout_seconds".to_string()));
    if direct.is_some() {
        return direct;
    }
    if name == "litellm" {
        return cfg.get(&Value::Str("timeout".to_string()));
    }
    None
}

/// `int(raw)`, CPython semantics: `int(4.9) == 4`, `int("4.9")` fails,
/// `int(True) == 1`. A failure, `0` or a negative value counts as
/// unset (callers treat `None` as "fall through to the next
/// precedence tier", and a stray `0` must do the same rather than
/// being read as "no limit").
fn py_int_timeout(value: &Value) -> Option<i64> {
    let parsed = electricity_value::pycompat::py_int(value).ok()?;
    if parsed > 0 { Some(parsed) } else { None }
}

/// `adapters/factory.py::configured_timeout_seconds`. *adapter_name* is
/// normalised the same way `build_adapter` normalises it
/// (`.strip().lower()`).
pub fn configured_timeout_seconds(adapter_name: &str, runtime: Option<&Value>) -> Option<i64> {
    let name = electricity_value::pycompat::py_strip(adapter_name).to_lowercase();
    let cfg = adapter_config_slice(runtime, &name)?;
    let raw = raw_timeout_value(cfg, &name)?;
    py_int_timeout(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    fn runtime_with_adapter_timeout(name: &str, key: &str, value: Value) -> Value {
        let mut inner = Dict::new();
        inner.insert(Value::Str(key.to_string()), value);
        let mut adapters = Dict::new();
        adapters.insert(Value::Str(name.to_string()), Value::Dict(inner));
        let mut runtime = Dict::new();
        runtime.insert(Value::Str("adapters".to_string()), Value::Dict(adapters));
        Value::Dict(runtime)
    }

    #[test]
    fn reads_an_integer_timeout() {
        let runtime = runtime_with_adapter_timeout("ollama", "timeout_seconds", Value::from(30i64));
        assert_eq!(
            configured_timeout_seconds("ollama", Some(&runtime)),
            Some(30)
        );
    }

    #[test]
    fn a_float_truncates_toward_zero() {
        let runtime = runtime_with_adapter_timeout("ollama", "timeout_seconds", Value::from(4.9));
        assert_eq!(
            configured_timeout_seconds("ollama", Some(&runtime)),
            Some(4)
        );
    }

    #[test]
    fn a_non_numeric_string_counts_as_unset() {
        let runtime = runtime_with_adapter_timeout("ollama", "timeout_seconds", Value::from("4.9"));
        assert_eq!(configured_timeout_seconds("ollama", Some(&runtime)), None);
    }

    #[test]
    fn a_numeric_string_parses() {
        let runtime = runtime_with_adapter_timeout("ollama", "timeout_seconds", Value::from("42"));
        assert_eq!(
            configured_timeout_seconds("ollama", Some(&runtime)),
            Some(42)
        );
    }

    #[test]
    fn true_is_one() {
        let runtime = runtime_with_adapter_timeout("ollama", "timeout_seconds", Value::Bool(true));
        assert_eq!(
            configured_timeout_seconds("ollama", Some(&runtime)),
            Some(1)
        );
    }

    #[test]
    fn zero_counts_as_unset() {
        let runtime = runtime_with_adapter_timeout("ollama", "timeout_seconds", Value::from(0i64));
        assert_eq!(configured_timeout_seconds("ollama", Some(&runtime)), None);
    }

    #[test]
    fn a_negative_value_counts_as_unset() {
        let runtime = runtime_with_adapter_timeout("ollama", "timeout_seconds", Value::from(-5i64));
        assert_eq!(configured_timeout_seconds("ollama", Some(&runtime)), None);
    }

    #[test]
    fn a_missing_block_is_none() {
        assert_eq!(configured_timeout_seconds("ollama", None), None);
    }

    #[test]
    fn the_name_is_normalized_before_lookup() {
        let runtime = runtime_with_adapter_timeout("ollama", "timeout_seconds", Value::from(30i64));
        assert_eq!(
            configured_timeout_seconds(" Ollama ", Some(&runtime)),
            Some(30)
        );
    }

    #[test]
    fn litellm_falls_back_to_the_deprecated_timeout_alias() {
        let runtime = runtime_with_adapter_timeout("litellm", "timeout", Value::from(60i64));
        assert_eq!(
            configured_timeout_seconds("litellm", Some(&runtime)),
            Some(60)
        );
    }

    #[test]
    fn a_different_adapter_never_uses_the_timeout_alias() {
        let runtime = runtime_with_adapter_timeout("ollama", "timeout", Value::from(60i64));
        assert_eq!(configured_timeout_seconds("ollama", Some(&runtime)), None);
    }
}
