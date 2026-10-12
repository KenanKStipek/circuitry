//! Issue #505's ruling 4: NaN/±inf reach `meta.complexity` exactly as
//! Python does -- not just inside this crate's own `ComplexityScore`,
//! but through the *state writer* a run would actually use to persist
//! it. A `ComplexityScore::to_meta()` carrying a NaN score (every
//! weight zeroed, `max(0.0, NaN)`'s own rule) must serialize through
//! `electricity-json`'s own writer to the same `NaN`/`Infinity` tokens
//! `json.dumps` emits for the Python reference's own `_complexity_meta`
//! result -- confirmed directly against CPython: `json.dumps(float("nan"))
//! == "NaN"`, `json.dumps(float("inf")) == "Infinity"`.

use electricity_complexity::{ScoreInput, score};
use electricity_json::{WriteMode, dumps};
use electricity_value::Value;

#[test]
fn a_nan_weight_reaches_meta_and_the_state_writer_as_nan() {
    // Every weight NaN: `weight_of` -> `max(0.0, round6(NaN))` -> `0.0`
    // for every signal, `weight_total` -> `0.0`, so every contribution
    // is `0.0 / 0.0`-shaped (`0.0` under the `weight_total <= 0.0`
    // branch) -- that path alone cannot *reach* a NaN score. The
    // reference reaches a NaN total by routing a NaN *through* the
    // weighted mean instead: an infinite weight makes `weight_total`
    // infinite and every contribution `inf * 0.0`-shaped, landing on
    // NaN for a signal whose own normalized value is nonzero.
    let weights = {
        let mut d = electricity_value::Dict::new();
        for name in electricity_complexity::SIGNAL_NAMES {
            d.insert(Value::Str(name.to_string()), Value::Float(f64::INFINITY));
        }
        Value::Dict(d)
    };
    let input = ScoreInput {
        rendered_prompt: Some("x"),
        weights: Some(&weights),
        ..Default::default()
    };
    let result = score(&input).expect("an infinite weight does not raise");

    let meta = result.to_meta();
    let signals = match &meta {
        Value::Dict(d) => match d.get(&Value::Str("signals".to_string())) {
            Some(Value::Dict(s)) => s,
            _ => panic!("meta.signals must be a dict"),
        },
        _ => panic!("meta must be a dict"),
    };
    let prompt_type = match signals.get(&Value::Str("prompt_type".to_string())) {
        Some(Value::Dict(d)) => d,
        _ => panic!("meta.signals.prompt_type must be a dict"),
    };
    let contribution = match prompt_type.get(&Value::Str("contribution".to_string())) {
        Some(Value::Float(f)) => *f,
        other => panic!("contribution must be a float, got {other:?}"),
    };
    assert!(
        contribution.is_nan(),
        "expected NaN contribution, got {contribution}"
    );

    let text = dumps(&meta, WriteMode::COMPACT).expect("NaN/inf serialize without raising");
    assert!(
        text.contains("NaN"),
        "state writer output must carry a bare NaN token: {text}"
    );
}
