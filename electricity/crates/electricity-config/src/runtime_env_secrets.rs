//! `core/compiler.py::_check_runtime_env_secrets` (issue #598/#725),
//! the well-formedness half: a `{secret: ...}` in a `runtime.plugins.
//! <name>.env.<VAR>` entry, document- or host-config-set, must be a
//! well-formed reference. Resolving a well-formed one into a value
//! (the Keychain, a command, an environment variable) is not this
//! crate's concern.
//!
//! `core/compiler.py`'s own "only a document run by path may name a
//! secret" refusal has no counterpart here: every document electricity
//! runs is trusted (DESIGN.md §11), so that branch never fires.

use electricity_bytecode::secrets::{has_secret_key, secret_ref_error};
use electricity_value::Value;

/// `secrets.py::runtime_env_secret_paths` -- `(runtime.plugins.<name>.
/// env.<VAR>, value)` for every value of *runtime*'s `plugins.<name>.env`
/// that has a `secret` key, well-formed or not.
fn runtime_env_secret_paths(runtime: Option<&Value>) -> Vec<(String, Value)> {
    let Some(plugins) = runtime
        .and_then(Value::as_dict)
        .and_then(|d| d.get(&Value::Str("plugins".to_string())))
        .and_then(Value::as_dict)
    else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (plugin_name, cfg) in plugins {
        let Some(env) = cfg
            .as_dict()
            .and_then(|d| d.get(&Value::Str("env".to_string())))
            .and_then(Value::as_dict)
        else {
            continue;
        };
        for (var, value) in env {
            if has_secret_key(value) {
                found.push((
                    format!("runtime.plugins.{}.env.{}", plugin_name.py_str(), var.py_str()),
                    value.clone(),
                ));
            }
        }
    }
    found
}

/// `_check_runtime_env_secrets`'s own well-formedness errors, zero or
/// more, one per malformed reference found.
pub fn runtime_env_secret_errors(runtime: Option<&Value>) -> Vec<String> {
    runtime_env_secret_paths(runtime)
        .into_iter()
        .filter_map(|(path, value)| {
            secret_ref_error(&value).map(|problem| format!("{path}: a secret reference {problem}."))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    fn runtime_with_env(plugin: &str, var: &str, value: Value) -> Value {
        let mut env = Dict::new();
        env.insert(Value::Str(var.to_string()), value);
        let mut cfg = Dict::new();
        cfg.insert(Value::Str("env".to_string()), Value::Dict(env));
        let mut plugins = Dict::new();
        plugins.insert(Value::Str(plugin.to_string()), Value::Dict(cfg));
        let mut runtime = Dict::new();
        runtime.insert(Value::Str("plugins".to_string()), Value::Dict(plugins));
        Value::Dict(runtime)
    }

    #[test]
    fn a_well_formed_env_secret_has_no_error() {
        let mut secret = Dict::new();
        secret.insert(Value::Str("secret".to_string()), Value::from("github"));
        let runtime = runtime_with_env("awk", "TOKEN", Value::Dict(secret));
        assert_eq!(runtime_env_secret_errors(Some(&runtime)), Vec::<String>::new());
    }

    #[test]
    fn a_malformed_env_secret_is_an_error_naming_the_path() {
        let mut secret = Dict::new();
        secret.insert(Value::Str("secret".to_string()), Value::from(5_i64));
        let runtime = runtime_with_env("awk", "TOKEN", Value::Dict(secret));
        let errors = runtime_env_secret_errors(Some(&runtime));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("runtime.plugins.awk.env.TOKEN: a secret reference"));
    }

    #[test]
    fn no_runtime_at_all_has_no_error() {
        assert_eq!(runtime_env_secret_errors(None), Vec::<String>::new());
    }

    #[test]
    fn an_ordinary_string_env_value_has_no_error() {
        let runtime = runtime_with_env("awk", "TOKEN", Value::from("plain"));
        assert_eq!(runtime_env_secret_errors(Some(&runtime)), Vec::<String>::new());
    }
}
