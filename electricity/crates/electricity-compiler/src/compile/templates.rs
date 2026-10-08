//! Lane C: `_check_templates` (`core/compiler.py`) -- walks a raw
//! document value recursively, rejecting the first malformed Mustache
//! template among its string leaves, and builds the matching
//! [`TemplateText`] for a checked scalar field.

use crate::CompileError;
use electricity_bytecode::{Escape, TemplateText};
use electricity_template::{SyntaxCheck, template_syntax_error_with};
use electricity_value::Value;

/// Ports `core/compiler.py::_check_templates`: every string leaf nested
/// anywhere in *value* (through dicts/lists) must be a well-formed
/// Mustache template; a non-string leaf is not a template and is
/// skipped. *allow_partials* lets a well-formed `{{> name}}` pass this
/// syntax gate (set only by a caller checking one of the composable
/// fields runtime-semantics §3.4 lists) -- whether the name itself
/// resolves is `compose::check_prompt_composition`'s separate, later
/// check (lane D).
pub(crate) fn check_templates(
    value: &Value,
    effect_path: &str,
    field: &str,
    allow_partials: bool,
) -> Result<(), CompileError> {
    match value {
        Value::Str(s) => {
            let check = SyntaxCheck { allow_partials };
            if let Some(reason) = template_syntax_error_with(s, check) {
                Err(CompileError(format!(
                    "{effect_path}.{field}: malformed Mustache template: {reason}"
                )))
            } else {
                Ok(())
            }
        }
        Value::Dict(dict) => {
            for (key, item) in dict {
                let key_str = match key {
                    Value::Str(s) => s.clone(),
                    other => other.py_str(),
                };
                check_templates(
                    item,
                    effect_path,
                    &format!("{field}.{key_str}"),
                    allow_partials,
                )?;
            }
            Ok(())
        }
        Value::List(items) => {
            for (index, item) in items.iter().enumerate() {
                check_templates(
                    item,
                    effect_path,
                    &format!("{field}[{index}]"),
                    allow_partials,
                )?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Checks *raw* as a single template field (not recursively -- for a
/// scalar field like `template`/`prompt`/`inline`, never a `params`-
/// shaped mapping) and wraps it as a [`TemplateText`] if it passes.
/// *composable* is both the `allow_partials` flag [`check_templates`]
/// is called with and the resulting [`TemplateText::composable`] value
/// -- they are the same flag by construction (runtime-semantics §3.4).
pub(crate) fn template_text(
    raw: &str,
    effect_path: &str,
    field: &str,
    composable: bool,
    escape: Escape,
) -> Result<TemplateText, CompileError> {
    check_templates(&Value::Str(raw.to_string()), effect_path, field, composable)?;
    Ok(TemplateText::new(raw, composable, escape))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_well_formed_template() {
        assert!(
            check_templates(
                &Value::Str("hi {{name}}".to_string()),
                "p",
                "template",
                false
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_a_malformed_template() {
        let err =
            check_templates(&Value::Str("{{#a}}".to_string()), "p", "template", false).unwrap_err();
        assert!(
            err.0
                .starts_with("p.template: malformed Mustache template:")
        );
    }

    #[test]
    fn rejects_a_partial_unless_allowed() {
        assert!(
            check_templates(&Value::Str("{{> p}}".to_string()), "p", "template", false).is_err()
        );
        assert!(check_templates(&Value::Str("{{> p}}".to_string()), "p", "template", true).is_ok());
    }

    #[test]
    fn recurses_into_nested_structures() {
        let mut inner = electricity_value::Dict::new();
        inner.insert(
            Value::Str("x".to_string()),
            Value::List(vec![Value::Str("{{#bad}}".to_string())]),
        );
        let err = check_templates(&Value::Dict(inner), "p", "params", false).unwrap_err();
        assert!(err.0.starts_with("p.params.x[0]: malformed"));
    }

    #[test]
    fn builds_template_text_on_success() {
        let text = template_text("hi {{name}}", "p", "template", true, Escape::None).unwrap();
        assert_eq!(text.source, "hi {{name}}");
        assert!(text.composable);
        assert_eq!(text.escape, Escape::None);
    }
}
