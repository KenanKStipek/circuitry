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

/// `cfg.get(key) or default`, kept as the raw post-`or` [`Value`] --
/// an absent/falsy config value becomes `Value::Str(default)`, but a
/// truthy non-string value is kept exactly as configured, never
/// coerced to a string the way [`cfg_str_or`] would. Two independent
/// uses share this: `binary`/`default_model`/`thinking` (issue #520
/// part B, B5) resolve -- or raise Python's own `TypeError` -- only
/// where actually used ([`require_str`]/[`resolve_truthy_str`]);
/// `base_url`/`endpoint` (issue #515, review finding F5) resolve via
/// [`rstrip_base_url`] at each adapter's own later Python call site,
/// since `self.base_url.rstrip("/")` there raises `AttributeError`
/// lazily, not at build time (a watsonx IAM exchange, say, must still
/// run before that attribute error would).
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

/// `adapters/factory.py::_env_pairs` -- `cfg["env"]` as `(name, value)`
/// pairs for a CLI child, empty when absent or explicitly `null`. A
/// leading `~/` in a value becomes the home directory (`os.path.
/// expanduser`), nothing else is expanded. An error names the variable,
/// never its value, and carries Python's own text.
pub fn env_pairs(cfg: &Value, adapter_name: &str) -> Result<EnvPairs, AdapterError> {
    let value_error = |message: String| AdapterError {
        message,
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::ValueError,
    };
    let raw = match cfg_get(cfg, "env") {
        None | Some(Value::None) => return Ok(EnvPairs::default()),
        Some(Value::Dict(dict)) => dict,
        Some(other) => {
            return Err(value_error(format!(
                "runtime.adapters.{adapter_name}.env must be a mapping of variable names \
                 to strings, got {}",
                other.type_name()
            )));
        }
    };
    let mut pairs = Vec::with_capacity(raw.len());
    for (key, value) in raw.iter() {
        let name = match key {
            Value::Str(name) if is_env_name(name) => name,
            other => {
                return Err(value_error(format!(
                    "runtime.adapters.{adapter_name}.env has an invalid variable name {}: \
                     a name matches [A-Za-z_][A-Za-z0-9_]*",
                    other.py_repr()
                )));
            }
        };
        let Value::Str(text) = value else {
            return Err(value_error(format!(
                "runtime.adapters.{adapter_name}.env.{name} must be a string, got {}",
                value.type_name()
            )));
        };
        pairs.push((name.clone(), expand_leading_home(text)));
    }
    Ok(EnvPairs(pairs))
}

/// An alias's `env`, in config order: `(name, value)` pairs for a CLI
/// child. Its `Debug` shows the variable names only -- a value can be
/// anything the CLI reads, and never belongs in a log.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct EnvPairs(Vec<(String, String)>);

impl EnvPairs {
    pub fn as_slice(&self) -> &[(String, String)] {
        &self.0
    }
}

impl std::fmt::Debug for EnvPairs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|(name, _)| name))
            .finish()
    }
}

/// `[A-Za-z_][A-Za-z0-9_]*`, the whole string (`re.fullmatch`).
fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `os.path.expanduser(value)` for a value starting `~/`: the home
/// directory (`$HOME`, else the password database -- `std::env::
/// home_dir`'s own order, the same one Python's `expanduser` follows)
/// without its trailing slashes, then the rest. Any other value, or no
/// home directory at all, is returned unchanged.
fn expand_leading_home(value: &str) -> String {
    if !value.starts_with("~/") {
        return value.to_string();
    }
    match std::env::home_dir() {
        Some(home) => format!(
            "{}{}",
            home.to_string_lossy().trim_end_matches('/'),
            &value[1..]
        ),
        None => value.to_string(),
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

    fn env_cfg(env: Value) -> Value {
        dict(&[("env", env)])
    }

    #[test]
    fn env_pairs_is_empty_when_absent_or_null() {
        assert_eq!(env_pairs(&Value::None, "a").unwrap(), EnvPairs::default());
        assert_eq!(
            env_pairs(&env_cfg(Value::None), "a").unwrap(),
            EnvPairs::default()
        );
        assert_eq!(
            env_pairs(&env_cfg(dict(&[])), "a").unwrap(),
            EnvPairs::default()
        );
    }

    #[test]
    fn env_pairs_keeps_config_order_and_expands_only_a_leading_tilde_slash() {
        let home = std::env::home_dir().expect("a home directory");
        let home = home.to_string_lossy().trim_end_matches('/').to_string();
        let cfg = env_cfg(dict(&[
            ("B", Value::Str("~/x".to_string())),
            ("A", Value::Str("~".to_string())),
            ("C", Value::Str("a/~/b".to_string())),
            ("D", Value::Str("~other/x".to_string())),
            ("E", Value::Str("$HOME/x".to_string())),
        ]));
        let pairs = env_pairs(&cfg, "a").unwrap();
        let expected: Vec<(String, String)> = vec![
            ("B".to_string(), format!("{home}/x")),
            ("A".to_string(), "~".to_string()),
            ("C".to_string(), "a/~/b".to_string()),
            ("D".to_string(), "~other/x".to_string()),
            ("E".to_string(), "$HOME/x".to_string()),
        ];
        assert_eq!(pairs.as_slice(), expected.as_slice());
    }

    #[test]
    fn env_pairs_rejects_a_non_mapping_without_showing_it() {
        let err = env_pairs(&env_cfg(Value::Str("A=secret".to_string())), "alt").unwrap_err();
        assert_eq!(
            err.message,
            "runtime.adapters.alt.env must be a mapping of variable names to strings, got str"
        );
        assert_eq!(err.py_class, PyExc::ValueError);
        assert!(!err.retry.retryable);
    }

    #[test]
    fn env_pairs_rejects_a_non_string_value_naming_the_variable_only() {
        let err = env_pairs(&env_cfg(dict(&[("A", Value::from(5i64))])), "alt").unwrap_err();
        assert_eq!(
            err.message,
            "runtime.adapters.alt.env.A must be a string, got int"
        );
    }

    #[test]
    fn env_pairs_rejects_an_invalid_variable_name() {
        for name in ["", "1A", "A-B", "A B", "A\n", "\u{e9}"] {
            let cfg = env_cfg(dict(&[(name, Value::Str("secret".to_string()))]));
            let err = env_pairs(&cfg, "alt").unwrap_err();
            assert_eq!(
                err.message,
                format!(
                    "runtime.adapters.alt.env has an invalid variable name {}: \
                     a name matches [A-Za-z_][A-Za-z0-9_]*",
                    Value::Str(name.to_string()).py_repr()
                )
            );
            assert!(!err.message.contains("secret"));
        }
        let ok = env_cfg(dict(&[("_a1", Value::Str("x".to_string()))]));
        assert!(env_pairs(&ok, "alt").is_ok());
    }

    #[test]
    fn env_pairs_debug_shows_names_never_values() {
        let cfg = env_cfg(dict(&[(
            "TOKEN_DIR",
            Value::Str("marker-value".to_string()),
        )]));
        let shown = format!("{:?}", env_pairs(&cfg, "a").unwrap());
        assert!(shown.contains("TOKEN_DIR") && !shown.contains("marker-value"));
    }
}
