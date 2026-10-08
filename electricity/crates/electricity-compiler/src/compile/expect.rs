//! Lane C: `_compile_retries`/`_compile_expect` (`core/compiler.py`) --
//! shared by `tool`/`use` effects.

use crate::CompileError;
use crate::compile::coerce::{get_truthy, py_int};
use crate::compile::templates::template_text;
use crate::state_ns::{validate_cel_expr, validate_cel_syntax};
use electricity_bytecode::{Escape, ExpectCondition, RetryPolicy};
use electricity_value::{Dict, Value};
use std::collections::BTreeSet;

/// `retries:` on a `tool`/`use` effect -- same shape/defaults as a
/// prompt's (#273). Ports `core/compiler.py::_compile_retries`.
pub(crate) fn compile_retries(effect: &Dict) -> Option<RetryPolicy> {
    let Some(Value::Dict(retries_raw)) = effect.get(&Value::Str("retries".to_string())) else {
        return None;
    };
    let max_attempts = get_truthy(retries_raw, "max_attempts")
        .map(py_int)
        .unwrap_or(1)
        .max(0) as u32;
    let backoff_ms = get_truthy(retries_raw, "backoff_ms")
        .map(py_int)
        .unwrap_or(1000)
        .max(0) as u32;
    Some(RetryPolicy {
        max_attempts,
        backoff_ms,
    })
}

/// `expect:` on a `tool`/`use` effect (#273). Ports
/// `core/compiler.py::_compile_expect`.
pub(crate) fn compile_expect(
    effect: &Dict,
    effect_type: &str,
    effect_path: &str,
    loop_names: &BTreeSet<String>,
) -> Result<Option<ExpectCondition>, CompileError> {
    let Some(raw) = effect.get(&Value::Str("expect".to_string())) else {
        return Ok(None);
    };
    if raw.is_none() {
        return Ok(None);
    }
    match raw {
        Value::Str(s) => {
            let expr = s.trim();
            if expr.is_empty() {
                return Err(CompileError(format!(
                    "{effect_type} effect at '{effect_path}': 'expect' must not be \
                     an empty string."
                )));
            }
            validate_cel_expr(expr, effect_path, loop_names)?;
            validate_cel_syntax(expr, effect_path, None, "expect CEL expression")?;
            Ok(Some(ExpectCondition::Cel {
                expr: expr.to_string(),
            }))
        }
        Value::Dict(raw) => {
            let mode_raw = get_truthy(raw, "mode")
                .map(Value::py_str)
                .unwrap_or_else(|| "cel".to_string());
            let mode = mode_raw.trim().to_lowercase();
            if mode == "model" {
                // Python strips only to test emptiness; the stored
                // `template` is the raw (untrimmed) value, so a block
                // scalar's trailing newline survives.
                let template_raw = raw
                    .get(&Value::Str("template".to_string()))
                    .and_then(Value::as_str);
                let is_non_empty = template_raw.is_some_and(|s| !s.trim().is_empty());
                let Some(template_raw) = template_raw.filter(|_| is_non_empty) else {
                    return Err(CompileError(format!(
                        "{effect_type} effect at '{effect_path}': expect mode 'model' \
                         requires a non-empty 'template' field."
                    )));
                };
                let text = template_text(
                    template_raw,
                    effect_path,
                    "expect.template",
                    false,
                    Escape::Html,
                )?;
                return Ok(Some(ExpectCondition::Model { template: text }));
            }
            let expr = raw
                .get(&Value::Str("expr".to_string()))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let Some(expr) = expr else {
                return Err(CompileError(format!(
                    "{effect_type} effect at '{effect_path}': expect mode 'cel' \
                     requires a non-empty 'expr' field."
                )));
            };
            validate_cel_expr(expr, effect_path, loop_names)?;
            validate_cel_syntax(expr, effect_path, None, "expect CEL expression")?;
            Ok(Some(ExpectCondition::Cel {
                expr: expr.to_string(),
            }))
        }
        _ => Err(CompileError(format!(
            "{effect_type} effect at '{effect_path}': 'expect' must be a CEL string \
             or a mapping with 'mode'."
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(pairs: Vec<(&str, Value)>) -> Dict {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        d
    }

    #[test]
    fn no_retries_is_none() {
        assert_eq!(compile_retries(&Dict::new()), None);
    }

    #[test]
    fn retries_applies_defaults() {
        let effect = dict(vec![("retries", Value::Dict(Dict::new()))]);
        assert_eq!(
            compile_retries(&effect),
            Some(RetryPolicy {
                max_attempts: 1,
                backoff_ms: 1000
            })
        );
    }

    #[test]
    fn retries_honors_explicit_values() {
        let retries = dict(vec![
            ("max_attempts", Value::Int(3.into())),
            ("backoff_ms", Value::Int(500.into())),
        ]);
        let effect = dict(vec![("retries", Value::Dict(retries))]);
        assert_eq!(
            compile_retries(&effect),
            Some(RetryPolicy {
                max_attempts: 3,
                backoff_ms: 500
            })
        );
    }

    #[test]
    fn no_expect_is_none() {
        assert!(
            compile_expect(&Dict::new(), "tool", "p", &BTreeSet::new())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn bare_string_expect_is_cel() {
        let effect = dict(vec![("expect", Value::Str("value.ok == true".to_string()))]);
        let expect = compile_expect(&effect, "tool", "p", &BTreeSet::new()).unwrap();
        assert!(matches!(expect, Some(ExpectCondition::Cel { .. })));
    }

    #[test]
    fn empty_string_expect_rejected() {
        let effect = dict(vec![("expect", Value::Str("  ".to_string()))]);
        let err = compile_expect(&effect, "tool", "p", &BTreeSet::new()).unwrap_err();
        assert!(err.0.contains("must not be an empty string"));
    }

    #[test]
    fn model_mode_requires_template() {
        let mode_dict = dict(vec![("mode", Value::Str("model".to_string()))]);
        let effect = dict(vec![("expect", Value::Dict(mode_dict))]);
        let err = compile_expect(&effect, "tool", "p", &BTreeSet::new()).unwrap_err();
        assert!(err.0.contains("requires a non-empty 'template' field"));
    }

    #[test]
    fn model_mode_builds_template() {
        let mode_dict = dict(vec![
            ("mode", Value::Str("model".to_string())),
            ("template", Value::Str("ok?".to_string())),
        ]);
        let effect = dict(vec![("expect", Value::Dict(mode_dict))]);
        let expect = compile_expect(&effect, "tool", "p", &BTreeSet::new()).unwrap();
        assert!(matches!(expect, Some(ExpectCondition::Model { .. })));
    }

    #[test]
    fn invalid_shape_rejected() {
        let effect = dict(vec![("expect", Value::Int(1.into()))]);
        let err = compile_expect(&effect, "tool", "p", &BTreeSet::new()).unwrap_err();
        assert!(err.0.contains("must be a CEL string or a mapping"));
    }
}
