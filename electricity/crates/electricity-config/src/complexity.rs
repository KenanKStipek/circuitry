//! `cli/complexity_config.py`'s own `runtime.complexity` validation --
//! the block's shape and defaults only (what [`resolve_complexity_settings`]
//! needs to raise the identical [`ConfigError`] text Circuitry's own
//! `ComplexityConfigError` would for a malformed block, and what
//! [`crate::effective_settings`] needs to record `sources` for). Scoring,
//! routing and decomposition *behaviour* -- picking a model, splitting an
//! effect -- are out of scope for M0-H (no prompt effects run at all), so
//! only [`RoutingSettings`]'s own shape is kept beyond validation: issue
//! #431's lane D still has to decide whether a run's `model` was pinned
//! above the router or resolved from its catch-all band
//! (`_apply_router_precedence`), which [`crate::effective_settings`] needs.

use crate::config::ConfigError;
use crate::util::{as_dict, get, py_format_g, type_name};
use electricity_value::{Dict, Value};

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

const DEFAULT_THRESHOLD: f64 = 80.0;
const DEFAULT_MAX_DEPTH: i64 = 2;
const DEFAULT_MAX_CHUNKS: i64 = 8;
const DEFAULT_ON_FAILURE: &str = "route_up";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScoringSettings {
    pub enabled: bool,
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

fn as_non_empty_str(value: &Value, path: &str) -> Result<String, ConfigError> {
    match value.as_str().map(str::trim) {
        Some(s) if !s.is_empty() => Ok(s.to_string()),
        _ => Err(ConfigError(format!(
            "{path} must be a non-empty string; found {}.",
            type_name(value)
        ))),
    }
}

/// `scoring.weights` -- validated for its own sake
/// ([`ScoringSettings`] does not carry the resolved weights: M0-H never
/// scores a prompt, so nothing downstream reads them), exactly the way
/// `_parse_scoring` validates them before building a `ScoringSettings`
/// Circuitry's own callers *do* read.
fn validate_weights(raw: &Value, path: &str) -> Result<(), ConfigError> {
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
        as_number(value, &format!("{path}.{name}"))?;
    }
    Ok(())
}

/// `scoring.keywords` -- validated for its own sake, same rationale as
/// [`validate_weights`]. Unlike weights, any non-empty string key is
/// accepted (there is no fixed vocabulary); `as_mapping` already
/// requires a string key, so the only further check per entry is that
/// it is non-empty.
fn validate_keywords(raw: &Value, path: &str) -> Result<(), ConfigError> {
    let block = as_mapping(raw, path)?;
    for (name, value) in block {
        let name = name.as_str().expect("as_mapping guarantees string keys");
        as_non_empty_str(&Value::Str(name.to_string()), &format!("{path} key"))?;
        as_number(value, &format!("{path}.{name}"))?;
    }
    Ok(())
}

fn parse_scoring(raw: &Value, path: &str) -> Result<ScoringSettings, ConfigError> {
    let block = as_mapping(raw, path)?;
    reject_unknown_keys(block, &SCORING_KEYS, path)?;

    if let Some(weights) = get(block, "weights").filter(|v| !matches!(v, Value::None)) {
        validate_weights(weights, &format!("{path}.weights"))?;
    }
    if let Some(keywords) = get(block, "keywords").filter(|v| !matches!(v, Value::None)) {
        validate_keywords(keywords, &format!("{path}.keywords"))?;
    }

    Ok(ScoringSettings {
        enabled: as_bool(block, "enabled", path, false)?,
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
}
