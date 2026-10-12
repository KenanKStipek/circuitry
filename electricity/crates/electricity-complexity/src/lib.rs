//! A pure-library port of `core/complexity.py` (the scorer) and
//! `core/router.py::route_model` (electricity M1 lane K1, issue #505).
//!
//! Nothing here calls a model, reads config, or touches a store --
//! [`score`] is a pure function of its [`ScoreInput`], deterministic and
//! side-effect-free, exactly like the reference. Wiring this into a real
//! run (`electricity-vm`'s `execute_prompt`, building the `definition`
//! mapping from a `PromptOp`, the display depth, `meta.complexity`
//! placement, `Supported::complexity`) is lane K2's job entirely --
//! nothing in this crate is reachable from a running orchestration yet.
//!
//! # K2's calling contract
//!
//! - **The `definition` mapping.** `ScoreInput::definition` is Python's
//!   mapping form: a [`Value::Dict`] carrying whichever of `template`,
//!   `messages`, `prompt_type`, `schema`, `params` and `description` the
//!   prompt effect declares. K2 builds this from `PromptOp`: the raw
//!   template source or `messages[].content`, `prompt_type`, `schema`,
//!   the **literal** `params` (unevaluated CEL/template text, not a
//!   rendered value), and `description`. A key your caller hasn't
//!   bothered to set can simply be absent from the dict -- [`score`]
//!   treats absent and `Value::None` the same way the reference's own
//!   `_get` does.
//! - **Display depth.** `ScoreInput::structure` is `{"depth":
//!   display_depth}` in the reference's own `_score_and_route` (the
//!   effect's nesting depth as the runtime displays it, not the length
//!   of some internal path); `loop_depth`/`reflector_generated` default
//!   to the top-level case until a later lane plumbs them through.
//! - **`Err` means "no score", never "the effect fails".** [`score`]'s
//!   failure type is [`ScoreError`], carrying a `PyExc` naming which
//!   Python exception class the reference would have raised for the
//!   same input: `PyExc::OverflowError` for a configured number too
//!   large to convert to `f64` (this crate's own `as_number`), or
//!   `PyExc::RuntimeError` for a `fancy-regex` translate/compile/match
//!   failure the reference's own `re` module never hits for a
//!   `re.escape` output (`keywords.rs`'s keyword-pattern path,
//!   `tags.rs`'s state-reference scan). The reference's
//!   `_score_and_route` catches *any* exception from `score()` and
//!   logs a warning rather than letting it interrupt the prompt; K2's
//!   own `score_and_route` must do the same with this crate's `Err`:
//!   record no `meta.complexity`, dispatch anyway.
//!
//! # Documented divergences from CPython (inherited, not introduced here)
//!
//! - **`sum()` on CPython 3.12+.** Every summation in this crate (the
//!   naive weight-total loop, the naive keyword-weight sum) matches
//!   CPython 3.11's left-to-right `+=` `sum()`, the generator's own
//!   pinned interpreter version. CPython 3.12 switched `sum()` to
//!   Neumaier (compensated) summation for floats, which can differ from
//!   a naive sum by one ULP before `round(x, 6)` -- `cof run` on 3.12+
//!   can therefore disagree with this crate by a value `round6` usually
//!   (not always) absorbs. Not reproduced here on purpose (#448
//!   decision 7's own precedent for `electricity-pyre`: pin to the
//!   generator's interpreter version, document the newer one as a gap).
//! - **Unicode lowercasing.** The `keywords` signal's case-folding uses
//!   `str::to_lowercase`, Rust's own full Unicode case mapping (with
//!   Final_Sigma), against whatever Unicode version the toolchain's
//!   `std` ships -- newer than CPython 3.11's Unicode 14.0. A character
//!   whose lowercase mapping changed between 14.0 and Rust's version
//!   would disagree; none of the ones this crate's own corpus and unit
//!   tests pin (Latin, Greek final sigma, Turkish `İ`, `ẞ`) have.

mod keywords;
mod router;
mod schema;
mod tags;

pub use router::{RouteDecision, route_model};

use electricity_config::ScoringSettings;
use electricity_value::pycompat::{PyExc, py_int, py_strip};
use electricity_value::{Dict, Value};
use std::sync::OnceLock;

/// Lowest possible score (`core/complexity.py::MIN_SCORE`).
pub const MIN_SCORE: f64 = 0.0;
/// Highest possible score (`core/complexity.py::MAX_SCORE`).
pub const MAX_SCORE: f64 = 100.0;

/// `type(x).__name__` -- the *unqualified* class name Python's own
/// `__name__` attribute gives, unlike [`Value::type_name`] (shared
/// across crates for other purposes, e.g. a comparison-type error),
/// which spells a date/datetime's own name as `"datetime.date"`/
/// `"datetime.datetime"` -- the module-qualified form `__name__` never
/// produces. Every `score()` "expected X, got {type}" warning reaches
/// here through this, not `Value::type_name` directly.
fn py_type_name(value: &Value) -> &'static str {
    match value {
        Value::Date(_) => "date",
        Value::DateTime(..) => "datetime",
        other => other.type_name(),
    }
}

/// Signals in report order (`core/complexity.py::SIGNAL_NAMES`) -- the
/// only vocabulary a `weights` mapping may name.
pub const SIGNAL_NAMES: [&str; 7] = [
    "prompt_size",
    "state_references",
    "prompt_type",
    "output_schema",
    "output_size",
    "structural_position",
    "keywords",
];

/// The *scorer's* own default weights (`core/complexity.py::
/// DEFAULT_WEIGHTS`), used only by a direct library call with
/// `weights: None`. The runtime always passes `electricity-config`'s
/// own, different, default table instead (`ScoringSettings::default`) --
/// both tables exist on purpose (issue #505's own ruling).
pub const DEFAULT_WEIGHTS: [(&str, f64); 7] = [
    ("prompt_size", 0.28),
    ("state_references", 0.18),
    ("prompt_type", 0.14),
    ("output_schema", 0.16),
    ("output_size", 0.10),
    ("structural_position", 0.08),
    ("keywords", 0.06),
];

/// `core/complexity.py::DEFAULT_KEYWORD_WEIGHTS` verbatim: 23 phrases,
/// table order (iteration order is irrelevant to the result -- matches
/// are sorted before use -- but kept exactly as written for review).
pub const DEFAULT_KEYWORD_WEIGHTS: [(&str, f64); 23] = [
    ("analyze", 0.15),
    ("chain of thought", 0.25),
    ("classify", 0.10),
    ("compare", 0.15),
    ("critique", 0.20),
    ("cross-reference", 0.25),
    ("deduce", 0.25),
    ("derive", 0.20),
    ("evaluate", 0.15),
    ("explain why", 0.20),
    ("extract", 0.15),
    ("infer", 0.20),
    ("justify", 0.20),
    ("plan", 0.20),
    ("prove", 0.30),
    ("reason", 0.20),
    ("reconcile", 0.25),
    ("rewrite", 0.05),
    ("step by step", 0.25),
    ("summarize", 0.10),
    ("synthesize", 0.25),
    ("translate", 0.10),
    ("weigh", 0.15),
];

/// Normalized value per `prompt_type` (`core/complexity.py::
/// _TYPE_NORMALS`). Unknown types fall back to `"text"`.
const TYPE_NORMALS: [(&str, f64); 7] = [
    ("boolean", 0.05),
    ("number", 0.10),
    ("text", 0.25),
    ("tool", 0.40),
    ("json", 0.60),
    ("object", 0.60),
    ("array", 0.75),
];

/// Characters per token (`core/complexity.py::_CHARS_PER_TOKEN`).
const CHARS_PER_TOKEN: u64 = 4;

const SIZE_HALF_TOKENS: f64 = 800.0;
const REFERENCE_HALF_COUNT: f64 = 4.0;
const SCHEMA_DEPTH_HALF: f64 = 3.0;
const SCHEMA_FIELDS_HALF: f64 = 10.0;
const OUTPUT_SIZE_HALF_TOKENS: f64 = 512.0;
const DEPTH_HALF: f64 = 3.0;

/// Tokens assumed per declared output item with no per-item length
/// (`core/complexity.py::_TOKENS_PER_ITEM`).
const TOKENS_PER_ITEM: u64 = 32;

/// `core/complexity.py::ScoreMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreMode {
    Static,
    Rendered,
}

impl ScoreMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ScoreMode::Static => "static",
            ScoreMode::Rendered => "rendered",
        }
    }
}

/// `core/complexity.py::StructureContext`. Every field defaults to the
/// top-level case, so an omitted context equals "a plain effect at the
/// root".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StructureContext {
    pub depth: u64,
    pub loop_depth: u64,
    pub reflector_generated: bool,
}

impl StructureContext {
    pub fn in_loop(&self) -> bool {
        self.loop_depth > 0
    }

    /// `StructureContext.from_any` (`core/complexity.py:281-327`) for a
    /// mapping, `None`, or anything else, plus its warnings.
    ///
    /// Widened to `Result` versus the sketch in issue #505's own public
    /// API table: `_as_number` (which `from_any`'s own `_int` helper
    /// calls, same as every other coercion in this crate) raises
    /// `OverflowError` for an integer too large to convert to `f64`,
    /// and that holds for `structure.depth`/`structure.loop_depth` the
    /// same as it does everywhere else `_as_number` is reached --
    /// confirmed directly against Circuitry's own `score()` (`structure
    /// = {"depth": 10**400}` raises there too). A non-`Result` signature
    /// could only reproduce this by silently not raising, which would be
    /// the actual divergence; this one is documented and literal.
    ///
    /// The same holds for `int(number)` itself, past the `_as_number`
    /// step: a finite, non-negative `depth`/`loop_depth` goes through
    /// [`py_int`] (not a saturating cast), which raises `ValueError` for
    /// `NaN` and `OverflowError` for `+inf` with CPython's own message
    /// text -- `-inf` never reaches it, having already failed the
    /// `number < 0` check just like any other negative value. A finite
    /// float too large for `i64` saturates rather than keeping Python's
    /// exact arbitrary-precision integer -- [`py_int`]'s own documented
    /// divergence, inherited here rather than re-litigated.
    pub fn from_value(
        value: Option<&Value>,
    ) -> Result<(StructureContext, Vec<String>), ScoreError> {
        let Some(value) = value else {
            return Ok((StructureContext::default(), Vec::new()));
        };
        let Value::Dict(dict) = value else {
            return Ok((
                StructureContext::default(),
                vec![format!(
                    "structure: expected a mapping or StructureContext, got {}; treated as top-level.",
                    py_type_name(value)
                )],
            ));
        };

        let mut warnings = Vec::new();
        let mut int_field = |key: &str| -> Result<u64, ScoreError> {
            let raw = dict.get(&Value::Str(key.to_string()));
            let (number, repr_value) = match raw {
                None => (Some(0.0), Value::from(0i64)),
                Some(v) => (as_number(v)?, v.clone()),
            };
            match number {
                Some(n) if n < 0.0 => {
                    warnings.push(format!(
                        "structure.{key}: {} is negative; treated as 0.",
                        repr_value.py_repr()
                    ));
                    Ok(0)
                }
                // Python's own `int(number)` here (not a saturating cast):
                // raises `ValueError`/`OverflowError` for NaN/+inf (`n < 0.0`
                // above already took -inf) rather than silently producing 0
                // or `u64::MAX`. `py_int` is the one shared implementation of
                // that coercion, divergence (a finite float beyond `i64`'s
                // range saturates rather than keeping Python's exact
                // arbitrary-precision integer) and all.
                Some(_) => py_int(&repr_value)
                    .map(|i| i as u64)
                    .map_err(|e| ScoreError {
                        py_class: e.py_class,
                        message: e.message,
                    }),
                None => {
                    let falsy_default = matches!(&repr_value, Value::None)
                        || matches!(&repr_value, Value::Int(i) if i.is_zero())
                        || matches!(&repr_value, Value::Bool(false));
                    if !falsy_default {
                        warnings.push(format!(
                            "structure.{key}: {} is not a number; treated as 0.",
                            repr_value.py_repr()
                        ));
                    }
                    Ok(0)
                }
            }
        };

        // `loop_depth` is coerced before `depth`, so its warnings come first.
        let mut loop_depth = int_field("loop_depth")?;
        if loop_depth == 0 && truthy(dict.get(&Value::Str("in_loop".to_string()))) {
            loop_depth = 1;
        }
        let depth = int_field("depth")?;
        let reflector_generated = truthy(dict.get(&Value::Str("reflector_generated".to_string())));

        Ok((
            StructureContext {
                depth,
                loop_depth,
                reflector_generated,
            },
            warnings,
        ))
    }
}

/// Python truthiness (`bool(value)`), for `get("in_loop", False)` /
/// `get("reflector_generated", False)` -- `None` (key absent) is
/// falsy, same as an explicit falsy value.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::None) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Int(i)) => !i.is_zero(),
        Some(Value::Float(f)) => *f != 0.0,
        Some(Value::Str(s)) => !s.is_empty(),
        Some(Value::List(items)) => !items.is_empty(),
        Some(Value::Dict(d)) => !d.is_empty(),
        Some(Value::Bytes(b)) => !b.is_empty(),
        Some(Value::Date(_)) | Some(Value::DateTime(..)) => true,
    }
}

/// `score()`'s arguments, Python's mapping form (what `cof score`
/// passes, and what K2 builds from a `PromptOp`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ScoreInput<'a> {
    pub definition: Option<&'a Value>,
    pub rendered_prompt: Option<&'a str>,
    pub weights: Option<&'a Value>,
    pub keyword_weights: Option<&'a Value>,
    pub structure: Option<&'a Value>,
}

/// One signal's measurement and its share of the total
/// (`core/complexity.py::SignalScore`, without `detail` -- this port has
/// no `cof score`/lint surface to report it to, issue #505's own "Out
/// of scope").
#[derive(Debug, Clone, PartialEq)]
pub struct SignalScore {
    pub name: &'static str,
    pub raw: f64,
    pub normalized: f64,
    pub weight: f64,
    pub contribution: f64,
    pub estimated: bool,
    pub note: String,
}

/// A score plus the breakdown that produced it
/// (`core/complexity.py::ComplexityScore`).
#[derive(Debug, Clone, PartialEq)]
pub struct ComplexityScore {
    pub score: f64,
    pub max_score: f64,
    pub mode: ScoreMode,
    pub estimated: bool,
    pub signals: Vec<SignalScore>,
    pub weight_total: f64,
    pub warnings: Vec<String>,
}

impl ComplexityScore {
    pub fn signal(&self, name: &str) -> Option<&SignalScore> {
        self.signals.iter().find(|s| s.name == name)
    }

    /// `core/prompt.py::_complexity_meta`: `{score, max_score, mode,
    /// estimated, weight_total, signals, warnings}`. `signals` is a
    /// mapping keyed by signal name (not a list -- a CEL condition
    /// addresses state by path), in [`SIGNAL_NAMES`] order. No `band`
    /// (K2's own addition) and no per-signal `detail`.
    pub fn to_meta(&self) -> Value {
        let mut signals = Dict::new();
        for signal in &self.signals {
            let mut entry = Dict::new();
            entry.insert(Value::Str("raw".to_string()), Value::Float(signal.raw));
            entry.insert(
                Value::Str("normalized".to_string()),
                Value::Float(signal.normalized),
            );
            entry.insert(
                Value::Str("weight".to_string()),
                Value::Float(signal.weight),
            );
            entry.insert(
                Value::Str("contribution".to_string()),
                Value::Float(signal.contribution),
            );
            entry.insert(
                Value::Str("note".to_string()),
                Value::Str(signal.note.clone()),
            );
            signals.insert(Value::Str(signal.name.to_string()), Value::Dict(entry));
        }

        let mut meta = Dict::new();
        meta.insert(Value::Str("score".to_string()), Value::Float(self.score));
        meta.insert(
            Value::Str("max_score".to_string()),
            Value::Float(self.max_score),
        );
        meta.insert(
            Value::Str("mode".to_string()),
            Value::Str(self.mode.as_str().to_string()),
        );
        meta.insert(
            Value::Str("estimated".to_string()),
            Value::Bool(self.estimated),
        );
        meta.insert(
            Value::Str("weight_total".to_string()),
            Value::Float(self.weight_total),
        );
        meta.insert(Value::Str("signals".to_string()), Value::Dict(signals));
        meta.insert(
            Value::Str("warnings".to_string()),
            Value::List(self.warnings.iter().cloned().map(Value::Str).collect()),
        );
        Value::Dict(meta)
    }
}

/// Python raised instead of returning (see this crate's own module
/// docs, "`Err` means no score").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoreError {
    pub py_class: PyExc,
    pub message: String,
}

impl ScoreError {
    fn overflow() -> Self {
        ScoreError {
            py_class: PyExc::OverflowError,
            message: "int too large to convert to float".to_string(),
        }
    }

    fn runtime_error(message: impl Into<String>) -> Self {
        ScoreError {
            py_class: PyExc::RuntimeError,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ScoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// `_as_number` (`core/complexity.py:220`): `bool`/`None` are not
/// numbers; an `int`/`float` coerces to `f64`, Python's `float(value)`
/// -- which raises `OverflowError` for an integer whose magnitude can't
/// be represented, rather than saturating. [`IntValue::to_f64`] *does*
/// saturate to infinity at that boundary (its own doc comment: "never
/// panics"); every integer is finite, so an infinite result here can
/// only mean the true magnitude overflowed, which is exactly the case
/// this function must turn into `Err` instead of a silent `inf`.
pub(crate) fn as_number(value: &Value) -> Result<Option<f64>, ScoreError> {
    match value {
        Value::Bool(_) | Value::None => Ok(None),
        Value::Int(i) => {
            let f = i.to_f64();
            if f.is_infinite() {
                Err(ScoreError::overflow())
            } else {
                Ok(Some(f))
            }
        }
        Value::Float(f) => Ok(Some(*f)),
        _ => Ok(None),
    }
}

/// Python's `min(a, b)`: returns `a` unless `b < a`.
pub(crate) fn py_min(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}

/// Python's `max(a, b)`: returns `a` unless `b > a`.
pub(crate) fn py_max(a: f64, b: f64) -> f64 {
    if b > a { b } else { a }
}

/// `_clamp01` (`core/complexity.py:227`): `0.0` below zero, otherwise
/// [`py_min`] against `1.0` -- `NaN` passes through as `NaN`, where
/// `f64::min` would give `1.0`.
pub(crate) fn clamp01(value: f64) -> f64 {
    if value < 0.0 { 0.0 } else { py_min(value, 1.0) }
}

/// `_saturate` (`core/complexity.py:231`): `value / (value + half)`,
/// `0.0` when either operand is non-positive.
pub(crate) fn saturate(value: f64, half: f64) -> f64 {
    if value <= 0.0 || half <= 0.0 {
        0.0
    } else {
        value / (value + half)
    }
}

/// `round(x, 6)` (`core/complexity.py::_round`).
pub(crate) fn round6(x: f64) -> f64 {
    electricity_value::pycompat::round6(x)
}

/// Python's `{:.0f}` fixed format: identical to Rust's own
/// `format!("{:.0}", x)` except for the lowercase `"nan"` spelling.
pub(crate) fn format_f0(x: f64) -> String {
    if x.is_nan() {
        "nan".to_string()
    } else {
        format!("{x:.0}")
    }
}

fn get_field<'a>(dict: Option<&'a Dict>, key: &str) -> Option<&'a Value> {
    dict.and_then(|d| d.get(&Value::Str(key.to_string())))
        .filter(|v| !matches!(v, Value::None))
}

fn is_scalar(value: &Value) -> bool {
    matches!(
        value,
        Value::Str(_) | Value::Bytes(_) | Value::Int(_) | Value::Float(_) | Value::Bool(_)
    )
}

/// `_message_text` (`core/complexity.py:483-493`).
fn message_text(messages: Option<&Value>) -> Option<String> {
    let Some(Value::List(items)) = messages else {
        return None;
    };
    let mut parts = Vec::new();
    for item in items {
        if let Value::Dict(d) = item {
            if let Some(Value::Str(content)) = get_field(Some(d), "content") {
                if !content.is_empty() {
                    parts.push(content.clone());
                }
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// `_template_text` (`core/complexity.py:496-510`).
fn template_text(dict: Option<&Dict>, warnings: &mut Vec<String>) -> String {
    match get_field(dict, "template") {
        Some(Value::Str(s)) if !s.is_empty() => return s.clone(),
        Some(other @ Value::Str(_)) => {
            let _ = other; // empty template string: falls through, no warning.
        }
        Some(other) => {
            warnings.push(format!(
                "template: expected a string, got {}; ignored.",
                py_type_name(other)
            ));
        }
        None => {}
    }
    if let Some(joined) = message_text(get_field(dict, "messages")) {
        return joined;
    }
    String::new()
}

/// `_schema_of` (`core/complexity.py:734-741`).
fn schema_of<'a>(dict: Option<&'a Dict>, warnings: &mut Vec<String>) -> Option<&'a Dict> {
    let raw = get_field(dict, "schema")?;
    match raw {
        Value::Dict(d) => Some(d),
        other => {
            warnings.push(format!(
                "schema: expected a mapping, got {}; ignored.",
                py_type_name(other)
            ));
            None
        }
    }
}

/// `_prompt_type` (`core/complexity.py:719-731`).
fn prompt_type(dict: Option<&Dict>, warnings: &mut Vec<String>) -> (String, f64) {
    let default_text = Value::Str("text".to_string());
    let raw = get_field(dict, "prompt_type").unwrap_or(&default_text);
    match raw {
        Value::Str(s) => {
            let cleaned_lower = py_strip(s).to_lowercase();
            let cleaned = if cleaned_lower.is_empty() {
                "text".to_string()
            } else {
                cleaned_lower
            };
            match TYPE_NORMALS.iter().find(|(name, _)| *name == cleaned) {
                Some((_, normal)) => (cleaned, *normal),
                None => {
                    warnings.push(format!(
                        "prompt_type: unknown type {}; treated as 'text'.",
                        Value::Str(s.clone()).py_repr()
                    ));
                    (cleaned, text_normal())
                }
            }
        }
        other => {
            warnings.push(format!(
                "prompt_type: expected a string, got {}; treated as 'text'.",
                py_type_name(other)
            ));
            ("text".to_string(), text_normal())
        }
    }
}

fn text_normal() -> f64 {
    TYPE_NORMALS
        .iter()
        .find(|(name, _)| *name == "text")
        .map(|(_, v)| *v)
        .expect("\"text\" is always in TYPE_NORMALS")
}

fn default_keyword_table() -> &'static Dict {
    static TABLE: OnceLock<Dict> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut dict = Dict::new();
        for (phrase, weight) in DEFAULT_KEYWORD_WEIGHTS {
            dict.insert(Value::Str(phrase.to_string()), Value::Float(weight));
        }
        dict
    })
}

/// `_resolve_weights` (`core/complexity.py:669-712`).
fn resolve_weights(
    weights: Option<&Value>,
    warnings: &mut Vec<String>,
) -> Result<Vec<(&'static str, f64)>, ScoreError> {
    let mut resolved: Vec<(&'static str, f64)> = DEFAULT_WEIGHTS.to_vec();
    let Some(weights_value) = weights else {
        return Ok(resolved);
    };
    let Value::Dict(dict) = weights_value else {
        warnings.push(format!(
            "weights: expected a mapping, got {}; used defaults.",
            py_type_name(weights_value)
        ));
        return Ok(resolved);
    };

    let mut keys: Vec<String> = dict.keys().map(Value::py_str).collect();
    keys.sort();
    for key in keys {
        if !SIGNAL_NAMES.contains(&key.as_str()) {
            warnings.push(format!(
                "weights.{key}: unknown signal; ignored. Valid signals: {}.",
                SIGNAL_NAMES.join(", ")
            ));
        }
    }

    for (name, default_value) in resolved.iter_mut() {
        let Some(raw) = dict.get(&Value::Str(name.to_string())) else {
            continue;
        };
        match as_number(raw)? {
            None => {
                warnings.push(format!(
                    "weights.{name}: {} is not a number; used default {}.",
                    raw.py_repr(),
                    Value::Float(*default_value).py_repr()
                ));
            }
            Some(mut value) => {
                if value < 0.0 {
                    warnings.push(format!(
                        "weights.{name}: {} is negative; clamped to 0.",
                        Value::Float(value).py_repr()
                    ));
                    value = 0.0;
                }
                *default_value = value;
            }
        }
    }

    Ok(resolved)
}

struct Measurement {
    name: &'static str,
    raw: f64,
    normalized: f64,
    estimated: bool,
    note: String,
}

fn measure_prompt_size(text: &str, mode: ScoreMode) -> Measurement {
    let chars = text.chars().count() as u64;
    let tokens = if chars == 0 {
        0
    } else {
        chars.div_ceil(CHARS_PER_TOKEN)
    };
    let rendered = mode == ScoreMode::Rendered;
    Measurement {
        name: "prompt_size",
        raw: tokens as f64,
        normalized: saturate(tokens as f64, SIZE_HALF_TOKENS),
        estimated: !rendered,
        note: format!(
            "~{tokens} tokens of {}",
            if rendered {
                "rendered prompt"
            } else {
                "template (estimate)"
            }
        ),
    }
}

fn measure_state_references(template: &str) -> Result<Measurement, ScoreError> {
    let (ref_count, section_count) = tags::extract_references(template)?;
    Ok(Measurement {
        name: "state_references",
        raw: ref_count as f64,
        normalized: saturate(ref_count as f64, REFERENCE_HALF_COUNT),
        estimated: false,
        note: format!(
            "{ref_count} distinct reference(s){}",
            if section_count > 0 {
                format!(", {section_count} section(s)")
            } else {
                String::new()
            }
        ),
    })
}

fn measure_prompt_type(type_name: &str, type_normal: f64) -> Measurement {
    Measurement {
        name: "prompt_type",
        raw: type_normal,
        normalized: type_normal,
        estimated: false,
        note: format!("prompt_type={type_name}"),
    }
}

fn measure_output_schema(schema: Option<&Dict>) -> Measurement {
    let (depth, fields) = schema::walk_schema(schema);
    let normalized = clamp01(
        0.5 * saturate(depth as f64, SCHEMA_DEPTH_HALF)
            + 0.5 * saturate(fields as f64, SCHEMA_FIELDS_HALF),
    );
    Measurement {
        name: "output_schema",
        raw: fields as f64,
        normalized,
        estimated: false,
        note: if schema.is_some() {
            format!("schema depth {depth}, {fields} field(s)")
        } else {
            "no schema declared".to_string()
        },
    }
}

fn measure_output_size(
    schema: Option<&Dict>,
    params: Option<&Dict>,
) -> Result<Measurement, ScoreError> {
    let (tokens, mut keys) = schema::declared_output_tokens(schema, params)?;
    keys.sort_unstable();
    let note = if keys.is_empty() {
        "no declared output limit".to_string()
    } else {
        format!(
            "~{} tokens declared ({})",
            format_f0(tokens),
            keys.join(", ")
        )
    };
    Ok(Measurement {
        name: "output_size",
        raw: round6(tokens),
        normalized: saturate(tokens, OUTPUT_SIZE_HALF_TOKENS),
        estimated: false,
        note,
    })
}

fn measure_structural_position(context: &StructureContext) -> Measurement {
    let normalized = clamp01(
        0.5 * saturate(context.depth as f64, DEPTH_HALF)
            + 0.15 * py_min(context.loop_depth as f64, 2.0)
            + if context.reflector_generated {
                0.2
            } else {
                0.0
            },
    );
    let mut notes = vec![format!("depth {}", context.depth)];
    if context.in_loop() {
        notes.push(format!("inside {} loop(s)", context.loop_depth));
    }
    if context.reflector_generated {
        notes.push("reflector-generated".to_string());
    }
    Measurement {
        name: "structural_position",
        raw: context.depth as f64,
        normalized,
        estimated: false,
        note: notes.join(", "),
    }
}

fn measure_keywords(haystack: &str, table: &Dict) -> Result<Measurement, ScoreError> {
    let matches = keywords::match_keywords(haystack, table)?;
    let sum = matches.iter().fold(0.0, |acc, (_, w)| acc + w);
    let note = if matches.is_empty() {
        "no keyword matches".to_string()
    } else {
        format!(
            "matched {}",
            matches
                .iter()
                .map(|(p, _)| p.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    Ok(Measurement {
        name: "keywords",
        raw: matches.len() as f64,
        normalized: clamp01(sum),
        estimated: false,
        note,
    })
}

fn assemble(
    measured: Vec<Measurement>,
    weights: Vec<(&'static str, f64)>,
    mode: ScoreMode,
    mut warnings: Vec<String>,
) -> ComplexityScore {
    let weight_of = |name: &str| -> f64 {
        let raw = weights
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| *v)
            .unwrap_or(0.0);
        py_max(0.0, round6(raw))
    };

    let weight_total = round6(measured.iter().fold(0.0, |acc, m| acc + weight_of(m.name)));

    if weight_total <= 0.0 {
        warnings.push(
            "weights: every weight is zero; score is 0 and no signal contributes.".to_string(),
        );
    }

    let mut signals = Vec::with_capacity(measured.len());
    let mut total = 0.0;
    for m in &measured {
        let weight = weight_of(m.name);
        let normalized = round6(clamp01(m.normalized));
        let contribution = round6(if weight_total <= 0.0 {
            0.0
        } else {
            MAX_SCORE * weight * normalized / weight_total
        });
        total += contribution;
        signals.push(SignalScore {
            name: m.name,
            raw: round6(m.raw),
            normalized,
            weight,
            contribution,
            estimated: m.estimated,
            note: m.note.clone(),
        });
    }

    let score_value = round6(py_min(MAX_SCORE, py_max(MIN_SCORE, total)));
    let estimated = signals.iter().any(|s| s.estimated);

    ComplexityScore {
        score: score_value,
        max_score: MAX_SCORE,
        mode,
        estimated,
        signals,
        weight_total: round6(weight_total),
        warnings,
    }
}

/// `score(definition=None, *, rendered_prompt=None, weights=None,
/// keyword_weights=None, structure=None)` (`core/complexity.py:
/// 915-1005`).
pub fn score(input: &ScoreInput<'_>) -> Result<ComplexityScore, ScoreError> {
    let mut warnings: Vec<String> = Vec::new();

    // Python never distinguishes "argument omitted" from "argument
    // explicitly `None`" -- both reach the same default. A `Value::Dict`
    // built from JSON *can* carry an explicit `null` for one of these
    // keys, so every optional field is normalized here before anything
    // downstream has a chance to treat `Some(&Value::None)` as a present,
    // non-mapping value (and warn about it, which Python never does).
    let definition = input.definition.filter(|v| !matches!(v, Value::None));
    let rendered_prompt = input.rendered_prompt;
    let weights_input = input.weights.filter(|v| !matches!(v, Value::None));
    let keyword_weights_input = input.keyword_weights.filter(|v| !matches!(v, Value::None));
    let structure_input = input.structure.filter(|v| !matches!(v, Value::None));

    let mut effective_definition: Option<&Value> = definition;
    if definition.is_none() && rendered_prompt.is_none() {
        warnings.push("definition: nothing to score; every signal read as empty.".to_string());
    } else if let Some(v) = definition {
        if is_scalar(v) {
            warnings.push(format!(
                "definition: expected a mapping or definition object, got {}; every signal read as empty.",
                py_type_name(v)
            ));
            effective_definition = None;
        }
    }

    let definition_dict: Option<&Dict> = match effective_definition {
        Some(Value::Dict(d)) => Some(d),
        _ => None,
    };

    let resolved_weights = resolve_weights(weights_input, &mut warnings)?;

    let (context, structure_warnings) = StructureContext::from_value(structure_input)?;
    warnings.extend(structure_warnings);

    let default_table = default_keyword_table();
    let keyword_table: &Dict = match keyword_weights_input {
        None => default_table,
        Some(Value::Dict(d)) => d,
        Some(other) => {
            warnings.push(format!(
                "keyword_weights: expected a mapping, got {}; used defaults.",
                py_type_name(other)
            ));
            default_table
        }
    };

    let template = template_text(definition_dict, &mut warnings);
    let mode = if rendered_prompt.is_some() {
        ScoreMode::Rendered
    } else {
        ScoreMode::Static
    };

    if mode == ScoreMode::Static && effective_definition.is_some() && template.is_empty() {
        warnings.push("template: no template or messages found; size read as empty.".to_string());
    }

    let measurable_text: &str = match mode {
        ScoreMode::Rendered => rendered_prompt.unwrap_or(""),
        ScoreMode::Static => &template,
    };

    let description = get_field(definition_dict, "description");
    let description_str = match description {
        Some(Value::Str(s)) => s.as_str(),
        _ => "",
    };
    let haystack = [measurable_text, description_str]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    let schema = schema_of(definition_dict, &mut warnings);
    let (type_name, type_normal) = prompt_type(definition_dict, &mut warnings);

    let params = match get_field(definition_dict, "params") {
        Some(Value::Dict(d)) => Some(d),
        _ => None,
    };

    let measured = vec![
        measure_prompt_size(measurable_text, mode),
        measure_state_references(&template)?,
        measure_prompt_type(&type_name, type_normal),
        measure_output_schema(schema),
        measure_output_size(schema, params)?,
        measure_structural_position(&context),
        measure_keywords(&haystack, keyword_table)?,
    ];

    Ok(assemble(measured, resolved_weights, mode, warnings))
}

/// `_score_and_route`'s own argument preparation (`core/prompt.py:
/// 959-970`): `(dict(settings.scoring.weights), dict(settings.scoring.
/// keywords) or None)`.
pub fn runtime_tables(settings: &ScoringSettings) -> (Value, Option<Value>) {
    let mut weights = Dict::new();
    for (name, value) in &settings.weights {
        weights.insert(Value::Str(name.clone()), Value::Float(*value));
    }

    let mut keywords = Dict::new();
    for (name, value) in &settings.keywords {
        keywords.insert(Value::Str(name.clone()), Value::Float(*value));
    }
    let keyword_weights = if keywords.is_empty() {
        None
    } else {
        Some(Value::Dict(keywords))
    };

    (Value::Dict(weights), keyword_weights)
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::IntValue;

    fn dict_of(pairs: Vec<(&str, Value)>) -> Dict {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        d
    }

    #[test]
    fn no_definition_and_no_rendered_prompt_warns() {
        let input = ScoreInput::default();
        let result = score(&input).unwrap();
        // `prompt_type` still defaults to "text" (0.25 normalized) even
        // with nothing to score, so the overall score is not zero --
        // confirmed directly against `circuitry.core.complexity.score(None)`.
        assert_eq!(result.score, 3.5);
        assert!(
            result
                .warnings
                .contains(&"definition: nothing to score; every signal read as empty.".to_string())
        );
    }

    #[test]
    fn a_scalar_definition_warns_and_is_treated_as_none() {
        let definition = Value::from(5i64);
        let input = ScoreInput {
            definition: Some(&definition),
            rendered_prompt: Some("x"),
            ..Default::default()
        };
        let result = score(&input).unwrap();
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("expected a mapping or definition object"))
        );
    }

    #[test]
    fn a_date_weights_warns_with_pythons_own_unqualified_class_name() {
        // `type(x).__name__` on a `datetime.date` is `"date"`, not
        // `Value::type_name`'s module-qualified `"datetime.date"`.
        let weights = Value::Date(chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());
        let input = ScoreInput {
            rendered_prompt: Some("x"),
            weights: Some(&weights),
            ..Default::default()
        };
        let result = score(&input).unwrap();
        assert!(result.warnings.iter().any(|w| w.contains("got date;")));
        assert!(!result.warnings.iter().any(|w| w.contains("datetime.date")));
    }

    #[test]
    fn a_nan_depth_raises_value_error() {
        let structure = Value::Dict(dict_of(vec![("depth", Value::from(f64::NAN))]));
        let input = ScoreInput {
            rendered_prompt: Some("x"),
            structure: Some(&structure),
            ..Default::default()
        };
        let err = score(&input).unwrap_err();
        assert_eq!(err.py_class, PyExc::ValueError);
        assert_eq!(err.message, "cannot convert float NaN to integer");
    }

    #[test]
    fn an_infinite_depth_raises_overflow_error() {
        let structure = Value::Dict(dict_of(vec![("depth", Value::from(f64::INFINITY))]));
        let input = ScoreInput {
            rendered_prompt: Some("x"),
            structure: Some(&structure),
            ..Default::default()
        };
        let err = score(&input).unwrap_err();
        assert_eq!(err.py_class, PyExc::OverflowError);
        assert_eq!(err.message, "cannot convert float infinity to integer");
    }

    #[test]
    fn rendered_mode_is_not_estimated_for_prompt_size() {
        let input = ScoreInput {
            rendered_prompt: Some("hello"),
            ..Default::default()
        };
        let result = score(&input).unwrap();
        let signal = result.signal("prompt_size").unwrap();
        assert!(!signal.estimated);
        assert!(!result.estimated); // no signal is estimated in rendered mode.
    }

    #[test]
    fn static_mode_with_no_template_or_messages_warns() {
        let definition = Value::Dict(dict_of(vec![]));
        let input = ScoreInput {
            definition: Some(&definition),
            ..Default::default()
        };
        let result = score(&input).unwrap();
        assert!(
            result.warnings.contains(
                &"template: no template or messages found; size read as empty.".to_string()
            )
        );
    }

    #[test]
    fn an_overflow_int_raises() {
        let big = Value::Int(IntValue::parse_decimal(&"1".repeat(400)).unwrap());
        let params = dict_of(vec![("max_tokens", big)]);
        let definition = Value::Dict(dict_of(vec![
            ("template", Value::from("x")),
            ("params", Value::Dict(params)),
        ]));
        let input = ScoreInput {
            definition: Some(&definition),
            ..Default::default()
        };
        let err = score(&input).unwrap_err();
        assert_eq!(err.py_class, PyExc::OverflowError);
        assert_eq!(err.message, "int too large to convert to float");
    }

    #[test]
    fn all_zero_weights_warns_and_scores_zero() {
        let weights = dict_of(
            SIGNAL_NAMES
                .iter()
                .map(|n| (*n, Value::from(0i64)))
                .collect(),
        );
        let weights_value = Value::Dict(weights);
        let input = ScoreInput {
            rendered_prompt: Some("x"),
            weights: Some(&weights_value),
            ..Default::default()
        };
        let result = score(&input).unwrap();
        assert_eq!(result.score, 0.0);
        assert!(result.warnings.contains(
            &"weights: every weight is zero; score is 0 and no signal contributes.".to_string()
        ));
    }

    #[test]
    fn runtime_tables_empty_keywords_becomes_none() {
        let settings = ScoringSettings::default();
        let (weights, keywords) = runtime_tables(&settings);
        assert!(matches!(weights, Value::Dict(_)));
        assert_eq!(keywords, None);
    }

    #[test]
    fn runtime_tables_non_empty_keywords_round_trips() {
        let mut settings = ScoringSettings::default();
        settings.keywords.insert("foo".to_string(), 1.5);
        let (_, keywords) = runtime_tables(&settings);
        assert!(keywords.is_some());
    }

    #[test]
    fn scoring_settings_default_equals_config_defaults() {
        let settings = ScoringSettings::default();
        assert_eq!(settings.weights.len(), 7);
        assert!(settings.keywords.is_empty());
    }
}
