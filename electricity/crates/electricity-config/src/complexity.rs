//! `cli/complexity_config.py`'s own `runtime.complexity` validation --
//! the block's shape and defaults, plus (since issue #505's K1 lane)
//! the *resolved* `scoring.weights`/`keywords` tables
//! `electricity-complexity::runtime_tables` reads directly. Scoring,
//! routing and decomposition *behaviour* -- picking a model, splitting
//! an effect -- stay out of this crate itself: `RoutingSettings`'s own
//! shape (`band_for`/`band_named`) and `ScoringSettings`'s own
//! `weights`/`keywords` are read by `electricity-complexity` and
//! `electricity-vm` (K2), never consulted here.

use crate::config::ConfigError;
use crate::util::{as_dict, get, py_format_g, type_name};
use electricity_value::pycompat::py_strip;
use electricity_value::{Dict, Value};
use indexmap::IndexMap;

pub const SCORE_MIN: f64 = 0.0;
pub const SCORE_MAX: f64 = 100.0;

const COMPLEXITY_KEYS: [&str; 3] = ["scoring", "routing", "decomposition"];
const SCORING_KEYS: [&str; 3] = ["enabled", "weights", "keywords"];
const ROUTING_KEYS: [&str; 3] = ["enabled", "bands", "respect_explicit"];
const DECOMPOSITION_KEYS: [&str; 5] = [
    "enabled",
    "threshold",
    "max_depth",
    "max_chunks",
    "on_failure",
];
const BAND_KEYS: [&str; 3] = ["max", "model", "name"];
const ON_FAILURE_CHOICES: [&str; 2] = ["route_up", "fail"];

/// `core/complexity.py::SIGNAL_NAMES` -- the only names
/// `scoring.weights` may set (`cli/complexity_config.py`'s own
/// `_DEFAULT_WEIGHT_VALUES`).
const SIGNAL_NAMES: [&str; 7] = [
    "prompt_size",
    "state_references",
    "prompt_type",
    "output_schema",
    "output_size",
    "structural_position",
    "keywords",
];

/// `cli/complexity_config.py::_DEFAULT_WEIGHT_VALUES` -- the config
/// surface's own default weight table (distinct from
/// `core/complexity.py::DEFAULT_WEIGHTS`, the scorer's own 0.28/0.18/...
/// table used only by a direct library call with `weights=None`;
/// `_score_and_route` always passes this config table instead, issue
/// #505's own ruling). `SIGNAL_NAMES` order.
const DEFAULT_WEIGHT_VALUES: [(&str, f64); 7] = [
    ("prompt_size", 1.0),
    ("state_references", 1.5),
    ("prompt_type", 0.75),
    ("output_schema", 1.25),
    ("output_size", 1.0),
    ("structural_position", 0.5),
    ("keywords", 1.0),
];

fn default_weights() -> IndexMap<String, f64> {
    DEFAULT_WEIGHT_VALUES
        .iter()
        .map(|(name, value)| (name.to_string(), *value))
        .collect()
}

const DEFAULT_THRESHOLD: f64 = 80.0;
const DEFAULT_MAX_DEPTH: i64 = 2;
const DEFAULT_MAX_CHUNKS: i64 = 8;
const DEFAULT_ON_FAILURE: &str = "route_up";

#[derive(Debug, Clone, PartialEq)]
pub struct ScoringSettings {
    pub enabled: bool,
    /// Resolved `scoring.weights`: the config table
    /// (`_DEFAULT_WEIGHT_VALUES`) with each configured value overlaid,
    /// `SIGNAL_NAMES` order -- `electricity-complexity`'s own
    /// `runtime_tables` reads this directly (`_score_and_route`'s own
    /// `dict(settings.scoring.weights)`).
    pub weights: IndexMap<String, f64>,
    /// Resolved `scoring.keywords`, keys already `py_strip`ped --
    /// empty when unconfigured, meaning "use the scorer's own default
    /// table" (`runtime_tables` is where that `or None` happens, not
    /// here). A key that collides with an earlier one after stripping
    /// keeps its first position and takes the later value, the same
    /// as a plain Python dict assignment to an existing key -- which
    /// is exactly what `IndexMap::insert` already does.
    pub keywords: IndexMap<String, f64>,
}

impl Default for ScoringSettings {
    fn default() -> Self {
        ScoringSettings {
            enabled: false,
            weights: default_weights(),
            keywords: IndexMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComplexityBand {
    pub model: String,
    pub max: Option<f64>,
    pub name: Option<String>,
}

impl ComplexityBand {
    pub fn is_catch_all(&self) -> bool {
        self.max.is_none()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoutingSettings {
    pub enabled: bool,
    pub bands: Vec<ComplexityBand>,
    pub respect_explicit: bool,
}

impl Default for RoutingSettings {
    fn default() -> Self {
        RoutingSettings {
            enabled: false,
            bands: Vec::new(),
            respect_explicit: true,
        }
    }
}

impl RoutingSettings {
    /// `band_for`: the first band whose inclusive `max` covers *value*,
    /// the catch-all last. `None` only for an empty table.
    pub fn band_for(&self, value: f64) -> Option<&ComplexityBand> {
        self.bands
            .iter()
            .find(|band| band.max.is_none_or(|max| value <= max))
    }

    /// `band_named` (`cli/complexity_config.py`): the band whose own
    /// `name` equals *name*, by direct identifier rather than by score --
    /// a profile's per-effect `routing: <band-name>` pin reads a table
    /// this way. A band with no `name` can never match.
    pub fn band_named(&self, name: &str) -> Option<&ComplexityBand> {
        self.bands
            .iter()
            .find(|band| band.name.as_deref() == Some(name))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecompositionSettings {
    pub enabled: bool,
    pub threshold: f64,
    pub max_depth: i64,
    pub max_chunks: i64,
    pub on_failure: String,
}

impl Default for DecompositionSettings {
    fn default() -> Self {
        DecompositionSettings {
            enabled: false,
            threshold: DEFAULT_THRESHOLD,
            max_depth: DEFAULT_MAX_DEPTH,
            max_chunks: DEFAULT_MAX_CHUNKS,
            on_failure: DEFAULT_ON_FAILURE.to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ComplexitySettings {
    pub scoring: ScoringSettings,
    pub routing: RoutingSettings,
    pub decomposition: DecompositionSettings,
}

impl ComplexitySettings {
    pub fn enabled(&self) -> bool {
        self.scoring.enabled || self.routing.enabled || self.decomposition.enabled
    }
}

fn as_mapping<'a>(value: &'a Value, path: &str) -> Result<&'a Dict, ConfigError> {
    match as_dict(value) {
        Some(dict) => {
            for key in dict.keys() {
                if key.as_str().is_none() {
                    return Err(ConfigError(format!(
                        "{path} keys must be strings; found {}.",
                        key.py_repr()
                    )));
                }
            }
            Ok(dict)
        }
        None => Err(ConfigError(format!(
            "{path} must be an object; found {}.",
            type_name(value)
        ))),
    }
}

fn reject_unknown_keys(block: &Dict, allowed: &[&str], path: &str) -> Result<(), ConfigError> {
    let mut unknown: Vec<&str> = block
        .keys()
        .filter_map(Value::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort_unstable();
    let label = if unknown.len() == 1 { "key" } else { "keys" };
    let listed = unknown
        .iter()
        .map(|k| Value::Str(k.to_string()).py_repr())
        .collect::<Vec<_>>()
        .join(", ");
    let mut valid = allowed.to_vec();
    valid.sort_unstable();
    Err(ConfigError(format!(
        "{path}: unknown {label} {listed}. Valid keys: {}.",
        valid.join(", ")
    )))
}

fn as_bool(block: &Dict, key: &str, path: &str, default: bool) -> Result<bool, ConfigError> {
    match get(block, key) {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(ConfigError(format!(
            "{path}.{key} must be true or false; found {}.",
            type_name(other)
        ))),
    }
}

/// `bool` is an `int` subclass in Python -- a switch value where a
/// number belongs is a mistake worth naming, not silently read as 0/1.
fn as_number(value: &Value, path: &str) -> Result<f64, ConfigError> {
    match value {
        Value::Int(i) => Ok(i.to_f64()),
        Value::Float(f) => Ok(*f),
        other => Err(ConfigError(format!(
            "{path} must be a number; found {}.",
            type_name(other)
        ))),
    }
}

fn as_score(value: &Value, path: &str) -> Result<f64, ConfigError> {
    let number = as_number(value, path)?;
    if !(SCORE_MIN..=SCORE_MAX).contains(&number) {
        return Err(ConfigError(format!(
            "{path} must be between {} and {} (the complexity score range); found {}.",
            py_format_g(SCORE_MIN),
            py_format_g(SCORE_MAX),
            py_format_g(number)
        )));
    }
    Ok(number)
}

fn as_int(value: &Value, path: &str, minimum: i64) -> Result<i64, ConfigError> {
    // `bool` is an `int` subclass in Python, same exclusion as
    // `as_number` -- accepts a JSON/YAML float with no fractional part
    // (`2.0`), like Python's own `isinstance(value, float) and
    // value.is_integer()` fallback.
    let as_whole = match value {
        Value::Bool(_) => None,
        Value::Int(i) => Some(i.to_f64() as i64),
        Value::Float(f) if f.fract() == 0.0 => Some(*f as i64),
        _ => None,
    };
    let Some(number) = as_whole else {
        return Err(ConfigError(format!(
            "{path} must be a whole number; found {}.",
            type_name(value)
        )));
    };
    if number < minimum {
        return Err(ConfigError(format!(
            "{path} must be {minimum} or greater; found {number}."
        )));
    }
    Ok(number)
}

/// `str.strip()`, via [`py_strip`] -- covers `\x1c`-`\x1f`, where
/// `str::trim` does not (issue #505's K1: every call site switches
/// here together, including band `model`/`name` and `on_failure`).
fn as_non_empty_str(value: &Value, path: &str) -> Result<String, ConfigError> {
    match value.as_str().map(py_strip) {
        Some(s) if !s.is_empty() => Ok(s.to_string()),
        _ => Err(ConfigError(format!(
            "{path} must be a non-empty string; found {}.",
            type_name(value)
        ))),
    }
}

/// `scoring.weights`: `cli/complexity_config.py::_parse_scoring`'s own
/// loop -- validates each entry against [`SIGNAL_NAMES`] and overlays
/// it onto *base* (seeded with [`default_weights`] by the caller), in
/// the raw block's own iteration order.
fn resolve_weights(
    raw: &Value,
    path: &str,
    base: &mut IndexMap<String, f64>,
) -> Result<(), ConfigError> {
    let block = as_mapping(raw, path)?;
    for (name, value) in block {
        // `as_mapping` already guarantees every key is a `Value::Str`.
        let name = name.as_str().expect("as_mapping guarantees string keys");
        if !SIGNAL_NAMES.contains(&name) {
            let mut valid = SIGNAL_NAMES.to_vec();
            valid.sort_unstable();
            return Err(ConfigError(format!(
                "{path}: unknown signal {}. Valid signals: {}.",
                Value::Str(name.to_string()).py_repr(),
                valid.join(", ")
            )));
        }
        let number = as_number(value, &format!("{path}.{name}"))?;
        base.insert(name.to_string(), number);
    }
    Ok(())
}

/// `scoring.keywords`: unlike weights, any non-empty string key is
/// accepted (there is no fixed vocabulary). A key that collides with
/// an earlier one after stripping keeps its first position and takes
/// the later value -- `IndexMap::insert`'s own behaviour for an
/// existing key, matching a plain Python dict assignment to one.
fn resolve_keywords(
    raw: &Value,
    path: &str,
    base: &mut IndexMap<String, f64>,
) -> Result<(), ConfigError> {
    let block = as_mapping(raw, path)?;
    for (name, value) in block {
        let name = name.as_str().expect("as_mapping guarantees string keys");
        let keyword = as_non_empty_str(&Value::Str(name.to_string()), &format!("{path} key"))?;
        let number = as_number(value, &format!("{path}.{name}"))?;
        base.insert(keyword, number);
    }
    Ok(())
}

fn parse_scoring(raw: &Value, path: &str) -> Result<ScoringSettings, ConfigError> {
    let block = as_mapping(raw, path)?;
    reject_unknown_keys(block, &SCORING_KEYS, path)?;

    let mut weights = default_weights();
    if let Some(raw_weights) = get(block, "weights").filter(|v| !matches!(v, Value::None)) {
        resolve_weights(raw_weights, &format!("{path}.weights"), &mut weights)?;
    }

    let mut keywords = IndexMap::new();
    if let Some(raw_keywords) = get(block, "keywords").filter(|v| !matches!(v, Value::None)) {
        resolve_keywords(raw_keywords, &format!("{path}.keywords"), &mut keywords)?;
    }

    Ok(ScoringSettings {
        enabled: as_bool(block, "enabled", path, false)?,
        weights,
        keywords,
    })
}

fn parse_band(raw: &Value, path: &str) -> Result<ComplexityBand, ConfigError> {
    let block = as_mapping(raw, path)?;
    reject_unknown_keys(block, &BAND_KEYS, path)?;

    let Some(model_raw) = get(block, "model") else {
        return Err(ConfigError(format!(
            "{path} is missing required field 'model'."
        )));
    };
    let model = as_non_empty_str(model_raw, &format!("{path}.model"))?;

    let max = match get(block, "max") {
        None | Some(Value::None) => None,
        Some(value) => Some(as_score(value, &format!("{path}.max"))?),
    };

    let name = match get(block, "name") {
        None | Some(Value::None) => None,
        Some(value) => Some(as_non_empty_str(value, &format!("{path}.name"))?),
    };

    Ok(ComplexityBand { model, max, name })
}

fn parse_bands(raw: &Value, path: &str) -> Result<Vec<ComplexityBand>, ConfigError> {
    let Value::List(items) = raw else {
        return Err(ConfigError(format!(
            "{path} must be an array of band objects; found {}.",
            type_name(raw)
        )));
    };
    if items.is_empty() {
        return Err(ConfigError(format!(
            "{path} must not be empty — a band table needs at least a catch-all band (one with no 'max')."
        )));
    }

    let bands: Vec<ComplexityBand> = items
        .iter()
        .enumerate()
        .map(|(index, entry)| parse_band(entry, &format!("{path}[{index}]")))
        .collect::<Result<_, _>>()?;

    for (index, band) in bands[..bands.len() - 1].iter().enumerate() {
        if band.is_catch_all() {
            return Err(ConfigError(format!(
                "{path}[{index}] is a catch-all band (no 'max') but is not last; bands after it \
                 can never match. Move it to the end of the table."
            )));
        }
    }

    if !bands[bands.len() - 1].is_catch_all() {
        return Err(ConfigError(format!(
            "{path} must end with a catch-all band — an entry with no 'max' — so every score \
             resolves to a model. Drop 'max' from {path}[{}] or append a new entry.",
            bands.len() - 1
        )));
    }

    let mut previous: Option<f64> = None;
    for (index, band) in bands.iter().enumerate() {
        let Some(max) = band.max else { continue };
        if let Some(previous_max) = previous {
            if max <= previous_max {
                return Err(ConfigError(format!(
                    "{path} must be ordered by ascending 'max' with no overlap: {path}[{index}] \
                     has max {}, which is not greater than the preceding band's max {}.",
                    py_format_g(max),
                    py_format_g(previous_max)
                )));
            }
        }
        previous = Some(max);
    }

    Ok(bands)
}

fn parse_routing(raw: &Value, path: &str) -> Result<RoutingSettings, ConfigError> {
    let block = as_mapping(raw, path)?;
    reject_unknown_keys(block, &ROUTING_KEYS, path)?;

    let enabled = as_bool(block, "enabled", path, false)?;

    let bands = match get(block, "bands") {
        None | Some(Value::None) => {
            if enabled {
                return Err(ConfigError(format!(
                    "{path}.enabled is true but no bands are defined. Add {path}.bands with at \
                     least a catch-all band (one with no 'max')."
                )));
            }
            Vec::new()
        }
        Some(value) => parse_bands(value, &format!("{path}.bands"))?,
    };

    Ok(RoutingSettings {
        enabled,
        bands,
        respect_explicit: as_bool(block, "respect_explicit", path, true)?,
    })
}

fn parse_decomposition(raw: &Value, path: &str) -> Result<DecompositionSettings, ConfigError> {
    let block = as_mapping(raw, path)?;
    reject_unknown_keys(block, &DECOMPOSITION_KEYS, path)?;

    let threshold = match get(block, "threshold") {
        None | Some(Value::None) => DEFAULT_THRESHOLD,
        Some(value) => as_score(value, &format!("{path}.threshold"))?,
    };

    let max_depth = match get(block, "max_depth") {
        None | Some(Value::None) => DEFAULT_MAX_DEPTH,
        Some(value) => as_int(value, &format!("{path}.max_depth"), 0)?,
    };

    let max_chunks = match get(block, "max_chunks") {
        None | Some(Value::None) => DEFAULT_MAX_CHUNKS,
        Some(value) => as_int(value, &format!("{path}.max_chunks"), 1)?,
    };

    let on_failure = match get(block, "on_failure") {
        None | Some(Value::None) => DEFAULT_ON_FAILURE.to_string(),
        Some(value) => {
            let text = as_non_empty_str(value, &format!("{path}.on_failure"))?;
            if !ON_FAILURE_CHOICES.contains(&text.as_str()) {
                return Err(ConfigError(format!(
                    "{path}.on_failure must be one of {}; found {}.",
                    ON_FAILURE_CHOICES.join(", "),
                    Value::Str(text.clone()).py_repr()
                )));
            }
            text
        }
    };

    Ok(DecompositionSettings {
        enabled: as_bool(block, "enabled", path, false)?,
        threshold,
        max_depth,
        max_chunks,
        on_failure,
    })
}

fn check_prerequisites(settings: &ComplexitySettings, path: &str) -> Result<(), ConfigError> {
    if settings.scoring.enabled {
        return Ok(());
    }
    for (name, enabled) in [
        ("routing", settings.routing.enabled),
        ("decomposition", settings.decomposition.enabled),
    ] {
        if enabled {
            let capitalized = {
                let mut chars = name.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            };
            return Err(ConfigError(format!(
                "{path}.{name}.enabled is true but {path}.scoring.enabled is false. \
                 {capitalized} consumes complexity scores, so it requires the scorer — set \
                 {path}.scoring.enabled to true or turn {name} off."
            )));
        }
    }
    Ok(())
}

/// `parse_complexity_settings(raw, path="runtime.complexity")`. `None`
/// (the block absent) resolves to the all-off defaults.
pub fn parse_complexity_settings(raw: Option<&Value>) -> Result<ComplexitySettings, ConfigError> {
    let path = "runtime.complexity";
    let Some(raw) = raw.filter(|v| !matches!(v, Value::None)) else {
        return Ok(ComplexitySettings::default());
    };

    let block = as_mapping(raw, path)?;
    reject_unknown_keys(block, &COMPLEXITY_KEYS, path)?;

    let sub_block = |key: &str| -> Value {
        match get(block, key) {
            None | Some(Value::None) => Value::Dict(Dict::new()),
            Some(value) => value.clone(),
        }
    };

    let settings = ComplexitySettings {
        scoring: parse_scoring(&sub_block("scoring"), &format!("{path}.scoring"))?,
        routing: parse_routing(&sub_block("routing"), &format!("{path}.routing"))?,
        decomposition: parse_decomposition(
            &sub_block("decomposition"),
            &format!("{path}.decomposition"),
        )?,
    };
    check_prerequisites(&settings, path)?;
    Ok(settings)
}

/// `resolve_complexity_settings(runtime)`: *runtime* is the already-
/// merged, effective `runtime:` mapping (not the raw document) --
/// `None`/empty resolves to the all-off defaults, otherwise this reads
/// `runtime["complexity"]` through [`parse_complexity_settings`].
pub fn resolve_complexity_settings(
    runtime: Option<&Value>,
) -> Result<ComplexitySettings, ConfigError> {
    let Some(dict) = runtime.and_then(as_dict).filter(|d| !d.is_empty()) else {
        return Ok(ComplexitySettings::default());
    };
    parse_complexity_settings(get(dict, "complexity"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime_with(complexity: Value) -> Value {
        let mut runtime = Dict::new();
        runtime.insert(Value::Str("complexity".to_string()), complexity);
        Value::Dict(runtime)
    }

    #[test]
    fn absent_runtime_resolves_to_defaults() {
        let settings = resolve_complexity_settings(None).unwrap();
        assert_eq!(settings, ComplexitySettings::default());
    }

    #[test]
    fn routing_enabled_without_bands_is_an_error() {
        let mut routing = Dict::new();
        routing.insert(Value::Str("enabled".to_string()), Value::Bool(true));
        let mut complexity = Dict::new();
        complexity.insert(Value::Str("routing".to_string()), Value::Dict(routing));
        let err =
            resolve_complexity_settings(Some(&runtime_with(Value::Dict(complexity)))).unwrap_err();
        assert!(err.0.contains("no bands are defined"));
    }

    #[test]
    fn routing_without_scoring_is_a_prerequisite_error() {
        let mut routing = Dict::new();
        routing.insert(Value::Str("enabled".to_string()), Value::Bool(true));
        let mut band = Dict::new();
        band.insert(
            Value::Str("model".to_string()),
            Value::Str("gpt-4".to_string()),
        );
        routing.insert(
            Value::Str("bands".to_string()),
            Value::List(vec![Value::Dict(band)]),
        );
        let mut complexity = Dict::new();
        complexity.insert(Value::Str("routing".to_string()), Value::Dict(routing));
        let err =
            resolve_complexity_settings(Some(&runtime_with(Value::Dict(complexity)))).unwrap_err();
        assert!(err.0.contains("requires the scorer"));
    }

    #[test]
    fn a_non_catch_all_last_band_is_rejected() {
        let mut band = Dict::new();
        band.insert(
            Value::Str("model".to_string()),
            Value::Str("gpt-4".to_string()),
        );
        band.insert(Value::Str("max".to_string()), Value::from(10i64));
        let mut scoring = Dict::new();
        scoring.insert(Value::Str("enabled".to_string()), Value::Bool(true));
        let mut routing = Dict::new();
        routing.insert(Value::Str("enabled".to_string()), Value::Bool(true));
        routing.insert(
            Value::Str("bands".to_string()),
            Value::List(vec![Value::Dict(band)]),
        );
        let mut complexity = Dict::new();
        complexity.insert(Value::Str("scoring".to_string()), Value::Dict(scoring));
        complexity.insert(Value::Str("routing".to_string()), Value::Dict(routing));
        let err =
            resolve_complexity_settings(Some(&runtime_with(Value::Dict(complexity)))).unwrap_err();
        assert!(err.0.contains("must end with a catch-all band"));
    }

    #[test]
    fn score_out_of_range_names_the_bound() {
        let mut decomposition = Dict::new();
        decomposition.insert(Value::Str("threshold".to_string()), Value::from(150i64));
        let mut complexity = Dict::new();
        complexity.insert(
            Value::Str("decomposition".to_string()),
            Value::Dict(decomposition),
        );
        let err =
            resolve_complexity_settings(Some(&runtime_with(Value::Dict(complexity)))).unwrap_err();
        assert_eq!(
            err.0,
            "runtime.complexity.decomposition.threshold must be between 0 and 100 (the \
             complexity score range); found 150."
        );
    }

    #[test]
    fn unknown_key_is_rejected() {
        let mut complexity = Dict::new();
        complexity.insert(Value::Str("bogus".to_string()), Value::Bool(true));
        let err =
            resolve_complexity_settings(Some(&runtime_with(Value::Dict(complexity)))).unwrap_err();
        assert!(err.0.contains("unknown key 'bogus'"));
    }

    #[test]
    fn a_valid_full_block_resolves_cleanly() {
        let mut scoring = Dict::new();
        scoring.insert(Value::Str("enabled".to_string()), Value::Bool(true));
        let mut catch_all = Dict::new();
        catch_all.insert(
            Value::Str("model".to_string()),
            Value::Str("gpt-4".to_string()),
        );
        let mut low = Dict::new();
        low.insert(
            Value::Str("model".to_string()),
            Value::Str("gpt-3.5".to_string()),
        );
        low.insert(Value::Str("max".to_string()), Value::from(50i64));
        let mut routing = Dict::new();
        routing.insert(Value::Str("enabled".to_string()), Value::Bool(true));
        routing.insert(
            Value::Str("bands".to_string()),
            Value::List(vec![Value::Dict(low), Value::Dict(catch_all)]),
        );
        let mut complexity = Dict::new();
        complexity.insert(Value::Str("scoring".to_string()), Value::Dict(scoring));
        complexity.insert(Value::Str("routing".to_string()), Value::Dict(routing));
        let settings =
            resolve_complexity_settings(Some(&runtime_with(Value::Dict(complexity)))).unwrap();
        assert!(settings.scoring.enabled);
        assert!(settings.routing.enabled);
        assert_eq!(settings.routing.bands.len(), 2);
        assert_eq!(settings.routing.band_for(10.0).unwrap().model, "gpt-3.5");
        assert_eq!(settings.routing.band_for(99.0).unwrap().model, "gpt-4");
    }

    #[test]
    fn scoring_settings_default_matches_the_config_table() {
        let settings = ScoringSettings::default();
        assert_eq!(settings.weights.get("prompt_size"), Some(&1.0));
        assert_eq!(settings.weights.get("state_references"), Some(&1.5));
        assert_eq!(settings.weights.get("prompt_type"), Some(&0.75));
        assert_eq!(settings.weights.get("output_schema"), Some(&1.25));
        assert_eq!(settings.weights.get("output_size"), Some(&1.0));
        assert_eq!(settings.weights.get("structural_position"), Some(&0.5));
        assert_eq!(settings.weights.get("keywords"), Some(&1.0));
        assert_eq!(settings.weights.len(), 7);
        assert!(settings.keywords.is_empty());
    }

    #[test]
    fn scoring_weights_overlay_a_partial_table_onto_the_defaults() {
        let mut weights = Dict::new();
        weights.insert(Value::Str("prompt_size".to_string()), Value::from(9i64));
        let mut scoring = Dict::new();
        scoring.insert(Value::Str("weights".to_string()), Value::Dict(weights));
        let settings = parse_scoring(&Value::Dict(scoring), "runtime.complexity.scoring").unwrap();
        // `float(value)`: an int overlay becomes a float.
        assert_eq!(settings.weights.get("prompt_size"), Some(&9.0));
        // Every other signal keeps the config's own default.
        assert_eq!(settings.weights.get("state_references"), Some(&1.5));
    }

    #[test]
    fn scoring_keywords_strip_keys_and_a_collision_after_stripping_keeps_first_position_last_value()
    {
        let mut keywords = Dict::new();
        keywords.insert(Value::Str("  analyze  ".to_string()), Value::from(1i64));
        keywords.insert(Value::Str("compare".to_string()), Value::from(2i64));
        keywords.insert(Value::Str("analyze".to_string()), Value::from(3i64));
        let mut scoring = Dict::new();
        scoring.insert(Value::Str("keywords".to_string()), Value::Dict(keywords));
        let settings = parse_scoring(&Value::Dict(scoring), "runtime.complexity.scoring").unwrap();
        assert_eq!(settings.keywords.get("analyze"), Some(&3.0));
        // First position (from "  analyze  ") is kept, not pushed to the
        // end by the later "analyze" insert.
        assert_eq!(
            settings.keywords.keys().collect::<Vec<_>>(),
            vec!["analyze", "compare"]
        );
    }
}
