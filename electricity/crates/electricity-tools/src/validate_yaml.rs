//! `plugins/validate_yaml.py::ValidateYamlPlugin`, ported in full
//! (issue #521, lane L2a of #448). Made a native tool by #448's
//! decision 3, because decomposition's own bundled planner
//! (`electricity-vm::decompose::PLANNER_YAML`) calls it twice -- every
//! other plugin in Circuitry's registry stays refused up front until
//! the M2 plugin bridge.
//!
//! Validating is not the refusal walker (#448's Settled question 3): a
//! draft naming content electricity can't run yet (an M2 provider, an
//! unbuildable adapter) validates `ok: true`, same as `cof check`.
//! Only *running* it refuses. The compile step uses
//! [`electricity_compiler::DocumentOrigin::Generated`] (Settled
//! question 2): Python passes no `document_dir`, so a `{file: ...}`
//! prompt source is a compile error here too.

use crate::fs::{as_bool, get, value_error};
use crate::{CheckResult, PyExc, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_compiler::structural::py_class_name;
use electricity_compiler::{DocumentOrigin, compile_document, structural_errors};
use electricity_value::pycompat::py_int;
use electricity_value::{Dict, Value};

const DEFAULT_MAX_ERRORS: i64 = 20;

/// `plugins/validate_yaml.py::_validate_document`: the first branch
/// that applies returns its own error list; an empty list means the
/// document (as cleaned, in *document*) validated.
///
/// A calendar-invalid timestamp (`x: 2024-02-30`) is the one way this
/// can raise rather than return: `core/yaml_load.py::load_yaml` lets
/// CPython's own `ValueError` (`day is out of range for month`, ...)
/// escape uncaught, since it is never a `yaml.YAMLError` that
/// `_validate_document`'s `except yaml.YAMLError` could catch
/// (review finding 5 on PR #524).
fn validate_document(document: &str, run_compile: bool) -> Result<Vec<String>, ToolError> {
    if electricity_value::pycompat::py_strip(document).is_empty() {
        return Ok(vec![
            "Empty document \u{2014} expected a Circuitry orchestration YAML.".to_string(),
        ]);
    }

    let parsed = match electricity_yaml::load_yaml(document) {
        Ok(value) => value,
        Err(electricity_yaml::YamlError::InvalidTimestampValue { message, .. }) => {
            return Err(value_error(message));
        }
        Err(err) => return Ok(vec![format!("YAML parse error: {err}")]),
    };

    let Some(dict) = parsed.as_dict() else {
        return Ok(vec![format!(
            "Orchestration must be a YAML mapping with an 'effects' key, got {}.",
            py_class_name(&parsed)
        )]);
    };
    if !dict.contains_key(&Value::Str("effects".to_string())) {
        return Ok(vec![
            "Orchestration is missing the required top-level 'effects' key.".to_string(),
        ]);
    }

    let errors = structural_errors(&parsed);
    if !errors.is_empty() {
        return Ok(errors);
    }

    if run_compile {
        if let Err(err) = compile_document(&parsed, &DocumentOrigin::Generated) {
            return Ok(vec![format!("Compile error: {err}")]);
        }
    }

    Ok(Vec::new())
}

pub struct ValidateYamlPlugin;

#[async_trait(?Send)]
impl ToolPlugin for ValidateYamlPlugin {
    fn name(&self) -> &str {
        "validate_yaml"
    }

    async fn execute(&self, params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        // 1. `params['yaml']`: absent/`null` reads as `""`; any other
        // non-string raises.
        let mut document = match get(&params, "yaml") {
            None | Some(Value::None) => String::new(),
            Some(Value::Str(s)) => s.clone(),
            Some(other) => {
                return Err(value_error(format!(
                    "ValidateYamlPlugin requires params['yaml'] as a string (got {}).",
                    py_class_name(other)
                )));
            }
        };

        // 2. `strip_fences` (default true): clean the document first --
        // every later step (the emptiness check, the parse, max_errors'
        // cap) sees the cleaned text, and `yaml` echoes it back.
        if as_bool(get(&params, "strip_fences"), true) {
            document = electricity_yaml::clean_yaml_fences(&document);
        }

        // 3. `max_errors` (default 20), through CPython `int()`.
        let limit = match get(&params, "max_errors") {
            None | Some(Value::None) => DEFAULT_MAX_ERRORS,
            Some(other) => match py_int(other) {
                Ok(n) => n,
                // `int(float("inf"))`/`int(float("nan"))` raise
                // `OverflowError`/`ValueError` CPython never catches here
                // (`except (TypeError, ValueError)` doesn't name
                // `OverflowError`) -- it escapes bare, unwrapped, with no
                // "must be an integer:" prefix (review finding 4 on PR
                // #524).
                Err(err) if err.py_class == PyExc::OverflowError => {
                    return Err(ToolError {
                        py_class: PyExc::OverflowError,
                        message: err.message,
                        cause: crate::ErrorCause::None,
                    });
                }
                Err(err) => {
                    return Err(value_error(format!(
                        "ValidateYamlPlugin params['max_errors'] must be an integer: {}",
                        err.message
                    )));
                }
            },
        };
        if limit < 1 {
            return Err(value_error(
                "ValidateYamlPlugin params['max_errors'] must be >= 1.",
            ));
        }

        // 4. `compile` (default true).
        let run_compile = as_bool(get(&params, "compile"), true);

        // 5. Validate.
        let mut errors = validate_document(&document, run_compile)?;

        // 6. The cap.
        let limit = limit as usize;
        if errors.len() > limit {
            let hidden = errors.len() - limit;
            errors.truncate(limit);
            errors.push(format!("... and {hidden} more error(s)."));
        }

        // 7. The result.
        let ok = errors.is_empty();
        let mut value = Dict::new();
        value.insert(Value::Str("ok".to_string()), Value::Bool(ok));
        value.insert(
            Value::Str("errors".to_string()),
            Value::List(errors.iter().cloned().map(Value::Str).collect()),
        );
        value.insert(Value::Str("yaml".to_string()), Value::Str(document));
        let value = Value::Dict(value);
        let raw = value.clone();

        let stderr = if errors.is_empty() {
            None
        } else {
            Some(errors.join("\n"))
        };

        Ok(ToolResult {
            value,
            raw,
            stdout: None,
            stderr,
            exit_code: None,
            ok: true,
            stdout_parsed: false,
        })
    }

    fn check(&self) -> CheckResult {
        // pyyaml and jsonschema are core dependencies in Circuitry --
        // nothing to probe; electricity's own `electricity-yaml`/
        // `electricity-compiler` are always linked in too.
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
    use crate::ErrorCause;
    use electricity_value::CancellationToken;

    fn call<'a>(token: &'a CancellationToken, config: &'a Value) -> ToolCall<'a> {
        ToolCall {
            timeout_seconds: 30,
            config,
            token,
            armed: false,
        }
    }

    fn params(pairs: Vec<(&str, Value)>) -> Value {
        let mut dict = Dict::new();
        for (k, v) in pairs {
            dict.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(dict)
    }

    async fn run(params: Value) -> Result<ToolResult, ToolError> {
        let token = CancellationToken::new();
        let config = Value::None;
        ValidateYamlPlugin
            .execute(params, &call(&token, &config))
            .await
    }

    fn errors_of(result: &ToolResult) -> Vec<String> {
        result
            .value
            .as_dict()
            .unwrap()
            .get(&Value::Str("errors".to_string()))
            .unwrap()
            .as_list()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    fn ok_of(result: &ToolResult) -> bool {
        result
            .value
            .as_dict()
            .unwrap()
            .get(&Value::Str("ok".to_string()))
            .unwrap()
            .as_bool()
            .unwrap()
    }

    #[tokio::test]
    async fn a_valid_document_validates_ok_with_no_errors() {
        let result = run(params(vec![(
            "yaml",
            Value::Str("effects: []\n".to_string()),
        )]))
        .await
        .unwrap();
        assert!(ok_of(&result));
        assert_eq!(errors_of(&result), Vec::<String>::new());
        assert!(result.ok);
        assert_eq!(result.stderr, None);
    }

    #[tokio::test]
    async fn an_absent_yaml_param_is_empty_document() {
        let result = run(params(vec![])).await.unwrap();
        assert!(!ok_of(&result));
        assert_eq!(
            errors_of(&result),
            vec!["Empty document \u{2014} expected a Circuitry orchestration YAML.".to_string()]
        );
    }

    #[tokio::test]
    async fn a_non_string_yaml_param_raises() {
        let err = run(params(vec![("yaml", Value::from(5i64))]))
            .await
            .unwrap_err();
        assert_eq!(err.py_class, crate::PyExc::ValueError);
        assert_eq!(err.cause, ErrorCause::None);
        assert_eq!(
            err.message,
            "ValidateYamlPlugin requires params['yaml'] as a string (got int)."
        );
    }

    #[tokio::test]
    async fn strip_fences_default_cleans_a_fenced_document_and_echoes_it() {
        let result = run(params(vec![(
            "yaml",
            Value::Str("```yaml\neffects: []\n```".to_string()),
        )]))
        .await
        .unwrap();
        assert!(ok_of(&result));
        let yaml = result
            .value
            .as_dict()
            .unwrap()
            .get(&Value::Str("yaml".to_string()))
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(yaml, "effects: []");
    }

    #[tokio::test]
    async fn strip_fences_false_leaves_fences_in_for_the_parser() {
        let result = run(params(vec![
            ("yaml", Value::Str("```yaml\neffects: []\n```".to_string())),
            ("strip_fences", Value::Bool(false)),
        ]))
        .await
        .unwrap();
        assert!(!ok_of(&result));
        assert!(errors_of(&result)[0].starts_with("YAML parse error: "));
    }

    #[tokio::test]
    async fn not_a_mapping_reports_the_python_type_name() {
        for (yaml, expected_type) in [
            ("2024-01-01", "date"),
            ("# only a comment", "NoneType"),
            ("- a", "list"),
        ] {
            let result = run(params(vec![("yaml", Value::Str(yaml.to_string()))]))
                .await
                .unwrap();
            assert_eq!(
                errors_of(&result),
                vec![format!(
                    "Orchestration must be a YAML mapping with an 'effects' key, got {expected_type}."
                )],
                "yaml: {yaml:?}"
            );
        }
    }

    #[tokio::test]
    async fn missing_effects_key_is_reported() {
        let result = run(params(vec![("yaml", Value::Str("name: x".to_string()))]))
            .await
            .unwrap();
        assert_eq!(
            errors_of(&result),
            vec!["Orchestration is missing the required top-level 'effects' key.".to_string()]
        );
    }

    #[tokio::test]
    async fn compile_false_skips_the_compiler() {
        let yaml = "effects:\n  - {type: tool, name: a, provider: json}\n  - {type: tool, name: a, provider: json}\n";
        let with_compile = run(params(vec![("yaml", Value::Str(yaml.to_string()))]))
            .await
            .unwrap();
        assert!(!ok_of(&with_compile));
        assert!(errors_of(&with_compile)[0].starts_with("Compile error: "));

        let without_compile = run(params(vec![
            ("yaml", Value::Str(yaml.to_string())),
            ("compile", Value::Bool(false)),
        ]))
        .await
        .unwrap();
        assert!(ok_of(&without_compile));
    }

    #[tokio::test]
    async fn max_errors_non_integer_string_raises() {
        let err = run(params(vec![
            ("yaml", Value::Str("effects: []".to_string())),
            ("max_errors", Value::Str("x".to_string())),
        ]))
        .await
        .unwrap_err();
        assert!(
            err.message
                .starts_with("ValidateYamlPlugin params['max_errors'] must be an integer: ")
        );
    }

    #[tokio::test]
    async fn max_errors_wrong_type_raises() {
        let err = run(params(vec![
            ("yaml", Value::Str("effects: []".to_string())),
            ("max_errors", Value::List(vec![Value::from(1i64)])),
        ]))
        .await
        .unwrap_err();
        assert!(
            err.message
                .starts_with("ValidateYamlPlugin params['max_errors'] must be an integer: ")
        );
    }

    #[tokio::test]
    async fn max_errors_below_one_raises() {
        let err = run(params(vec![
            ("yaml", Value::Str("effects: []".to_string())),
            ("max_errors", Value::from(0i64)),
        ]))
        .await
        .unwrap_err();
        assert_eq!(
            err.message,
            "ValidateYamlPlugin params['max_errors'] must be >= 1."
        );
    }

    #[tokio::test]
    async fn max_errors_infinity_raises_bare_overflow_error() {
        // `int(float("inf"))` raises `OverflowError`, which Python's own
        // `except (TypeError, ValueError)` doesn't catch -- it escapes
        // unwrapped, with no "must be an integer:" prefix.
        let err = run(params(vec![
            ("yaml", Value::Str("effects: []".to_string())),
            ("max_errors", Value::Float(f64::INFINITY)),
        ]))
        .await
        .unwrap_err();
        assert_eq!(err.py_class, PyExc::OverflowError);
        assert_eq!(err.message, "cannot convert float infinity to integer");
        assert_eq!(err.cause, ErrorCause::None);
    }

    #[tokio::test]
    async fn an_invalid_timestamp_raises_pythons_own_value_error() {
        // `x: 2024-02-30` parses the key fine but the implicit
        // `!!timestamp` value fails CPython's own `datetime.date`
        // construction -- a bare, uncaught `ValueError`
        // `core/yaml_load.py::load_yaml` never catches, so
        // `ValidateYamlPlugin` raises instead of returning `ok: false`.
        for (yaml, message) in [
            ("x: 2024-02-30", "day is out of range for month"),
            ("x: 2024-13-01", "month must be in 1..12"),
            ("x: 2024-01-01 25:00:00", "hour must be in 0..23"),
        ] {
            let err = run(params(vec![("yaml", Value::Str(yaml.to_string()))]))
                .await
                .unwrap_err();
            assert_eq!(err.py_class, PyExc::ValueError, "yaml={yaml}");
            assert_eq!(err.message, message, "yaml={yaml}");
            assert_eq!(err.cause, ErrorCause::None, "yaml={yaml}");
        }
    }

    #[tokio::test]
    async fn max_errors_float_truncates() {
        // `int(2.9) == 2`: accepted, truncated, not rounded.
        let yaml = "effects:\n  - {type: nope1}\n  - {type: nope2}\n  - {type: nope3}\n";
        let result = run(params(vec![
            ("yaml", Value::Str(yaml.to_string())),
            ("max_errors", Value::Float(2.9)),
        ]))
        .await
        .unwrap();
        let errors = errors_of(&result);
        assert_eq!(errors.len(), 3);
        assert_eq!(errors[2], "... and 1 more error(s).");
    }

    #[tokio::test]
    async fn stderr_is_none_when_there_are_no_errors_else_joined_with_newlines() {
        let ok_result = run(params(vec![(
            "yaml",
            Value::Str("effects: []".to_string()),
        )]))
        .await
        .unwrap();
        assert_eq!(ok_result.stderr, None);

        let bad_result = run(params(vec![("yaml", Value::Str("name: x".to_string()))]))
            .await
            .unwrap();
        assert_eq!(
            bad_result.stderr,
            Some("Orchestration is missing the required top-level 'effects' key.".to_string())
        );
    }

    #[test]
    fn check_reports_ok_with_no_missing_dependencies() {
        let result = ValidateYamlPlugin.check();
        assert!(result.ok);
        assert_eq!(result.missing, Vec::<String>::new());
        assert_eq!(result.message, None);
    }
}
