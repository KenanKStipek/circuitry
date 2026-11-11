//! `cli/config.py`'s own config-resolution port. [`resolve_config`] is
//! `resolve_config`'s explicit-path branch (`--config <path>` on
//! electricity's own legacy `electricity <config.json> <doc>` form,
//! where the positional *is* that explicit path, with no
//! `CIRCUITRY_CONFIG` of its own to layer in) -- `SANE_DEFAULTS` deep-
//! merged with the named file, then the `CIRCUITRY_*` environment
//! overlays (`_apply_env_vars`).
//!
//! [`resolve_config_for_run`] is the `run` subcommand's own entry
//! point (issue #728): when `--config` is absent, it ports
//! `resolve_config`'s *other* branch -- global config, then a
//! discovered project config (`circuitry.config.json`/`config.json` in
//! the working directory) gated by [`crate::config_trust`]'s trust
//! check and narrowed against the global allowlists, then the same
//! `CIRCUITRY_*` overlays. `ConfigSource`/`sources` reporting (`cof
//! config`'s own `Config:` line) has no electricity surface and is not
//! ported; a discovered-but-skipped project config's one warning
//! (`ProjectConfigStatus.skip_warning`) is, since it lands in a run's
//! own `warnings` on both engines.

use crate::config_trust::{self, TrustState};
use crate::util::{get, is_truthy};
use electricity_json::ReadError;
use electricity_value::{Dict, Value};
use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

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

/// `CircuitryConfig.from_dict`'s own `environment` field: a truthy value
/// (Python's `d.get("environment") or "dev"`) not in
/// [`VALID_ENVIRONMENTS`] falls back to `"dev"`, logging
/// `cli/config.py::CircuitryConfig.from_dict`'s own
/// `logger.warning("Unknown environment %r; falling back to 'dev'. Valid: "
/// "%s", env, _VALID_ENVIRONMENTS)` word for word -- `%s` on the fixed
/// `_VALID_ENVIRONMENTS` tuple is always this same literal text, and `%r`
/// on *raw* (which can be any JSON type, not just a string) is its own
/// Python `repr`, via [`Value::py_repr`]. A falsy raw value (`None`,
/// `""`, `0`, ...) never warns, same as Python's `or "dev"` never even
/// reaching the `not in` check for one.
fn resolve_environment(raw: Option<&Value>) -> String {
    let is_valid_str = raw
        .and_then(Value::as_str)
        .is_some_and(|env| VALID_ENVIRONMENTS.contains(&env));
    if is_valid_str {
        return raw
            .and_then(Value::as_str)
            .expect("checked above")
            .to_string();
    }
    if is_truthy(raw) {
        log::warn!(
            "Unknown environment {}; falling back to 'dev'. Valid: ('dev', 'prod', 'test')",
            raw.expect("is_truthy(None) is false").py_repr()
        );
    }
    "dev".to_string()
}

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
    let environment = resolve_environment(get(merged, "environment"));
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
    let file_dict = load_config_file(explicit_path, None)?;
    let merged = deep_merge(&sane_defaults(), &file_dict);
    let merged = apply_env_vars(merged, env);
    Ok(config_from_dict(&merged))
}

/// `cli/config.py::_load_json_file`: reads *path* as a JSON object,
/// every message tagged for *env_var* the way `_config_error_message`
/// tags it -- `Some("CIRCUITRY_CONFIG")` for that env var's own
/// resolution branch, `None` everywhere else (an explicit `--config`
/// path, or a file discovered rather than named).
fn load_config_file(path: &Path, env_var: Option<&str>) -> Result<Dict, ConfigError> {
    let display_path = pathlib_str(path);
    let bytes = std::fs::read(path).map_err(|err| read_error(&display_path, &err, env_var))?;
    let text = String::from_utf8(bytes).map_err(|_| {
        config_error_message(
            &display_path,
            env_var,
            "is not valid UTF-8 text",
            format!("Config file is not valid UTF-8 text: {display_path}"),
        )
    })?;
    let raw = electricity_json::loads(&text).map_err(|err| match err {
        ReadError::Syntax {
            message,
            lineno,
            colno,
            ..
        } => config_error_message(
            &display_path,
            env_var,
            &format!("is not valid JSON: {message} (line {lineno}, column {colno})"),
            format!(
                "Config file {display_path} is not valid JSON: {message} (line {lineno}, column {colno})"
            ),
        ),
        ReadError::DuplicateKey { message } => ConfigError(message),
        ReadError::Depth { pos } => {
            let (lineno, colno) = line_col(&text, pos);
            config_error_message(
                &display_path,
                env_var,
                &format!("is not valid JSON: exceeded the maximum nesting depth (line {lineno}, column {colno})"),
                format!(
                    "Config file {display_path} is not valid JSON: exceeded the maximum nesting depth (line {lineno}, column {colno})"
                ),
            )
        }
    })?;
    if !matches!(raw, Value::Dict(_)) {
        let found = json_root_type_name(&raw);
        return Err(config_error_message(
            &display_path,
            env_var,
            &format!("must contain a JSON object at the root; found {found}"),
            format!("Config file {display_path} must contain a JSON object at the root; found {found}."),
        ));
    }
    // `raw` implements `Drop`, so its `Dict` field can't be moved out by
    // destructuring (`match raw { Value::Dict(dict) => dict, ... }`) --
    // only read through a borrow, then cloned.
    Ok(raw.as_dict().expect("checked above").clone())
}

/// `cli/config.py::_config_error_message`: with *env_var*, names the
/// variable instead of just the path it resolved to (so a stale export
/// in a shell profile is diagnosable); without one, *default* verbatim.
fn config_error_message(
    display_path: &str,
    env_var: Option<&str>,
    reason: &str,
    default: String,
) -> ConfigError {
    match env_var {
        Some(var) => ConfigError(format!(
            "{var} points to {display_path}, which {reason}; unset it or fix the path"
        )),
        None => ConfigError(default),
    }
}

fn read_error(display_path: &str, err: &io::Error, env_var: Option<&str>) -> ConfigError {
    match err.kind() {
        io::ErrorKind::NotFound => config_error_message(
            display_path,
            env_var,
            "does not exist",
            format!("Config file not found: {display_path}"),
        ),
        io::ErrorKind::IsADirectory => config_error_message(
            display_path,
            env_var,
            "is a directory, not a file",
            format!("Config path is a directory, not a file: {display_path}"),
        ),
        io::ErrorKind::PermissionDenied => config_error_message(
            display_path,
            env_var,
            "could not be read (Permission denied)",
            format!("Config file could not be read: {display_path} (Permission denied)"),
        ),
        _ => config_error_message(
            display_path,
            env_var,
            &format!("could not be read ({})", strerror(err)),
            format!(
                "Config file could not be read: {display_path} ({})",
                strerror(err)
            ),
        ),
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
/// `CONFIG_ENV_VARS` lists them, except `CIRCUITRY_SYSTEMONE_URL`, which
/// this crate skips on purpose: electricity does not run the `systemone`
/// tool.
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

/// `cli/config.py::DEFAULT_CONFIG_FILENAMES`.
const DEFAULT_CONFIG_FILENAMES: [&str; 2] = ["circuitry.config.json", "config.json"];

/// `cli/config.py::GLOBAL_CONFIG_PATH`: `<home>/.config/circuitry/config.json`.
pub fn global_config_path(home: &Path) -> PathBuf {
    home.join(".config").join("circuitry").join("config.json")
}

/// `cli/config_trust.py::trust_store_path`: `trusted.json` beside the
/// global config.
pub fn trust_store_path(home: &Path) -> PathBuf {
    global_config_path(home)
        .parent()
        .expect("global_config_path always has a parent")
        .join(config_trust::TRUST_STORE_FILENAME)
}

/// `cli/config.py::discover_project_config`: the first of
/// [`DEFAULT_CONFIG_FILENAMES`] that exists as a file directly in
/// *cwd*.
pub fn discover_project_config(cwd: &Path) -> Option<PathBuf> {
    DEFAULT_CONFIG_FILENAMES
        .iter()
        .map(|name| cwd.join(name))
        .find(|path| path.is_file())
}

/// `cli/config.py::_ALLOWLIST_KEYS`.
const ALLOWLIST_KEYS: [&str; 3] = ["enabled_adapters", "enabled_plugins", "enabled_tools"];

/// `cli/config.py::_narrow_allowlists`: a discovered project config may
/// only narrow an allowlist the global config set, never widen it.
/// *project_path_display* is the discovered file's own `str(Path(...))`
/// text, for the warning.
fn narrow_allowlists(
    merged: &mut Dict,
    global_config: &Dict,
    project_config: &Dict,
    project_path_display: &str,
) {
    for key in ALLOWLIST_KEYS {
        let lowercase = key != "enabled_plugins";
        let Some(global_list) = normalize_allowlist(get(global_config, key), lowercase) else {
            continue;
        };
        if !project_config.contains_key(&Value::Str(key.to_string())) {
            continue;
        }
        let project_list = normalize_allowlist(get(project_config, key), lowercase);
        match project_list {
            None => {
                log::warn!(
                    "Project config {project_path_display} cannot re-open {key}; keeping the \
                     global list {global_list:?}."
                );
                merged.insert(
                    Value::Str(key.to_string()),
                    Value::List(global_list.into_iter().map(Value::Str).collect()),
                );
            }
            Some(project_list) => {
                let widened: Vec<&String> = project_list
                    .iter()
                    .filter(|name| !global_list.contains(name))
                    .collect();
                if !widened.is_empty() {
                    log::warn!(
                        "Project config {project_path_display} cannot widen {key} beyond the \
                         global list; ignoring {widened:?}."
                    );
                }
                let narrowed: Vec<Value> = project_list
                    .into_iter()
                    .filter(|name| global_list.contains(name))
                    .map(Value::Str)
                    .collect();
                merged.insert(Value::Str(key.to_string()), Value::List(narrowed));
            }
        }
    }
}

/// [`resolve_config`]'s own result, plus the warnings a run reports
/// alongside it (`CircuitryConfig.resolution_warnings`'s own electricity
/// surface: just the one skipped-project-config line, since there is no
/// `ConfigSource`/`sources` reporting here at all -- this module's own
/// doc comment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigForRun {
    pub config: CircuitryConfig,
    pub warnings: Vec<String>,
}

/// `cli/config.py::resolve_config`, in full: the `run` subcommand's own
/// config resolution (issue #728) when `--config` is optional, unlike
/// [`resolve_config`]'s single explicit-path branch.
///
/// Order: *explicit_path* (`--config`) > `CIRCUITRY_CONFIG` (in *env*) >
/// global config (`<home>/.config/circuitry/config.json`) layered with a
/// discovered, trusted project config (`circuitry.config.json`/
/// `config.json` in *cwd*) > built-in defaults alone. An explicit path or
/// `CIRCUITRY_CONFIG` *replaces* the global/project layers rather than
/// layering on top of them, exactly as `resolve_config` does; a read/parse
/// problem in the *discovered* global or project file is non-fatal (a
/// `log::warn!`, same as the reference's own `logger.warning`, and that
/// layer is simply skipped) where the same problem in an explicit path or
/// `CIRCUITRY_CONFIG` is a fatal [`ConfigError`] (`load_config_file`'s own
/// env_var-tagged message when it's `CIRCUITRY_CONFIG`).
///
/// `home` is `None` when the process has no resolvable home directory at
/// all (`std::env::home_dir()` returning `None`) -- then there is no
/// global config and no trust store to consult, same as a `cof` process
/// in that state would find neither `Path.home()` branch either.
pub fn resolve_config_for_run(
    explicit_path: Option<&Path>,
    cwd: &Path,
    home: Option<&Path>,
    env: &HashMap<String, String>,
) -> Result<ConfigForRun, ConfigError> {
    let mut warnings = Vec::new();
    let env_config = non_empty(env, "CIRCUITRY_CONFIG");

    let merged = if let Some(path) = explicit_path {
        let file_dict = load_config_file(path, None)?;
        deep_merge(&sane_defaults(), &file_dict)
    } else if let Some(env_path) = env_config {
        let file_dict = load_config_file(Path::new(env_path), Some("CIRCUITRY_CONFIG"))?;
        deep_merge(&sane_defaults(), &file_dict)
    } else {
        let mut merged = sane_defaults();
        let mut global_config = Dict::new();
        if let Some(home) = home {
            let global_path = global_config_path(home);
            if global_path.is_file() {
                match load_config_file(&global_path, None) {
                    Ok(dict) => {
                        global_config = dict.clone();
                        merged = deep_merge(&merged, &dict);
                    }
                    Err(err) => log::warn!(
                        "Skipping malformed global config {}: {}",
                        global_path.display(),
                        err.0
                    ),
                }
            }
        }

        if let Some(discovered) = discover_project_config(cwd) {
            match std::fs::read(&discovered) {
                Ok(data) => {
                    let store_path = home
                        .map(trust_store_path)
                        .unwrap_or_else(|| discovered.clone());
                    let state = if home.is_some() {
                        config_trust::check_trust(&discovered, &data, &store_path, env)
                    } else {
                        TrustState::Untrusted
                    };
                    if state.applied() {
                        match load_config_file(&discovered, None) {
                            Ok(project_dict) => {
                                merged = deep_merge(&merged, &project_dict);
                                narrow_allowlists(
                                    &mut merged,
                                    &global_config,
                                    &project_dict,
                                    &pathlib_str(&discovered),
                                );
                            }
                            Err(err) => log::warn!(
                                "Skipping malformed project config {}: {}",
                                discovered.display(),
                                err.0
                            ),
                        }
                    } else if let Some(warning) = config_trust::skip_warning(&discovered, state) {
                        warnings.push(warning);
                    }
                }
                Err(_) => {
                    // Unreadable: treated the same as untrusted, same
                    // degrade `find_config_path` applies (`check_trust`
                    // itself never raises -- it only ever returns a
                    // `TrustState`) -- nothing to report, since the
                    // reference's own malformed-file warning never fires
                    // for an unreadable file either (`read_config_bytes`
                    // raising is a *fatal* `ConfigError` only on an
                    // explicit path, never reached for a discovered one
                    // here).
                }
            }
        }
        merged
    };

    let merged = apply_env_vars(merged, env);
    Ok(ConfigForRun {
        config: config_from_dict(&merged),
        warnings,
    })
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
