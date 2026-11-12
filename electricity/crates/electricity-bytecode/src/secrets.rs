//! `secrets.py`'s own data-shape half, ported: whether a `Value` reads
//! as a `{secret: <name>, prefix?: <text>}` reference, and why one
//! doesn't (issue #598). The resolution half (the Keychain, a command,
//! an environment variable) is not this crate's concern -- a later
//! lane's own crate resolves a well-formed [`SecretRef`] into a value;
//! this module only recognizes the shape, the same way this crate's
//! own `ParamNode::From` recognizes a `{from: ...}` leaf's shape
//! without resolving it either.

use electricity_value::Value;

pub const SECRET_KEY: &str = "secret";
pub const PREFIX_KEY: &str = "prefix";

/// `secrets.py::_NAME_RE` (`^[A-Za-z_][A-Za-z0-9_-]*$`), checked by hand
/// rather than pulling `regex` into this crate for one fixed pattern.
fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// `secrets.py::_FORBIDDEN_CHARS`.
fn has_forbidden_char(text: &str) -> bool {
    text.contains('\r') || text.contains('\n') || text.contains('\0')
}

/// A document's `{secret: <name>, prefix?: <text>}`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SecretRef {
    pub name: String,
    pub prefix: String,
}

impl SecretRef {
    /// What every record shows in place of `prefix` plus the value.
    pub fn placeholder(&self) -> String {
        format!("{}[secret:{}]", self.prefix, self.name)
    }
}

/// `secrets.py::has_secret_key` -- whether *value* is a mapping with a
/// `secret` key, well-formed or not.
pub fn has_secret_key(value: &Value) -> bool {
    matches!(value, Value::Dict(dict) if dict.contains_key(&Value::Str(SECRET_KEY.to_string())))
}

/// `secrets.py::secret_ref_error` -- why *value* is not a well-formed
/// reference, or `None` when it is one.
pub fn secret_ref_error(value: &Value) -> Option<String> {
    let Value::Dict(dict) = value else {
        return Some("must be a mapping".to_string());
    };
    let mut extra: Vec<String> = dict
        .keys()
        .filter(|key| {
            !matches!(key, Value::Str(s) if s == SECRET_KEY || s == PREFIX_KEY)
        })
        .map(Value::py_repr)
        .collect();
    extra.sort();
    if !extra.is_empty() {
        return Some(format!(
            "has unknown key(s) {}; a secret reference has 'secret' and an optional 'prefix'",
            extra.join(", ")
        ));
    }
    let name = dict.get(&Value::Str(SECRET_KEY.to_string()));
    let name_str = match name {
        Some(Value::Str(s)) if is_valid_name(s) => Some(s),
        _ => None,
    };
    if name_str.is_none() {
        let repr = name.cloned().unwrap_or(Value::None).py_repr();
        return Some(format!(
            "'secret' must name a runtime.secrets entry (letters, digits, '_' and '-', \
             not starting with a digit or '-'), got {repr}"
        ));
    }
    match dict.get(&Value::Str(PREFIX_KEY.to_string())) {
        None | Some(Value::None) => {}
        Some(Value::Str(prefix)) => {
            if has_forbidden_char(prefix) {
                return Some("'prefix' must not contain a line break or a NUL".to_string());
            }
        }
        Some(other) => {
            return Some(format!(
                "'prefix' must be a string, got {}",
                other.type_name()
            ));
        }
    }
    None
}

/// `secrets.py::is_secret_ref` -- whether *value* reads as a reference:
/// a mapping whose keys are `secret` plus, optionally, `prefix`, with a
/// string `secret`. Any other mapping with a `secret` key (a `json` body,
/// a schema with a `secret` property) is literal data.
pub fn is_secret_ref(value: &Value) -> bool {
    let Value::Dict(dict) = value else {
        return false;
    };
    let keys_ok = dict.keys().all(|key| {
        matches!(key, Value::Str(s) if s == SECRET_KEY || s == PREFIX_KEY)
    });
    keys_ok && matches!(dict.get(&Value::Str(SECRET_KEY.to_string())), Some(Value::Str(_)))
}

/// `secrets.py::secret_ref` -- *value* as a [`SecretRef`], or `None`
/// when it is not a well-formed reference.
pub fn secret_ref(value: &Value) -> Option<SecretRef> {
    if !has_secret_key(value) || secret_ref_error(value).is_some() {
        return None;
    }
    let Value::Dict(dict) = value else {
        return None;
    };
    let name = match dict.get(&Value::Str(SECRET_KEY.to_string())) {
        Some(Value::Str(s)) => s.clone(),
        _ => return None,
    };
    let prefix = match dict.get(&Value::Str(PREFIX_KEY.to_string())) {
        Some(Value::Str(s)) => s.clone(),
        _ => String::new(),
    };
    Some(SecretRef { name, prefix })
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    fn dict_of(pairs: &[(&str, Value)]) -> Value {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v.clone());
        }
        Value::Dict(d)
    }

    #[test]
    fn a_well_formed_reference_parses() {
        let value = dict_of(&[
            ("secret", Value::from("github")),
            ("prefix", Value::from("Bearer ")),
        ]);
        assert!(has_secret_key(&value));
        assert!(is_secret_ref(&value));
        assert_eq!(secret_ref_error(&value), None);
        assert_eq!(
            secret_ref(&value),
            Some(SecretRef {
                name: "github".to_string(),
                prefix: "Bearer ".to_string()
            })
        );
    }

    #[test]
    fn a_reference_with_no_prefix_defaults_to_empty() {
        let value = dict_of(&[("secret", Value::from("api"))]);
        assert_eq!(
            secret_ref(&value),
            Some(SecretRef {
                name: "api".to_string(),
                prefix: String::new()
            })
        );
        assert_eq!(secret_ref(&value).unwrap().placeholder(), "[secret:api]");
    }

    #[test]
    fn an_unknown_key_is_malformed() {
        let value = dict_of(&[("secret", Value::from("api")), ("extra", Value::from("x"))]);
        assert!(has_secret_key(&value));
        assert!(!is_secret_ref(&value));
        assert!(secret_ref_error(&value).unwrap().contains("unknown key"));
        assert_eq!(secret_ref(&value), None);
    }

    #[test]
    fn a_non_string_name_is_malformed() {
        let value = dict_of(&[("secret", Value::from(5_i64))]);
        assert!(secret_ref_error(&value).unwrap().contains("must name a runtime.secrets"));
    }

    #[test]
    fn a_name_starting_with_a_digit_or_dash_is_malformed() {
        for name in ["1abc", "-abc"] {
            let value = dict_of(&[("secret", Value::from(name))]);
            assert!(secret_ref_error(&value).is_some(), "{name} should be malformed");
        }
    }

    #[test]
    fn a_prefix_with_a_line_break_is_malformed() {
        let value = dict_of(&[("secret", Value::from("api")), ("prefix", Value::from("a\nb"))]);
        assert!(secret_ref_error(&value).unwrap().contains("line break"));
    }

    #[test]
    fn a_mapping_with_no_secret_key_has_no_secret_key() {
        let value = dict_of(&[("other", Value::from("x"))]);
        assert!(!has_secret_key(&value));
        assert_eq!(secret_ref(&value), None);
    }

    #[test]
    fn a_non_mapping_is_not_a_reference_at_all() {
        let value = Value::from("secret");
        assert!(!has_secret_key(&value));
        assert!(!is_secret_ref(&value));
        assert_eq!(secret_ref(&value), None);
    }
}
