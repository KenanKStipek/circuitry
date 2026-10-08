//! Lane C: `_validate_name` (`core/compiler.py`) -- an effect's `name:`
//! field: must be a string, non-empty, no leading/trailing or internal
//! whitespace, no `.`, not the reserved `iter_<n>` loop-pass pattern,
//! not a structural-slot name the runtime itself writes (`value`,
//! `meta`, `input`, `prime`, `runtime`), and matching
//! `^[A-Za-z_][A-Za-z0-9_]*$`.

use crate::CompileError;
use electricity_value::Value;

/// Collides with structural slots the runtime itself writes onto an
/// effect's own node (`value`, `meta`) or merges into the context
/// (`input`, `prime`, `runtime`) -- issue #260 part 3.
const RESERVED_EFFECT_NAMES: [&str; 5] = ["value", "meta", "input", "prime", "runtime"];

fn is_valid_name_pattern(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn is_iter_pass_pattern(name: &str) -> bool {
    match name.strip_prefix("iter_") {
        Some(rest) => !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

/// Ports `core/compiler.py::_validate_name`. *scope_path* is accepted
/// but unused, matching the Python reference (kept only so callers can
/// report deterministic addressing context -- it's never read inside
/// the function itself).
pub(crate) fn validate_name(
    name: &Value,
    effect_type: &str,
    _scope_path: &str,
    effect_path: &str,
) -> Result<String, CompileError> {
    let Value::Str(name) = name else {
        return Err(CompileError(format!(
            "{effect_type} effect at '{effect_path}' must define a string 'name'."
        )));
    };

    if name.trim().is_empty() {
        return Err(CompileError(format!(
            "{effect_type} effect at '{effect_path}' has an empty/whitespace-only name."
        )));
    }
    if name != name.trim() {
        return Err(CompileError(format!(
            "Invalid name '{name}' for {effect_type} at '{effect_path}': \
             leading/trailing whitespace is not allowed."
        )));
    }
    if name.contains('.') {
        return Err(CompileError(format!(
            "Invalid name '{name}' for {effect_type} at '{effect_path}': \
             '.' is not allowed in names."
        )));
    }
    if name.chars().any(char::is_whitespace) {
        return Err(CompileError(format!(
            "Invalid name '{name}' for {effect_type} at '{effect_path}': \
             whitespace is not allowed in names."
        )));
    }
    if is_iter_pass_pattern(name) {
        return Err(CompileError(format!(
            "Invalid name '{name}' for {effect_type} at '{effect_path}': \
             reserved loop iteration segment pattern 'iter_<n>' is not allowed."
        )));
    }
    if RESERVED_EFFECT_NAMES.contains(&name.as_str()) {
        return Err(CompileError(format!(
            "Invalid name '{name}' for {effect_type} at '{effect_path}': \
             '{name}' is reserved — it collides with a structural slot the \
             runtime itself writes ('value', 'meta', 'input', 'prime', \
             'runtime'). Choose a different name."
        )));
    }
    if !is_valid_name_pattern(name) {
        return Err(CompileError(format!(
            "Invalid name '{name}' for {effect_type} at '{effect_path}': \
             expected pattern [A-Za-z_][A-Za-z0-9_]*."
        )));
    }
    Ok(name.clone())
}

#[cfg(test)]
mod tests {
    use super::validate_name;
    use electricity_value::Value;

    fn check(name: Value) -> Result<String, String> {
        validate_name(&name, "tool", "prime", "prime.effects[0]").map_err(|e| e.0)
    }

    #[test]
    fn accepts_a_plain_name() {
        assert_eq!(
            check(Value::Str("fetch".to_string())),
            Ok("fetch".to_string())
        );
    }

    #[test]
    fn rejects_non_string() {
        assert_eq!(
            check(Value::Int(1.into())),
            Err("tool effect at 'prime.effects[0]' must define a string 'name'.".to_string())
        );
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(
            check(Value::Str("   ".to_string())),
            Err("tool effect at 'prime.effects[0]' has an empty/whitespace-only name.".to_string())
        );
    }

    #[test]
    fn rejects_leading_whitespace() {
        let err = check(Value::Str(" fetch".to_string())).unwrap_err();
        assert!(err.contains("leading/trailing whitespace is not allowed"));
    }

    #[test]
    fn rejects_dot() {
        let err = check(Value::Str("a.b".to_string())).unwrap_err();
        assert!(err.contains("'.' is not allowed in names"));
    }

    #[test]
    fn rejects_internal_whitespace() {
        let err = check(Value::Str("a b".to_string())).unwrap_err();
        assert!(err.contains("whitespace is not allowed in names"));
    }

    #[test]
    fn rejects_iter_pattern() {
        let err = check(Value::Str("iter_3".to_string())).unwrap_err();
        assert!(err.contains("reserved loop iteration segment pattern"));
    }

    #[test]
    fn allows_iter_without_trailing_digits() {
        // "iter_" alone (no digits after it) is not the reserved pattern.
        assert_eq!(
            check(Value::Str("iter_".to_string())),
            Ok("iter_".to_string())
        );
    }

    #[test]
    fn rejects_reserved_name() {
        for reserved in ["value", "meta", "input", "prime", "runtime"] {
            let err = check(Value::Str(reserved.to_string())).unwrap_err();
            assert!(err.contains("is reserved"));
        }
    }

    #[test]
    fn rejects_bad_pattern() {
        let err = check(Value::Str("1abc".to_string())).unwrap_err();
        assert!(err.contains("expected pattern"));
    }
}
