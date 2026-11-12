//! `core/compiler.py::_check_param_secrets` (issue #598/#725), ported:
//! `{secret: <name>, prefix?: <text>}` in a tool effect's literal
//! `params` is only ever honoured as a value in an `http` effect's
//! `params.headers`; anywhere else it is a compile error. Well-formed
//! references found in `params.headers` are returned, by header name,
//! so [`crate::compile::leaves::compile_tool`] can bake them straight
//! into [`electricity_bytecode::effects::ToolOp::secret_headers`]
//! instead of re-deriving them from the compiled [`electricity_bytecode::
//! ParamNode`] tree at dispatch time.
//!
//! `core/compiler.py`'s own "only a document run by path may name a
//! secret" refusal has no counterpart here: every document electricity
//! runs is trusted (DESIGN.md §11), so that branch never fires.

use crate::CompileError;
use electricity_bytecode::secrets::{SecretRef, has_secret_key, is_secret_ref, secret_ref, secret_ref_error};
use electricity_value::{Dict, Value};
use indexmap::IndexMap;

fn key_label(key: &Value) -> String {
    match key {
        Value::Str(s) => s.clone(),
        other => other.py_str(),
    }
}

/// Walks *value* (a tool effect's literal `params`, or a sub-value of
/// it), recording a well-formed reference at an *allowed* field path
/// into *found*, and erroring on a well-formed reference anywhere else.
/// *allowed* is the exact set of field paths a secret reference may
/// occupy -- `params.headers.<name>` for each of `http`'s own header
/// names, empty for every other provider.
fn walk(
    value: &Value,
    field: String,
    allowed: &IndexMap<String, ()>,
    label: &str,
    found: &mut IndexMap<String, SecretRef>,
) -> Result<(), CompileError> {
    if allowed.contains_key(&field) && has_secret_key(value) {
        if let Some(problem) = secret_ref_error(value) {
            return Err(CompileError(format!(
                "{label}: {field}: a secret reference {problem}."
            )));
        }
        let header_name = field
            .rsplit('.')
            .next()
            .expect("field always has at least one segment")
            .to_string();
        found.insert(
            header_name,
            secret_ref(value).expect("secret_ref_error returned None above"),
        );
        return Ok(());
    }
    if is_secret_ref(value) {
        return Err(CompileError(format!(
            "{label}: {field} is a {{secret: ...}} value, which is allowed only as a \
             value in an http effect's params.headers (or a subprocess tool's \
             runtime.plugins.<name>.env)."
        )));
    }
    match value {
        Value::Dict(dict) => {
            for (key, item) in dict {
                walk(item, format!("{field}.{}", key_label(key)), allowed, label, found)?;
            }
        }
        Value::List(items) => {
            for (index, item) in items.iter().enumerate() {
                walk(item, format!("{field}[{index}]"), allowed, label, found)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Checks *params* (a tool effect's literal `params:`) for `{secret: ...}`
/// placement, returning the well-formed references found in `http`'s own
/// `params.headers`, by header name. *provider* is already normalized
/// (`.strip().lower()`).
pub(crate) fn check_param_secrets(
    params: &Dict,
    name: &str,
    provider: &str,
    effect_path: &str,
) -> Result<IndexMap<String, SecretRef>, CompileError> {
    let mut allowed: IndexMap<String, ()> = IndexMap::new();
    if provider == "http" {
        if let Some(Value::Dict(headers)) = params.get(&Value::Str("headers".to_string())) {
            for key in headers.keys() {
                allowed.insert(format!("params.headers.{}", key_label(key)), ());
            }
        }
    }
    let label = format!("Tool effect '{name}' at '{effect_path}'");
    let mut found = IndexMap::new();
    walk(
        &Value::Dict(params.clone()),
        "params".to_string(),
        &allowed,
        &label,
        &mut found,
    )?;
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict as VDict;

    fn secret_value(name: &str) -> Value {
        let mut d = VDict::new();
        d.insert(Value::Str("secret".to_string()), Value::from(name));
        Value::Dict(d)
    }

    #[test]
    fn a_well_placed_http_header_secret_is_collected() {
        let mut headers = VDict::new();
        headers.insert(Value::Str("Authorization".to_string()), secret_value("github"));
        let mut params = VDict::new();
        params.insert(Value::Str("headers".to_string()), Value::Dict(headers));
        let found = check_param_secrets(&params, "call", "http", "prime.call").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found.get("Authorization").unwrap().name, "github");
    }

    #[test]
    fn a_secret_outside_headers_is_a_compile_error() {
        let mut params = VDict::new();
        params.insert(Value::Str("body".to_string()), secret_value("github"));
        let err = check_param_secrets(&params, "call", "http", "prime.call").unwrap_err();
        assert!(err.0.contains("is allowed only as a value in an http effect's params.headers"));
    }

    #[test]
    fn a_secret_in_a_non_http_providers_params_is_a_compile_error() {
        let mut headers = VDict::new();
        headers.insert(Value::Str("Authorization".to_string()), secret_value("github"));
        let mut params = VDict::new();
        params.insert(Value::Str("headers".to_string()), Value::Dict(headers));
        let err = check_param_secrets(&params, "call", "shell", "prime.call").unwrap_err();
        assert!(err.0.contains("is allowed only as a value in an http effect's params.headers"));
    }

    #[test]
    fn a_malformed_header_secret_is_a_compile_error_naming_the_problem() {
        let mut secret = VDict::new();
        secret.insert(Value::Str("secret".to_string()), Value::from("github"));
        secret.insert(Value::Str("extra".to_string()), Value::from("x"));
        let mut headers = VDict::new();
        headers.insert(Value::Str("Authorization".to_string()), Value::Dict(secret));
        let mut params = VDict::new();
        params.insert(Value::Str("headers".to_string()), Value::Dict(headers));
        let err = check_param_secrets(&params, "call", "http", "prime.call").unwrap_err();
        assert!(err.0.contains("unknown key"));
    }

    #[test]
    fn a_secret_look_alike_nested_inside_a_json_body_is_literal_data() {
        // Like a `{from: ...}` leaf, only {secret, prefix?} with a string
        // `secret` is a reference -- any other shape is passed through.
        let mut odd = VDict::new();
        odd.insert(Value::Str("secret".to_string()), Value::from("x"));
        odd.insert(Value::Str("response".to_string()), Value::from("r"));
        let mut params = VDict::new();
        params.insert(Value::Str("json".to_string()), Value::Dict(odd));
        assert!(check_param_secrets(&params, "call", "http", "prime.call").is_ok());
    }
}
