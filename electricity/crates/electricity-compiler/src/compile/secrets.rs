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
//! secret" refusal (`_untrusted_secret_error`) *does* have a
//! counterpart here: the top-level document electricity runs is always
//! trusted (DESIGN.md §11), but a `use` child compiled as
//! [`crate::DocumentOrigin::LimitedFile`] or [`crate::DocumentOrigin::
//! Generated`] (an inline child, or one resolved through a limited
//! parent) is not -- the same distinction [`crate::compile::leaves::
//! compile_agent_prompt_file`] and `UseOp::parent_trusted` already
//! make.

use crate::CompileError;
use electricity_bytecode::secrets::{
    SecretRef, has_secret_key, is_secret_ref, secret_ref, secret_ref_error,
};
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
pub(crate) fn untrusted_secret_error(where_: &str, name: &str) -> CompileError {
    CompileError(format!(
        "{where_} names secret {}: only a document run by path may name a secret (a \
         limited document, a library entry or a generated plan cannot).",
        Value::Str(name.to_string()).py_repr()
    ))
}

fn walk(
    value: &Value,
    field: String,
    allowed: &IndexMap<String, ()>,
    label: &str,
    trusted: bool,
    found: &mut IndexMap<String, SecretRef>,
) -> Result<(), CompileError> {
    if allowed.contains_key(&field) && has_secret_key(value) {
        if let Some(problem) = secret_ref_error(value) {
            return Err(CompileError(format!(
                "{label}: {field}: a secret reference {problem}."
            )));
        }
        let reference = secret_ref(value).expect("secret_ref_error returned None above");
        if !trusted {
            return Err(untrusted_secret_error(label, &reference.name));
        }
        let header_name = field
            .rsplit('.')
            .next()
            .expect("field always has at least one segment")
            .to_string();
        found.insert(header_name, reference);
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
                walk(
                    item,
                    format!("{field}.{}", key_label(key)),
                    allowed,
                    label,
                    trusted,
                    found,
                )?;
            }
        }
        Value::List(items) => {
            for (index, item) in items.iter().enumerate() {
                walk(
                    item,
                    format!("{field}[{index}]"),
                    allowed,
                    label,
                    trusted,
                    found,
                )?;
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
    trusted: bool,
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
        trusted,
        &mut found,
    )?;
    Ok(found)
}

/// `core/compiler.py::_check_runtime_env_secrets` (issue #598/#725): a
/// `{secret: ...}` in *this document's own* `runtime.plugins.<name>.env`
/// -- well-formed, and only in a trusted document. Checked for every
/// document `compile_document` ever compiles, a `use` child's own
/// (never-read) `runtime:` block included: a limited child may not
/// name one even though nothing ever resolves it, the same standing
/// cof has.
pub(crate) fn check_runtime_env_secrets(
    document: &Value,
    trusted: bool,
) -> Result<(), CompileError> {
    let Some(Value::Dict(plugins)) = document
        .as_dict()
        .and_then(|d| d.get(&Value::Str("runtime".to_string())))
        .and_then(Value::as_dict)
        .and_then(|d| d.get(&Value::Str("plugins".to_string())))
    else {
        return Ok(());
    };
    for (plugin_name, cfg) in plugins {
        let Some(Value::Dict(env)) = cfg
            .as_dict()
            .and_then(|d| d.get(&Value::Str("env".to_string())))
        else {
            continue;
        };
        for (var, value) in env {
            if !has_secret_key(value) {
                continue;
            }
            let path = format!(
                "runtime.plugins.{}.env.{}",
                key_label(plugin_name),
                key_label(var)
            );
            if let Some(problem) = secret_ref_error(value) {
                return Err(CompileError(format!(
                    "{path}: a secret reference {problem}."
                )));
            }
            if !trusted {
                let reference = secret_ref(value).expect("secret_ref_error returned None above");
                return Err(untrusted_secret_error(&path, &reference.name));
            }
        }
    }
    Ok(())
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
        headers.insert(
            Value::Str("Authorization".to_string()),
            secret_value("github"),
        );
        let mut params = VDict::new();
        params.insert(Value::Str("headers".to_string()), Value::Dict(headers));
        let found = check_param_secrets(&params, "call", "http", "prime.call", true).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found.get("Authorization").unwrap().name, "github");
    }

    #[test]
    fn a_secret_outside_headers_is_a_compile_error() {
        let mut params = VDict::new();
        params.insert(Value::Str("body".to_string()), secret_value("github"));
        let err = check_param_secrets(&params, "call", "http", "prime.call", true).unwrap_err();
        assert!(
            err.0
                .contains("is allowed only as a value in an http effect's params.headers")
        );
    }

    #[test]
    fn a_secret_in_a_non_http_providers_params_is_a_compile_error() {
        let mut headers = VDict::new();
        headers.insert(
            Value::Str("Authorization".to_string()),
            secret_value("github"),
        );
        let mut params = VDict::new();
        params.insert(Value::Str("headers".to_string()), Value::Dict(headers));
        let err = check_param_secrets(&params, "call", "shell", "prime.call", true).unwrap_err();
        assert!(
            err.0
                .contains("is allowed only as a value in an http effect's params.headers")
        );
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
        let err = check_param_secrets(&params, "call", "http", "prime.call", true).unwrap_err();
        assert!(err.0.contains("unknown key"));
    }

    #[test]
    fn an_untrusted_document_naming_a_well_placed_secret_is_a_compile_error() {
        let mut headers = VDict::new();
        headers.insert(
            Value::Str("Authorization".to_string()),
            secret_value("github"),
        );
        let mut params = VDict::new();
        params.insert(Value::Str("headers".to_string()), Value::Dict(headers));
        let err = check_param_secrets(&params, "call", "http", "prime.call", false).unwrap_err();
        assert_eq!(
            err.0,
            "Tool effect 'call' at 'prime.call' names secret 'github': only a document run \
             by path may name a secret (a limited document, a library entry or a generated \
             plan cannot)."
        );
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
        assert!(check_param_secrets(&params, "call", "http", "prime.call", true).is_ok());
    }

    fn document_with_plugin_env(plugin: &str, var: &str, value: Value) -> Value {
        let mut env = VDict::new();
        env.insert(Value::Str(var.to_string()), value);
        let mut cfg = VDict::new();
        cfg.insert(Value::Str("env".to_string()), Value::Dict(env));
        let mut plugins = VDict::new();
        plugins.insert(Value::Str(plugin.to_string()), Value::Dict(cfg));
        let mut runtime = VDict::new();
        runtime.insert(Value::Str("plugins".to_string()), Value::Dict(plugins));
        let mut document = VDict::new();
        document.insert(Value::Str("runtime".to_string()), Value::Dict(runtime));
        Value::Dict(document)
    }

    #[test]
    fn a_trusted_documents_own_runtime_env_secret_is_fine() {
        let document = document_with_plugin_env("gh", "TOKEN", secret_value("github"));
        assert!(check_runtime_env_secrets(&document, true).is_ok());
    }

    #[test]
    fn an_untrusted_documents_own_runtime_env_secret_is_a_compile_error() {
        // issue #598/#725: cof's own `_check_runtime_env_secrets` checks
        // every document it compiles, including a `use` child's own
        // `runtime:` block -- never read at run time, but still not
        // honoured from a document that isn't run by path.
        let document = document_with_plugin_env("gh", "TOKEN", secret_value("github"));
        let err = check_runtime_env_secrets(&document, false).unwrap_err();
        assert_eq!(
            err.0,
            "runtime.plugins.gh.env.TOKEN names secret 'github': only a document run by \
             path may name a secret (a limited document, a library entry or a generated \
             plan cannot)."
        );
    }

    #[test]
    fn a_malformed_runtime_env_secret_is_a_compile_error_even_when_trusted() {
        let mut malformed = VDict::new();
        malformed.insert(Value::Str("secret".to_string()), Value::from("github"));
        malformed.insert(Value::Str("extra".to_string()), Value::from("x"));
        let document = document_with_plugin_env("gh", "TOKEN", Value::Dict(malformed));
        let err = check_runtime_env_secrets(&document, true).unwrap_err();
        assert!(err.0.contains("runtime.plugins.gh.env.TOKEN"), "{}", err.0);
        assert!(err.0.contains("unknown key"), "{}", err.0);
    }

    #[test]
    fn a_document_with_no_runtime_env_secret_is_fine_whatever_its_trust() {
        let document = document_with_plugin_env("gh", "TOKEN", Value::from("plain"));
        assert!(check_runtime_env_secrets(&document, false).is_ok());
    }
}
