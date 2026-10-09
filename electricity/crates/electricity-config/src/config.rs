//! `cli/config.py`'s own config-resolution port, narrowed to the one
//! branch electricity ever reaches: the CLI's `<config.json>` is always
//! an explicit path (never discovered), so this is `resolve_config`'s
//! `--config <path>`/`CIRCUITRY_CONFIG` branch -- `SANE_DEFAULTS` deep-
//! merged with the named file, then the `CIRCUITRY_*` environment
//! overlays (`_apply_env_vars`) -- with no global config, no project-
//! config discovery/trust, and no `CIRCUITRY_CONFIG` env var of its own
//! (the positional *is* that explicit path). `ConfigSource`/`sources`
//! reporting (`cof config`'s own `Config:` line) has no electricity
//! surface either, so it is not ported.

use crate::util::get;
use electricity_json::ReadError;
use electricity_value::{Dict, Value};
use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::Path;

/// `cli/config.py::ConfigError` -- a config file the caller pointed at
/// cannot be used. Also [`crate::validate_complexity`]/
/// [`crate::validate_persistence`]'s own error shape: a malformed
/// `runtime.complexity`/`runtime.persistence` block raises the same
/// way (`ComplexityConfigError` subclasses this in Circuitry; this
/// crate doesn't need the subclass distinction, only the message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ConfigError {}

/// `cli/config.py::CircuitryConfig`, narrowed to the fields electricity
/// reads: no `trust_orchestration_runtime` (every document electricity
/// runs is named by path, so [`crate::effective_settings`] always takes
/// the trusted branch -- see that module's own doc comment), no
/// `project_config`/`sources` (nothing to report: there is no discovery
/// to explain). `plugins` keeps Circuitry's own looseness -- a
/// non-string entry is a [`crate::effective_settings`]-time error
/// (`"Plugins must be strings."`), not dropped silently here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CircuitryConfig {
    pub default_model: Option<String>,
    pub default_adapter: Option<String>,
    pub plugins: Vec<Value>,
    /// `None` is default-open (no enforcement); `Some(list)` -- `[]`
    /// included -- is strict. Lower-cased, matching
    /// `enabled_adapters`/`enabled_tools`'s case-insensitive comparison
    /// everywhere else (`_normalize_allowlist(..., lowercase=True)`).
    pub enabled_adapters: Option<Vec<String>>,
    /// Dotted Python import paths, case-sensitive -- not lower-cased.
    pub enabled_plugins: Option<Vec<String>>,
    pub enabled_tools: Option<Vec<String>>,
    pub environment: String,
    /// The config's own `runtime:` block, after the deep merge and env
    /// overlays -- `None` only when it is not a mapping after the
    /// merge (Circuitry's own `CircuitryConfig.runtime` is never `None`,
    /// always at least `{}`; this crate's version collapses "absent"
    /// and "not an object" into the same `None`, a narrow, deliberate
    /// divergence from a config shape Circuitry itself would crash on
    /// with an unhandled `TypeError`/`ValueError` rather than a
    /// friendly [`ConfigError`] -- see this module's own tests).
    pub runtime: Option<Value>,
}

impl Default for CircuitryConfig {
    fn default() -> Self {
        CircuitryConfig {
            default_model: None,
            default_adapter: None,
            plugins: Vec::new(),
            enabled_adapters: None,
            enabled_plugins: None,
            enabled_tools: None,
            environment: "dev".to_string(),
            runtime: None,
        }
    }
}

const VALID_ENVIRONMENTS: [&str; 3] = ["dev", "prod", "test"];

/// `cli/config.py::SANE_DEFAULTS`.
pub(crate) fn sane_defaults() -> Dict {
    let mut ollama = Dict::new();
    ollama.insert(
        Value::Str("base_url".to_string()),
        Value::Str("http://localhost:11434".to_string()),
    );
    let mut adapters = Dict::new();
    adapters.insert(Value::Str("ollama".to_string()), Value::Dict(ollama));

    let mut comfyui = Dict::new();
    comfyui.insert(
        Value::Str("base_url".to_string()),
        Value::Str("http://localhost:8188".to_string()),
    );
    let mut plugins = Dict::new();
    plugins.insert(Value::Str("comfyui".to_string()), Value::Dict(comfyui));

    let mut runtime = Dict::new();
    runtime.insert(Value::Str("adapters".to_string()), Value::Dict(adapters));
    runtime.insert(Value::Str("plugins".to_string()), Value::Dict(plugins));

    let mut defaults = Dict::new();
    defaults.insert(
        Value::Str("default_model".to_string()),
        Value::Str("llama3.1:8b".to_string()),
    );
    defaults.insert(
        Value::Str("default_adapter".to_string()),
        Value::Str("ollama".to_string()),
    );
    defaults.insert(Value::Str("enabled_adapters".to_string()), Value::None);
    defaults.insert(Value::Str("enabled_plugins".to_string()), Value::None);
    defaults.insert(Value::Str("enabled_tools".to_string()), Value::None);
    defaults.insert(
        Value::Str("environment".to_string()),
        Value::Str("dev".to_string()),
    );
    defaults.insert(Value::Str("runtime".to_string()), Value::Dict(runtime));
    defaults
}

/// `cli/config.py::_deep_merge`: *overlay* onto *base*, recursing only
/// where both sides are an object; anything else, *overlay*'s value
/// wins outright.
pub(crate) fn deep_merge(base: &Dict, overlay: &Dict) -> Dict {
    let mut merged = base.clone();
    for (key, value) in overlay {
        let recurse = match (merged.get(key), value) {
            (Some(Value::Dict(base_dict)), Value::Dict(overlay_dict)) => {
                Some(Value::Dict(deep_merge(base_dict, overlay_dict)))
            }
            _ => None,
        };
        merged.insert(key.clone(), recurse.unwrap_or_else(|| value.clone()));
    }
    merged
}

/// `cli/config.py::_normalize_allowlist`. `None` -> `None`
/// (default-open); a list -> its truthy strings, trimmed (and
/// lower-cased when *lowercase*); anything else -> `None` (unset).
fn normalize_allowlist(value: Option<&Value>, lowercase: bool) -> Option<Vec<String>> {
    match value {
        None | Some(Value::None) => None,
        Some(Value::List(items)) => Some(
            items
                .iter()
                .map(Value::py_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(|s| if lowercase { s.to_lowercase() } else { s })
                .collect(),
        ),
        Some(_) => None,
    }
}

/// `CircuitryConfig.from_dict`.
fn config_from_dict(merged: &Dict) -> CircuitryConfig {
    let default_model = get(merged, "default_model")
        .and_then(Value::as_str)
        .map(str::to_string);
    let default_adapter = get(merged, "default_adapter")
        .and_then(Value::as_str)
        .map(str::to_string);
    let plugins = get(merged, "plugins")
        .and_then(Value::as_list)
        .map(<[Value]>::to_vec)
        .unwrap_or_default();
    let environment = get(merged, "environment")
        .and_then(Value::as_str)
        .filter(|env| VALID_ENVIRONMENTS.contains(env))
        .unwrap_or("dev")
        .to_string();
    let runtime = match get(merged, "runtime") {
        Some(Value::Dict(_)) => get(merged, "runtime").cloned(),
        _ => None,
    };

    CircuitryConfig {
        default_model,
        default_adapter,
        plugins,
        enabled_adapters: normalize_allowlist(get(merged, "enabled_adapters"), true),
        enabled_plugins: normalize_allowlist(get(merged, "enabled_plugins"), false),
        enabled_tools: normalize_allowlist(get(merged, "enabled_tools"), true),
        environment,
        runtime,
    }
}

/// pathlib's `str(Path(given))`: drops a `.` component, collapses
/// repeated `/`, strips a trailing `/` -- every [`ConfigError`] message
/// interpolates the path exactly the way Python's own f-string does
/// (`str` on a `pathlib.Path`), not the raw CLI argument text.
fn pathlib_str(path: &Path) -> String {
    use std::path::Component;
    let mut is_absolute = false;
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir => is_absolute = true,
            Component::CurDir => {}
            Component::ParentDir => parts.push("..".to_string()),
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::Prefix(prefix) => {
                parts.push(prefix.as_os_str().to_string_lossy().into_owned())
            }
        }
    }
    if parts.is_empty() {
        return ".".to_string();
    }
    let joined = parts.join("/");
    if is_absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// `exc.lineno`/`exc.colno` for a `json.JSONDecodeError` at char offset
/// *pos* into *text* (`doc.count('\n', 0, pos) + 1`,
/// `pos - doc.rfind('\n', 0, pos)`) -- *pos* is a Unicode-scalar index
/// (matching [`electricity_json::ReadError::Syntax`]'s own `pos`), so
/// this walks `char`s, not bytes.
fn line_col(text: &str, pos: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    let upto = &chars[..pos.min(chars.len())];
    let lineno = upto.iter().filter(|&&c| c == '\n').count() + 1;
    match upto.iter().rposition(|&c| c == '\n') {
        Some(idx) => (lineno, pos - idx),
        None => (lineno, pos + 1),
    }
}

fn json_root_type_name(value: &Value) -> &'static str {
    match value {
        Value::List(_) => "an array",
        Value::Str(_) => "a string",
        Value::Bool(_) => "a boolean",
        Value::Int(_) | Value::Float(_) => "a number",
        Value::None => "null",
        _ => "a non-object value",
    }
}

/// `cli/config.py::resolve_config`'s explicit-path branch: reads
/// *explicit_path* (every [`ConfigError`] text `_load_json_file`/
/// `read_config_bytes`/`parse_config_bytes` raise for it, in the same
/// order Python's own ladder checks them), deep-merges it over
/// [`sane_defaults`], then overlays *env*'s `CIRCUITRY_*` variables
/// (`_apply_env_vars`). *env* is taken explicitly rather than read from
/// the process environment, so a test (and electricity-cli's own call
/// site) controls it exactly.
pub fn resolve_config(
    explicit_path: &Path,
    env: &HashMap<String, String>,
) -> Result<CircuitryConfig, ConfigError> {
    let display_path = pathlib_str(explicit_path);
    let bytes = std::fs::read(explicit_path).map_err(|err| read_error(&display_path, &err))?;
    let text = String::from_utf8(bytes).map_err(|_| {
        ConfigError(format!(
            "Config file is not valid UTF-8 text: {display_path}"
        ))
    })?;
    let raw = electricity_json::loads(&text).map_err(|err| match err {
        ReadError::Syntax { message, pos } => {
            let (lineno, colno) = line_col(&text, pos);
            ConfigError(format!(
                "Config file {display_path} is not valid JSON: {message} (line {lineno}, column {colno})"
            ))
        }
        ReadError::DuplicateKey { message } => ConfigError(message),
        ReadError::Depth { pos } => {
            let (lineno, colno) = line_col(&text, pos);
            ConfigError(format!(
                "Config file {display_path} is not valid JSON: exceeded the maximum nesting depth (line {lineno}, column {colno})"
            ))
        }
    })?;
    if !matches!(raw, Value::Dict(_)) {
        let found = json_root_type_name(&raw);
        return Err(ConfigError(format!(
            "Config file {display_path} must contain a JSON object at the root; found {found}."
        )));
    }
    // `raw` implements `Drop`, so its `Dict` field can't be moved out by
    // destructuring (`match raw { Value::Dict(dict) => dict, ... }`) --
    // only read through a borrow, then cloned.
    let file_dict = raw.as_dict().expect("checked above").clone();

    let merged = deep_merge(&sane_defaults(), &file_dict);
    let merged = apply_env_vars(merged, env);
    Ok(config_from_dict(&merged))
}

fn read_error(display_path: &str, err: &io::Error) -> ConfigError {
    match err.kind() {
        io::ErrorKind::NotFound => ConfigError(format!("Config file not found: {display_path}")),
        io::ErrorKind::IsADirectory => ConfigError(format!(
            "Config path is a directory, not a file: {display_path}"
        )),
        io::ErrorKind::PermissionDenied => ConfigError(format!(
            "Config file could not be read: {display_path} (Permission denied)"
        )),
        _ => ConfigError(format!(
            "Config file could not be read: {display_path} ({})",
            strerror(err)
        )),
    }
}

/// `exc.strerror`: the bare OS error message, without Rust's own
/// `io::Error::to_string()` " (os error N)" suffix.
fn strerror(err: &io::Error) -> String {
    let text = err.to_string();
    match text.rfind(" (os error ") {
        Some(idx) if text.ends_with(')') => text[..idx].to_string(),
        _ => text,
    }
}

/// `cli/config.py::_apply_env_vars`, in the same order
/// `CONFIG_ENV_VARS` lists them.
fn apply_env_vars(merged: Dict, env: &HashMap<String, String>) -> Dict {
    let mut result = merged;

    if let Some(model) = non_empty(env, "CIRCUITRY_MODEL") {
        result.insert(
            Value::Str("default_model".to_string()),
            Value::Str(model.clone()),
        );
    }

    if let Some(adapter) = non_empty(env, "CIRCUITRY_ADAPTER") {
        result.insert(
            Value::Str("default_adapter".to_string()),
            Value::Str(adapter.clone()),
        );
    }

    if let Some(url) = non_empty(env, "CIRCUITRY_ADAPTER_URL") {
        let adapter_name = get(&result, "default_adapter")
            .and_then(Value::as_str)
            .filter(|a| !a.is_empty())
            .unwrap_or("ollama")
            .to_string();
        set_nested_base_url(&mut result, "adapters", &adapter_name, url);
    }

    if let Some(url) = non_empty(env, "CIRCUITRY_COMFYUI_URL") {
        set_nested_base_url(&mut result, "plugins", "comfyui", url);
    }

    for (env_key, cfg_key) in [
        ("CIRCUITRY_ENABLED_ADAPTERS", "enabled_adapters"),
        ("CIRCUITRY_ENABLED_PLUGINS", "enabled_plugins"),
        ("CIRCUITRY_ENABLED_TOOLS", "enabled_tools"),
    ] {
        if let Some(raw) = env.get(env_key) {
            let items: Vec<Value> = raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Value::Str(s.to_string()))
                .collect();
            result.insert(Value::Str(cfg_key.to_string()), Value::List(items));
        }
    }

    if let Some(environment) = non_empty(env, "CIRCUITRY_ENVIRONMENT") {
        result.insert(
            Value::Str("environment".to_string()),
            Value::Str(environment.clone()),
        );
    }

    result
}

fn non_empty<'a>(env: &'a HashMap<String, String>, key: &str) -> Option<&'a String> {
    env.get(key).filter(|v| !v.is_empty())
}

/// Sets `runtime.<block>.<name>.base_url`, creating any missing object
/// along the way -- `CIRCUITRY_ADAPTER_URL`/`CIRCUITRY_COMFYUI_URL`'s
/// own write (`_apply_env_vars`).
fn set_nested_base_url(result: &mut Dict, block: &str, name: &str, url: &str) {
    let mut runtime = match result.get(&Value::Str("runtime".to_string())) {
        Some(Value::Dict(d)) => d.clone(),
        _ => Dict::new(),
    };
    let mut block_dict = match runtime.get(&Value::Str(block.to_string())) {
        Some(Value::Dict(d)) => d.clone(),
        _ => Dict::new(),
    };
    let mut name_dict = match block_dict.get(&Value::Str(name.to_string())) {
        Some(Value::Dict(d)) => d.clone(),
        _ => Dict::new(),
    };
    name_dict.insert(
        Value::Str("base_url".to_string()),
        Value::Str(url.to_string()),
    );
    block_dict.insert(Value::Str(name.to_string()), Value::Dict(name_dict));
    runtime.insert(Value::Str(block.to_string()), Value::Dict(block_dict));
    result.insert(Value::Str("runtime".to_string()), Value::Dict(runtime));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A fresh, uniquely named scratch directory under the OS temp dir
    /// (no `tempfile` dependency anywhere else in this workspace --
    /// every other crate's own tests follow this same convention).
    /// Removed again at the end of the test that created it.
    struct ScratchDir(std::path::PathBuf);

    impl ScratchDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "electricity-config-test-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            fs::create_dir_all(&dir).unwrap();
            ScratchDir(dir)
        }

        fn write_config(&self, contents: &str) -> std::path::PathBuf {
            let path = self.0.join("config.json");
            fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_file_reports_not_found() {
        let err = resolve_config(Path::new("/no/such/config.json"), &HashMap::new()).unwrap_err();
        assert_eq!(
            err.0,
            "Config file not found: /no/such/config.json".to_string()
        );
    }

    #[test]
    fn a_directory_is_reported_as_such() {
        let scratch = ScratchDir::new("is-a-directory");
        let err = resolve_config(&scratch.0, &HashMap::new()).unwrap_err();
        assert!(
            err.0
                .starts_with("Config path is a directory, not a file: ")
        );
    }

    #[test]
    fn invalid_json_reports_line_and_column() {
        let scratch = ScratchDir::new("invalid-json");
        let path = scratch.write_config("{\n  \"a\": ,\n}\n");
        let err = resolve_config(&path, &HashMap::new()).unwrap_err();
        assert!(err.0.contains("is not valid JSON"));
        assert!(err.0.contains("line 2, column"));
    }

    #[test]
    fn a_non_object_root_names_its_type() {
        let scratch = ScratchDir::new("non-object-root");
        let path = scratch.write_config("[1, 2, 3]");
        let err = resolve_config(&path, &HashMap::new()).unwrap_err();
        assert!(err.0.contains("found an array."));
    }

    #[test]
    fn defaults_only_match_sane_defaults() {
        let scratch = ScratchDir::new("defaults-only");
        let path = scratch.write_config("{}");
        let cfg = resolve_config(&path, &HashMap::new()).unwrap();
        assert_eq!(cfg.default_model, Some("llama3.1:8b".to_string()));
        assert_eq!(cfg.default_adapter, Some("ollama".to_string()));
        assert_eq!(cfg.environment, "dev");
        assert_eq!(cfg.enabled_adapters, None);
    }

    #[test]
    fn a_user_file_deep_merges_over_the_defaults() {
        let scratch = ScratchDir::new("deep-merge");
        let path = scratch.write_config(
            r#"{"default_model": "gpt-4", "runtime": {"adapters": {"openai": {"base_url": "https://api.openai.com"}}}}"#,
        );
        let cfg = resolve_config(&path, &HashMap::new()).unwrap();
        assert_eq!(cfg.default_model, Some("gpt-4".to_string()));
        let runtime = cfg.runtime.unwrap();
        let adapters = get(runtime.as_dict().unwrap(), "adapters")
            .unwrap()
            .as_dict()
            .unwrap();
        // The default `ollama` adapter survives the merge (deep, not a
        // top-level replace) alongside the user's own `openai` entry.
        assert!(adapters.contains_key(&Value::Str("ollama".to_string())));
        assert!(adapters.contains_key(&Value::Str("openai".to_string())));
    }

    #[test]
    fn env_overlays_win_over_the_file() {
        let scratch = ScratchDir::new("env-overlays");
        let path = scratch.write_config(r#"{"default_model": "gpt-4"}"#);
        let mut env = HashMap::new();
        env.insert("CIRCUITRY_MODEL".to_string(), "llama3.3".to_string());
        env.insert(
            "CIRCUITRY_ENABLED_TOOLS".to_string(),
            "json, shell".to_string(),
        );
        let cfg = resolve_config(&path, &env).unwrap();
        assert_eq!(cfg.default_model, Some("llama3.3".to_string()));
        assert_eq!(
            cfg.enabled_tools,
            Some(vec!["json".to_string(), "shell".to_string()])
        );
    }

    #[test]
    fn circuitry_adapter_url_targets_the_resolved_default_adapter() {
        let scratch = ScratchDir::new("adapter-url");
        let path = scratch.write_config(r#"{"default_adapter": "openai"}"#);
        let mut env = HashMap::new();
        env.insert(
            "CIRCUITRY_ADAPTER_URL".to_string(),
            "https://example.test".to_string(),
        );
        let cfg = resolve_config(&path, &env).unwrap();
        let runtime = cfg.runtime.unwrap();
        let adapters = get(runtime.as_dict().unwrap(), "adapters")
            .unwrap()
            .as_dict()
            .unwrap();
        let openai = get(adapters, "openai").unwrap().as_dict().unwrap();
        assert_eq!(
            get(openai, "base_url"),
            Some(&Value::Str("https://example.test".to_string()))
        );
    }

    #[test]
    fn an_unknown_environment_falls_back_to_dev() {
        let scratch = ScratchDir::new("unknown-environment");
        let path = scratch.write_config(r#"{"environment": "staging"}"#);
        let cfg = resolve_config(&path, &HashMap::new()).unwrap();
        assert_eq!(cfg.environment, "dev");
    }
}
