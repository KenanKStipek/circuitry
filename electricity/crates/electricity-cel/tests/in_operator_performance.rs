//! Issue #400: both `state.l.filter(x, x in state.l)` and
//! `state.l.filter(x, state.l[0] == 0)` over a wide list rebuilt a fresh
//! `Value::List`/`Value::Map` from the whole container on *every* call
//! `cel` made into this crate's own `in`/`[]`, including every one of
//! the `filter` macro's own `N` iterations -- paid again and again for
//! the same list, rather than once. `in`'s own per-call cost is still a
//! linear scan (unavoidable without an index this crate does not build,
//! and inherent to Circuitry's own `in` too, no less a linear scan
//! there) -- removing the copy only lowers its constant factor, so
//! [`filtering_a_wide_list_by_its_own_membership_stays_well_under_a_generous_deadline`]
//! stays a generous-margin regression guard, not a complexity proof.
//!
//! `[]`'s own per-call cost, unlike `in`'s, is O(1) once the copy is
//! gone: a constant index into the same list on every one of `N` calls
//! is O(N) in total if the container is never copied, O(N²) if it is
//! (issue #400 review). [`indexing_the_same_wide_list_on_every_call_is_linear_not_quadratic`]
//! is the one that actually tells the two apart -- quadrupling `N`
//! quadruples a linear implementation's time and multiplies a quadratic
//! one's by 16, so comparing the two is a complexity check rather than
//! a fixed deadline that would have to be re-tuned for the machine.

use electricity_cel::evaluate_condition;
use electricity_value::{IntValue, Value};
use std::time::{Duration, Instant};

fn wide_list_state(len: i64) -> Value {
    let items: Vec<Value> = (0..len).map(|i| Value::Int(IntValue::Small(i))).collect();
    let mut state = electricity_value::Dict::new();
    state.insert(Value::Str("l".to_string()), Value::List(items));
    Value::Dict(state)
}

#[test]
fn filtering_a_wide_list_by_its_own_membership_stays_well_under_a_generous_deadline() {
    let state = wide_list_state(5_000);

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

/// Indexes the same wide list, at a constant position, once per one of
/// its own `N` elements: O(N) in total unless `[]` still copies the
/// whole list on every call, which would make it O(N²). Timing `N` and
/// `4N` and requiring the ratio to stay well under 16x (quadratic's own
/// ratio for a 4x size increase) distinguishes the two complexity
/// classes directly, rather than relying on a fixed deadline tuned to
/// one machine's own speed.
#[test]
fn indexing_the_same_wide_list_on_every_call_is_linear_not_quadratic() {
    let time_filter = |len: i64| -> Duration {
        let state = wide_list_state(len);
        let expr = format!("size(state.l.filter(x, state.l[0] == 0)) == {len}");
        let start = Instant::now();
        let result = evaluate_condition(&expr, &state, false);
        let elapsed = start.elapsed();
        assert_eq!(result, Ok(true));
        elapsed
    };

    // Warms up allocator/codegen paths before the measured runs, so a
    // one-off cold-start cost doesn't skew the ratio.
    time_filter(500);

    let small = time_filter(5_000);
    let large = time_filter(20_000);

    assert!(
        large < Duration::from_secs(10),
        "indexing a 20,000-item list {} times took {large:?}, past the \
         deadline this test gives an O(N) implementation",
        20_000,
    );
    // A quadratic implementation's ratio for a 4x size increase is 16x;
    // a linear one's is 4x. 10x leaves generous headroom above linear
    // variance while still catching quadratic long before 16x.
    let ratio = large.as_secs_f64() / small.as_secs_f64().max(0.000_001);
    assert!(
        ratio < 10.0,
        "indexing a 4x wider list took {ratio:.1}x as long ({small:?} -> {large:?}) \
         -- that's closer to quadratic (16x) than linear (4x), suggesting `[]` is \
         copying the whole container again on every call"
    );
}
