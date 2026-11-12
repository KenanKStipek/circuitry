//! `plugins/http.py::static_param_errors` (issue #726), the compile-
//! time half: `params.save_to`/`within`/`max_bytes`/`parse`, checked
//! against the document's own literal `params` (and whatever a raw
//! `params_json` that parses as JSON sets). A `{from: ...}` value or a
//! template is checked again at dispatch instead
//! ([`crate::compile::leaves::compile_tool`]'s own caller has no
//! rendered value yet).
//!
//! `cof check`'s own "only a document run by path may name `save_to`"
//! refusal (`_LIMITED_SAVE_TO`) *does* have a counterpart here: the
//! top-level document electricity runs is always trusted (DESIGN.md
//! §11), but a `use` child compiled as [`crate::DocumentOrigin::
//! LimitedFile`] or [`crate::DocumentOrigin::Generated`] is not -- the
//! same `trusted` [`crate::compile::secrets::check_param_secrets`]
//! takes.

use electricity_value::Value;

/// `plugins/http.py::_LIMITED_SAVE_TO`.
const LIMITED_SAVE_TO: &str = "only a document run by path may save a response to a file (a \
     limited document, a library entry or a generated plan cannot)";

fn key_name(key: &Value) -> String {
    match key {
        Value::Str(s) => s.clone(),
        other => other.py_str(),
    }
}

/// `core/tool.py::param_reference`'s own shape check: a mapping whose
/// keys are `from` (a string) plus any of `default`/`as`.
fn is_param_reference(value: &Value) -> bool {
    let Value::Dict(dict) = value else {
        return false;
    };
    let from_key = Value::Str("from".to_string());
    let default_key = Value::Str("default".to_string());
    let as_key = Value::Str("as".to_string());
    dict.get(&from_key).and_then(Value::as_str).is_some()
        && !dict
            .keys()
            .any(|key| *key != from_key && *key != default_key && *key != as_key)
}

/// `plugins/systemone.py::within_param_errors`, http's own copy (also
/// used from `save_to`): a literal absolute path, or `{from:
/// input.<name>}`.
fn within_param_errors(within: &Value) -> Vec<String> {
    if is_param_reference(within) {
        let Value::Dict(dict) = within else {
            unreachable!("is_param_reference checked this is a Dict");
        };
        let path = dict
            .get(&Value::Str("from".to_string()))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let is_input_path = path.starts_with("input.")
            && !path[6..].is_empty()
            && !path[6..].contains('.')
            && !path[6..].contains(char::is_whitespace);
        if !is_input_path {
            return vec![
                "params.within must be a literal absolute path or {from: input.<name>}".to_string(),
            ];
        }
        return Vec::new();
    }
    match within {
        Value::Str(s) if !s.trim().is_empty() && !s.contains("{{") && !s.contains("}}") => {
            if std::path::Path::new(s).is_absolute() {
                Vec::new()
            } else {
                vec![format!(
                    "params.within must be an absolute path, got {}",
                    within.py_repr()
                )]
            }
        }
        _ => vec![
            "params.within must be a literal absolute path or {from: input.<name>}".to_string(),
        ],
    }
}

/// `plugins/http.py::static_param_errors`, minus its `trusted` branch
/// (electricity's documents are always trusted). *params* is the tool
/// effect's own literal `params:`; *params_json* its raw (unrendered)
/// template text, sniffed as JSON the same lenient way the reference
/// does.
pub(crate) fn http_save_to_errors(
    params: &electricity_value::Dict,
    params_json: Option<&str>,
    trusted: bool,
) -> Vec<String> {
    let overlay: Option<Value> = params_json.and_then(|text| electricity_json::loads(text).ok());
    let overlay_keys: Vec<Value> = match &overlay {
        Some(Value::Dict(dict)) => dict.keys().cloned().collect(),
        _ => Vec::new(),
    };
    let mut errors = Vec::new();
    let within_key = Value::Str("within".to_string());
    if overlay_keys.contains(&within_key) {
        errors.push("params_json must not set within: only the literal params may".to_string());
    }
    let save_to_key = Value::Str("save_to".to_string());
    let has_save_to = params.contains_key(&save_to_key);
    if !trusted {
        for key in [&save_to_key, &within_key] {
            if params.contains_key(key) {
                errors.push(format!("params.{}: {LIMITED_SAVE_TO}", key_name(key)));
            }
        }
        if overlay_keys.contains(&save_to_key) {
            errors.push(format!("params_json sets save_to: {LIMITED_SAVE_TO}"));
        }
        return errors;
    }
    let has_within_via_params_json = params_json.is_some() && params.contains_key(&within_key);
    if !has_save_to && !has_within_via_params_json {
        return errors;
    }
    if has_save_to {
        let save_to = params
            .get(&save_to_key)
            .expect("checked contains_key above");
        if !is_param_reference(save_to) {
            match save_to {
                Value::Str(s) if !s.trim().is_empty() => {
                    if std::path::Path::new(s).is_absolute() {
                        errors.push(format!(
                            "params.save_to must be a relative path, got {}",
                            save_to.py_repr()
                        ));
                    }
                }
                _ => errors.push("params.save_to must be a non-empty relative path".to_string()),
            }
        }
    }
    if let Some(within) = params.get(&within_key) {
        errors.extend(within_param_errors(within));
    }
    if let Some(max_bytes) = params.get(&Value::Str("max_bytes".to_string())) {
        let is_positive_int =
            matches!(max_bytes, Value::Int(n) if !n.is_zero() && !n.is_negative());
        let ok = is_param_reference(max_bytes)
            || matches!(max_bytes, Value::Str(s) if s.contains("{{"))
            || is_positive_int;
        if !ok {
            errors.push(format!(
                "params.max_bytes must be a positive integer, got {}",
                max_bytes.py_repr()
            ));
        }
    }
    if has_save_to {
        if let Some(Value::Str(parse)) = params.get(&Value::Str("parse".to_string())) {
            if parse.to_lowercase() != "auto" {
                errors.push(
                    "params.parse must be auto (or unset) with params.save_to: the body is \
                     saved as bytes, never parsed"
                        .to_string(),
                );
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    fn params_with(pairs: &[(&str, Value)]) -> Dict {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v.clone());
        }
        d
    }

    #[test]
    fn no_save_to_or_within_is_fine() {
        let params = params_with(&[("url", Value::from("http://x"))]);
        assert_eq!(
            http_save_to_errors(&params, None, true),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_relative_save_to_is_fine() {
        let params = params_with(&[("save_to", Value::from("a/b.png"))]);
        assert_eq!(
            http_save_to_errors(&params, None, true),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_absolute_save_to_is_an_error() {
        let params = params_with(&[("save_to", Value::from("/etc/passwd"))]);
        let errors = http_save_to_errors(&params, None, true);
        assert_eq!(
            errors,
            vec!["params.save_to must be a relative path, got '/etc/passwd'".to_string()]
        );
    }

    #[test]
    fn an_absolute_within_is_fine() {
        let params = params_with(&[
            ("save_to", Value::from("a.png")),
            ("within", Value::from("/tmp/run")),
        ]);
        assert_eq!(
            http_save_to_errors(&params, None, true),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_relative_within_is_an_error() {
        let params = params_with(&[
            ("save_to", Value::from("a.png")),
            ("within", Value::from("relative/dir")),
        ]);
        let errors = http_save_to_errors(&params, None, true);
        assert_eq!(
            errors,
            vec!["params.within must be an absolute path, got 'relative/dir'".to_string()]
        );
    }

    #[test]
    fn params_json_setting_within_is_an_error() {
        let params = params_with(&[("save_to", Value::from("a.png"))]);
        let errors = http_save_to_errors(&params, Some(r#"{"within": "/tmp"}"#), true);
        assert_eq!(
            errors,
            vec!["params_json must not set within: only the literal params may".to_string()]
        );
    }

    #[test]
    fn a_non_positive_max_bytes_is_an_error() {
        let params = params_with(&[
            ("save_to", Value::from("a.png")),
            ("max_bytes", Value::from(0_i64)),
        ]);
        let errors = http_save_to_errors(&params, None, true);
        assert_eq!(
            errors,
            vec!["params.max_bytes must be a positive integer, got 0".to_string()]
        );
    }

    #[test]
    fn a_positive_max_bytes_is_fine() {
        let params = params_with(&[
            ("save_to", Value::from("a.png")),
            ("max_bytes", Value::from(1024_i64)),
        ]);
        assert_eq!(
            http_save_to_errors(&params, None, true),
            Vec::<String>::new()
        );
    }

    #[test]
    fn parse_other_than_auto_with_save_to_is_an_error() {
        let params = params_with(&[
            ("save_to", Value::from("a.png")),
            ("parse", Value::from("json")),
        ]);
        let errors = http_save_to_errors(&params, None, true);
        assert_eq!(
            errors,
            vec![
                "params.parse must be auto (or unset) with params.save_to: the body is \
                 saved as bytes, never parsed"
                    .to_string()
            ]
        );
    }

    #[test]
    fn an_untrusted_document_naming_save_to_is_an_error() {
        let params = params_with(&[("save_to", Value::from("a.png"))]);
        let errors = http_save_to_errors(&params, None, false);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("only a document run by path may save a response to a file"));
    }

    #[test]
    fn an_untrusted_document_naming_within_is_also_an_error() {
        let params = params_with(&[
            ("save_to", Value::from("a.png")),
            ("within", Value::from("/tmp/run")),
        ]);
        let errors = http_save_to_errors(&params, None, false);
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn an_untrusted_document_with_neither_save_to_nor_within_is_fine() {
        let params = params_with(&[("url", Value::from("http://x"))]);
        assert_eq!(
            http_save_to_errors(&params, None, false),
            Vec::<String>::new()
        );
    }

    #[test]
    fn within_as_an_input_reference_is_fine() {
        let mut reference = Dict::new();
        reference.insert(Value::from("from"), Value::from("input.run_dir"));
        let params = params_with(&[
            ("save_to", Value::from("a.png")),
            ("within", Value::Dict(reference)),
        ]);
        assert_eq!(
            http_save_to_errors(&params, None, true),
            Vec::<String>::new()
        );
    }
}
