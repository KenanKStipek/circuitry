//! A user expression that calls one of this crate's own reserved
//! `__electricity_*` names directly -- the internal substitutions
//! `paths::rewrite` makes for `cel`'s native `==`/`!=`/`in`/`<`/`<=`/
//! `>`/`>=`/`_[_]`, and the marker `has()`'s own macro expansion wraps
//! -- must fail evaluation the same way calling any other undeclared
//! function does, never panic, regardless of how many arguments it's
//! called with (issue #400).

use electricity_cel::evaluate_condition;
use electricity_value::{Dict, Value};

const RESERVED_NAMES: &[&str] = &[
    "__electricity_strict_eq",
    "__electricity_strict_ne",
    "__electricity_strict_in",
    "__electricity_python_index",
    "__electricity_strict_lt",
    "__electricity_strict_le",
    "__electricity_strict_gt",
    "__electricity_strict_ge",
    "__electricity_false",
];

fn call_with(name: &str, arg_count: usize) -> String {
    let args = vec!["1"; arg_count].join(", ");
    format!("{name}({args})")
}

#[test]
fn every_reserved_name_errors_never_panics_at_any_arity() {
    let state = Value::Dict(Dict::new());
    for name in RESERVED_NAMES {
        for arg_count in [0, 1, 3] {
            let expr = call_with(name, arg_count);
            let result = evaluate_condition(&expr, &state, false);
            assert!(
                result.is_err(),
                "{expr:?} should fail evaluation, not resolve to {result:?}"
            );
        }
    }
}

#[test]
fn a_reserved_name_call_is_worded_like_an_undeclared_function() {
    let state = Value::Dict(Dict::new());
    let err = evaluate_condition("__electricity_strict_eq(1)", &state, false).unwrap_err();
    assert_eq!(
        err.to_string(),
        "CEL evaluation failed for '__electricity_strict_eq(1)': \
         undeclared reference to '__electricity_strict_eq'"
    );
}

#[test]
fn a_reserved_name_call_nested_inside_a_legitimate_has_still_errors() {
    let state = Value::Dict(Dict::new());
    let err = evaluate_condition("has(__electricity_python_index(1))", &state, false).unwrap_err();
    assert_eq!(
        err.to_string(),
        "CEL evaluation failed for 'has(__electricity_python_index(1))': \
         undeclared reference to '__electricity_python_index'"
    );
}

#[test]
fn an_ordinary_has_call_still_works() {
    // The one legitimate shape that calls `__electricity_false`
    // directly -- `has()`'s own macro expansion -- must stay unaffected
    // by rejecting every *other* reserved-name call.
    let state = Value::Dict(Dict::new());
    assert_eq!(evaluate_condition("has([1][0])", &state, false), Ok(true));
    assert_eq!(evaluate_condition("has([][0])", &state, false), Ok(false));
}
