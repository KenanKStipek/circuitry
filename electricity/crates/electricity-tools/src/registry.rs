//! `plugins/factory.py::build_plugin`, ported -- a per-attempt plugin
//! builder (issue #449's gate lane, item 2), replacing the flat
//! [`crate::ToolRegistry`] as `electricity_vm::exec::tool::execute_tool`'s
//! own dispatch seam: lane C calls this *inside* the retry loop, exactly
//! where `core/tool.py::ToolRuntime.execute` calls Python's own
//! `build_plugin`, so a per-plugin config error fails one attempt and is
//! retried like any other non-HTTP failure, never treated as a document
//! error caught up front.
//!
//! `builders()` has an entry for every M1 native-tool lane's own
//! provider (`json`, plus `sleep`/`fail`/`flaky_build` under the
//! `test-tools` feature, plus `shell`/`awk`/`imagemagick`/`ffmpeg`/`fs`/
//! `env_vars`/`hash`/`uuid`/`clock`/`regex`/`http`/`validate_yaml`),
//! each one returning that
//! lane's own stub plugin (whose own `execute` still fails with a
//! not-implemented message) rather than this function's own
//! "Unknown plugin" text -- so lane C/D1/D2/E/L2's own first PR only
//! ever fills a body in the file this map already names a builder for,
//! never adds an entry to this shared map itself. The unknown-plugin
//! message's own "Supported plugins" list is still *not* this map's own
//! key set -- it is [`crate::plugin_names::PLUGIN_NAMES`], Circuitry's
//! **full** registry, because that is what `plugins/factory.py::
//! build_plugin`'s own message would say for the same *plugin_name*:
//! the message names what Circuitry itself recognizes, not what this
//! one Rust crate has a builder for. In ordinary operation this path is
//! unreachable for any M1 provider in any case --
//! `electricity_bytecode::refusal::first_unsupported`'s own `Supported`
//! value already refuses a document naming one before its own lane
//! flips the capability flag, long before a real run ever calls this
//! function -- but the message still has to be exactly right for the
//! golden unit tests that call it directly, and for a genuinely
//! unimplemented (M2-scope) name like `mcp`.

use crate::{ToolError, ToolPlugin};
use electricity_value::Value;
use std::collections::HashMap;

/// Fallible: a builder that validates its own `runtime.plugins.<name>`
/// config (`plugins/shell.py`'s own `_build_shell`, `_subprocess.py`'s
/// own `plugin_env_override`) can fail on a single attempt without that
/// failure being mistaken for a document-shape error caught up front --
/// `core/tool.py::ToolRuntime.execute` calls Python's own `build_plugin`
/// inside its retry loop for exactly this reason (M1 gate seam S1).
type Builder = fn(&Value) -> Result<Box<dyn ToolPlugin>, ToolError>;

fn builders() -> HashMap<&'static str, Builder> {
    let mut map: HashMap<&'static str, Builder> = HashMap::new();
    map.insert(
        "json",
        |_cfg: &Value| -> Result<Box<dyn ToolPlugin>, ToolError> {
            Ok(Box::new(crate::json::JsonTool))
        },
    );
    // Every M1 native-tool lane's own provider is pre-registered
    // here now, each builder returning that lane's own (still
    // not-implemented) stub plugin --
    // so lane C/D1/D2/E/L2's own first PR only ever fills a body in
    // the file this map already names, never adds a line to this
    // shared `HashMap::insert` call sequence another lane might also
    // be touching in the same window. The refusal walker's own
    // `Supported` flags (not this map) are still what keeps a real
    // document from reaching any of these before its own lane lands --
    // this map only changes what a *direct* `build_plugin` call (this
    // module's own tests, or an embedder bypassing the walker) gets
    // back: the real stub plugin (whose `execute` fails with its own
    // "not implemented yet" message) instead of this function's
    // "Unknown plugin" text, which would otherwise wrongly suggest
    // Circuitry itself doesn't recognize the name.
    map.insert(
        "shell",
        |cfg: &Value| -> Result<Box<dyn ToolPlugin>, ToolError> { crate::shell::make_plugin(cfg) },
    );
    map.insert(
        "awk",
        |cfg: &Value| -> Result<Box<dyn ToolPlugin>, ToolError> { crate::awk::make_plugin(cfg) },
    );
    map.insert(
        "imagemagick",
        |cfg: &Value| -> Result<Box<dyn ToolPlugin>, ToolError> {
            crate::imagemagick::make_plugin(cfg)
        },
    );
    map.insert(
        "ffmpeg",
        |cfg: &Value| -> Result<Box<dyn ToolPlugin>, ToolError> { crate::ffmpeg::make_plugin(cfg) },
    );
    map.insert(
        "fs",
        |_cfg: &Value| -> Result<Box<dyn ToolPlugin>, ToolError> {
            Ok(Box::new(crate::fs::FsPlugin))
        },
    );
    map.insert(
        "env_vars",
        |_cfg: &Value| -> Result<Box<dyn ToolPlugin>, ToolError> {
            Ok(Box::new(crate::env_vars::EnvVarsPlugin))
        },
    );
    map.insert(
        "hash",
        |_cfg: &Value| -> Result<Box<dyn ToolPlugin>, ToolError> {
            Ok(Box::new(crate::hash::HashPlugin))
        },
    );
    map.insert(
        "uuid",
        |_cfg: &Value| -> Result<Box<dyn ToolPlugin>, ToolError> {
            Ok(Box::new(crate::uuid::UuidPlugin))
        },
    );
    map.insert(
        "clock",
        |_cfg: &Value| -> Result<Box<dyn ToolPlugin>, ToolError> {
            Ok(Box::new(crate::clock::ClockPlugin))
        },
    );
    map.insert(
        "regex",
        |_cfg: &Value| -> Result<Box<dyn ToolPlugin>, ToolError> {
            Ok(Box::new(crate::regex::RegexPlugin))
        },
    );
    map.insert(
        "http",
        |_cfg: &Value| -> Result<Box<dyn ToolPlugin>, ToolError> {
            Ok(Box::new(crate::http::HttpPlugin))
        },
    );
    map.insert(
        "validate_yaml",
        |_cfg: &Value| -> Result<Box<dyn ToolPlugin>, ToolError> {
            Ok(Box::new(crate::validate_yaml::ValidateYamlPlugin))
        },
    );
    #[cfg(feature = "test-tools")]
    {
        map.insert(
            "sleep",
            |_cfg: &Value| -> Result<Box<dyn ToolPlugin>, ToolError> {
                Ok(Box::new(crate::test_tools::SleepTool))
            },
        );
        map.insert(
            "fail",
            |_cfg: &Value| -> Result<Box<dyn ToolPlugin>, ToolError> {
                Ok(Box::new(crate::test_tools::FailTool))
            },
        );
        map.insert("flaky_build", crate::test_tools::build_flaky);
    }
    map
}

/// Every name *this crate* actually has a builder for in
/// [`builders`], sorted -- [`electricity_vm::adapter::AdapterRegistry::
/// supported_names`]'s own tool-side counterpart, used the same way:
/// a caller that needs to know what's buildable right now, as opposed
/// to [`crate::plugin_names::PLUGIN_NAMES`], Circuitry's full registry.
pub fn supported_names() -> Vec<String> {
    let mut names: Vec<String> = builders().keys().map(|s| s.to_string()).collect();
    names.sort();
    names
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
    let normalized = electricity_value::pycompat::py_strip(plugin_name).to_lowercase();
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
    builder(&cfg)
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
    fn build_plugin_strips_c0_control_whitespace_rusts_trim_does_not() {
        // `plugins/factory.py::build_plugin`'s own
        // `.strip().lower()` strips C0 control whitespace
        // (`\x1c`-`\x1f`) Rust's `str::trim` does not -- `py_strip` is
        // the shared port of that exact whitespace set.
        let plugin = build_plugin("\u{1c}JSON\u{1d}", &Value::None).unwrap();
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
    fn a_plugin_circuitry_supports_but_no_m1_lane_builds_is_still_unknown_here() {
        // `mcp` is in Circuitry's own registry (and so in the
        // "Supported plugins" list the message above names), but it's
        // M2 scope (#448's own "Out of scope" list) -- no M1 lane ever
        // builds it, so `builders()` never gets an entry for it, unlike
        // `shell`/`fs`/`regex`/`http`/`validate_yaml` (each of those
        // is pre-registered here now, returning its own lane's
        // still-not-implemented stub plugin, reachable only through
        // a direct call like this one --
        // the refusal walker's own `Supported::allows_provider` keeps a
        // real document with one of those providers from ever reaching
        // this function before its own lane flips the flag).
        let err = match build_plugin("mcp", &Value::None) {
            Err(err) => err,
            Ok(_) => panic!("expected an unknown-plugin error"),
        };
        assert!(err.message.starts_with("Unknown plugin: 'mcp'."));
    }

    #[test]
    fn build_plugin_validates_the_shell_host_pin_per_attempt() {
        // `shell::make_plugin` (lane C's own `_build_shell` seam) --
        // a non-list `allowed_commands` pin fails this one attempt's
        // build, matching Python's own per-attempt `ValueError`.
        let mut cfg = electricity_value::Dict::new();
        let mut plugins = electricity_value::Dict::new();
        let mut shell_cfg = electricity_value::Dict::new();
        shell_cfg.insert(Value::from("allowed_commands"), Value::from("not-a-list"));
        plugins.insert(Value::from("shell"), Value::Dict(shell_cfg));
        cfg.insert(Value::from("plugins"), Value::Dict(plugins));
        let err = match build_plugin("shell", &Value::Dict(cfg)) {
            Err(err) => err,
            Ok(_) => panic!("expected a bad-pin build error"),
        };
        assert_eq!(
            err.message,
            "runtime.plugins.shell.allowed_commands must be a list of strings."
        );
    }

    #[test]
    fn build_plugin_builds_a_pinned_shell_plugin_from_config() {
        let mut cfg = electricity_value::Dict::new();
        let mut plugins = electricity_value::Dict::new();
        let mut shell_cfg = electricity_value::Dict::new();
        shell_cfg.insert(
            Value::from("allowed_commands"),
            Value::List(vec![Value::from("ls")]),
        );
        plugins.insert(Value::from("shell"), Value::Dict(shell_cfg));
        cfg.insert(Value::from("plugins"), Value::Dict(plugins));
        let plugin = build_plugin("shell", &Value::Dict(cfg)).unwrap();
        assert_eq!(plugin.name(), "shell");
    }

    #[test]
    fn every_m1_native_provider_builds_its_own_still_not_implemented_stub() {
        // `builders()` now has an entry for every M1 native-tool
        // lane's own provider, each returning that lane's own stub
        // plugin -- `execute()` still
        // fails with that plugin's own "not implemented yet" message,
        // but `build_plugin` itself no longer reports it as unknown.
        for name in [
            "shell",
            "awk",
            "imagemagick",
            "ffmpeg",
            "fs",
            "env_vars",
            "hash",
            "uuid",
            "clock",
            "regex",
            "http",
            "validate_yaml",
        ] {
            let plugin = build_plugin(name, &Value::None)
                .unwrap_or_else(|err| panic!("{name}: expected a stub plugin, got {err:?}"));
            assert_eq!(plugin.name(), name);
        }
    }
}
