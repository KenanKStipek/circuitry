//! `plugins/factory.py::build_plugin`, ported -- a per-attempt plugin
//! builder (issue #449's gate lane, item 2), replacing the flat
//! [`crate::ToolRegistry`] as `electricity_vm::exec::tool::execute_tool`'s
//! own dispatch seam: lane C calls this *inside* the retry loop, exactly
//! where `core/tool.py::ToolRuntime.execute` calls Python's own
//! `build_plugin`, so a per-plugin config error fails one attempt and is
//! retried like any other non-HTTP failure, never treated as a document
//! error caught up front.
//!
//! [`BUILDERS`] is deliberately small: only the providers this crate can
//! actually build today (`json`, plus `sleep`/`fail` under the
//! `test-tools` feature). The unknown-plugin message's own "Supported
//! plugins" list is *not* this map's own key set -- it is
//! [`crate::plugin_names::PLUGIN_NAMES`], Circuitry's **full** registry,
//! because that is what `plugins/factory.py::build_plugin`'s own message
//! would say for the same *plugin_name*: the message names what
//! Circuitry itself recognizes, not what this one Rust crate has ported
//! so far. In ordinary operation this path is unreachable in any case --
//! `electricity_bytecode::refusal::first_unsupported`'s own `Supported`
//! value already refuses a document naming any provider this crate
//! can't build, long before a real run ever calls this function -- but
//! the message still has to be exactly right for the golden unit tests
//! that call it directly.

use crate::{ToolError, ToolPlugin};
use electricity_value::Value;
use std::collections::HashMap;

type Builder = fn(&Value) -> Box<dyn ToolPlugin>;

fn builders() -> HashMap<&'static str, Builder> {
    let mut map: HashMap<&'static str, Builder> = HashMap::new();
    map.insert("json", |_cfg: &Value| -> Box<dyn ToolPlugin> {
        Box::new(crate::json::JsonTool)
    });
    #[cfg(feature = "test-tools")]
    {
        map.insert("sleep", |_cfg: &Value| -> Box<dyn ToolPlugin> {
            Box::new(crate::test_tools::SleepTool)
        });
        map.insert("fail", |_cfg: &Value| -> Box<dyn ToolPlugin> {
            Box::new(crate::test_tools::FailTool)
        });
    }
    map
}

/// `(runtime or {}).get("plugins") or {}`, then `.get(plugin_name) or {}`
/// -- `runtime.plugins.<plugin_name>`, defaulting to an empty dict at
/// either level exactly as Python's own `dict.get(...) or {}` does
/// (a present-but-falsy value, e.g. `plugins: null`, is treated the
/// same as an absent key). Public: `electricity-vm`'s own `run_tool`
/// builds the same [`crate::ToolCall::config`] slice `build_plugin`
/// just used to construct the plugin itself, so both read the
/// identical config through this one function.
pub fn plugin_config_slice(runtime_config: &Value, plugin_name: &str) -> Value {
    let plugins = runtime_config
        .as_dict()
        .and_then(|d| d.get(&Value::Str("plugins".to_string())))
        .and_then(Value::as_dict);
    match plugins.and_then(|d| d.get(&Value::Str(plugin_name.to_string()))) {
        Some(Value::Dict(dict)) => Value::Dict(dict.clone()),
        _ => Value::Dict(Default::default()),
    }
}

/// `plugins/factory.py::build_plugin`. *plugin_name* is normalized the
/// same way (`.strip().lower()`) before lookup; *runtime_config* is the
/// merged `runtime:` block [`electricity_vm::RunContext::runtime_config`]
/// carries, from which this reads `runtime.plugins.<plugin_name>`.
pub fn build_plugin(
    plugin_name: &str,
    runtime_config: &Value,
) -> Result<Box<dyn ToolPlugin>, ToolError> {
    let normalized = plugin_name.trim().to_lowercase();
    let table = builders();
    let builder = table.get(normalized.as_str()).ok_or_else(|| {
        let supported = crate::plugin_names::PLUGIN_NAMES.join(", ");
        ToolError {
            py_class: crate::PyExc::ValueError,
            message: format!(
                "Unknown plugin: {}. Supported plugins: {supported}. Check the 'provider' field on your tool effect.",
                Value::Str(normalized.clone()).py_repr()
            ),
            cause: crate::ErrorCause::None,
        }
    })?;
    let cfg = plugin_config_slice(runtime_config, &normalized);
    Ok(builder(&cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_plugin_builds_json_with_no_config_needed() {
        let plugin = build_plugin("json", &Value::None).unwrap();
        assert_eq!(plugin.name(), "json");
        assert!(plugin.check().ok);
    }

    #[test]
    fn build_plugin_normalizes_whitespace_and_case() {
        let plugin = build_plugin(" JSON ", &Value::None).unwrap();
        assert_eq!(plugin.name(), "json");
    }

    #[test]
    fn an_unknown_plugin_lists_pythons_full_registry() {
        let err = match build_plugin("bogus", &Value::None) {
            Err(err) => err,
            Ok(_) => panic!("expected an unknown-plugin error"),
        };
        assert!(
            err.message
                .starts_with("Unknown plugin: 'bogus'. Supported plugins: ")
        );
        assert!(err.message.contains("json"));
        assert!(err.message.contains("shell"));
        assert!(
            err.message
                .ends_with("Check the 'provider' field on your tool effect.")
        );
    }

    #[test]
    fn a_plugin_circuitry_supports_but_this_crate_cannot_build_yet_is_still_unknown_here() {
        // `shell` is in Circuitry's own registry (and so in the
        // "Supported plugins" list the message above names), but this
        // crate has no builder for it yet (lane C) -- reachable only
        // through a direct call like this one, since the refusal
        // walker's own `Supported.providers` already keeps a real
        // document with `provider: shell` from ever reaching this
        // function.
        let err = match build_plugin("shell", &Value::None) {
            Err(err) => err,
            Ok(_) => panic!("expected an unknown-plugin error"),
        };
        assert!(err.message.starts_with("Unknown plugin: 'shell'."));
    }
}
