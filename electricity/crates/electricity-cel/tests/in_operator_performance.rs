//! Issue #400: `state.l.filter(x, x in state.l)` over a wide list stayed
//! quadratic because the `in` operator rebuilt a fresh `Vec<Value>` from
//! the whole container on every call, including every one of the
//! `filter` macro's own `N` iterations -- paid again and again for the
//! same list. This only guards against that *extra* copy coming back,
//! not against `in`'s own, inherent per-call scan cost (unavoidable
//! without an index this crate does not build, and inherent to
//! Circuitry's own `in` too -- it is no less a linear scan there): a
//! wide enough margin that the copy regressing is what would blow the
//! deadline, not ordinary variance from one run to the next.

use electricity_cel::evaluate_condition;
use electricity_value::{IntValue, Value};
use std::time::{Duration, Instant};

#[test]
fn filtering_a_wide_list_by_its_own_membership_stays_well_under_a_generous_deadline() {
    let items: Vec<Value> = (0..5_000).map(|i| Value::Int(IntValue::Small(i))).collect();
    let mut state = electricity_value::Dict::new();
    state.insert(Value::Str("l".to_string()), Value::List(items));
    let state = Value::Dict(state);

    let start = Instant::now();
    let result = evaluate_condition(
        "size(state.l.filter(x, x in state.l)) == 5000",
        &state,
        false,
    );
    let elapsed = start.elapsed();

    assert_eq!(result, Ok(true));
    assert!(
        elapsed < Duration::from_secs(5),
        "filtering a 5,000-item list by its own membership took {elapsed:?}, \
         well past the deadline this test gives the per-call `Vec<Value>` \
         copy issue #400 removed room to regress back in"
    );
}
