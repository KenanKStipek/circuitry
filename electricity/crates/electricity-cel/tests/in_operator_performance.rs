//! Issue #400: both `state.l.filter(x, x in state.l)` and
//! `state.l.filter(x, state.l[0] == 0)` over a wide list rebuilt a fresh
//! `Value::List`/`Value::Map` from the whole container on *every* call
//! `cel` made into this crate's own `in`/`[]`/`==`/`!=`, including every
//! one of the `filter` macro's own `N` iterations -- paid again and
//! again for the same list, rather than once. `in`'s own per-call cost
//! is still a linear scan (unavoidable without an index this crate does
//! not build, and inherent to Circuitry's own `in` too, no less a
//! linear scan there) -- removing the copy only lowers its constant
//! factor, so
//! [`filtering_a_wide_list_by_its_own_membership_stays_well_under_a_generous_deadline`]
//! stays a generous-margin regression guard, not a complexity proof.
//!
//! `[]`'s own per-call cost, unlike `in`'s, is O(1) once the copy is
//! gone: a constant index into the same list on every one of `N` calls
//! is O(N) in total if the container is never copied, O(N²) if it is.
//! The same is true of `in`/`==`/`!=` once the
//! *probe* itself makes every call decide in O(1): `0 in state.l`
//! matches on the very first element scanned every time, and
//! `state.l != 0` decides on the two operands' own [`Kind`](cel::common::types::Kind)s
//! alone (`equality.rs`'s own `spec_eq_from_context`), without ever
//! converting the list at all. The three
//! `*_is_linear_not_quadratic` tests below tell a linear implementation
//! from a quadratic one directly -- quadrupling `N` quadruples a linear
//! implementation's time and multiplies a quadratic one's by 16 -- so
//! comparing the two is a complexity check rather than a fixed deadline
//! that would have to be re-tuned for the machine. Each takes the
//! minimum of 5 runs per size, interleaved (`small`, `large`, `small`,
//! `large`, ... alternating which goes first each round), and uses a
//! size large enough that even the smaller sample runs for comfortably
//! over 100ms -- both needed so one slow sample on a loaded CI runner
//! (every job here runs twice, on two runner types) can't by itself
//! push the ratio past the quadratic-vs-linear threshold below.

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

/// *expr_for(n)*'s running time at *small_n* and *large_n*, each the
/// minimum of 5 runs, interleaved: which size is timed first
/// alternates from one pass to the next, so a transient stall never
/// lands on the same size every time. One untimed warm-up call first,
/// so a one-off cold-start cost (allocator/codegen) never counts as one
/// of the 5.
fn min_of_five_interleaved(
    small_n: i64,
    large_n: i64,
    expr_for: impl Fn(i64) -> String,
) -> (Duration, Duration) {
    let run = |n: i64| -> Duration {
        let state = wide_list_state(n);
        let expr = expr_for(n);
        let start = Instant::now();
        let result = evaluate_condition(&expr, &state, false);
        let elapsed = start.elapsed();
        assert_eq!(result, Ok(true), "{expr}");
        elapsed
    };

    run(small_n.min(500));

    let mut small_min = Duration::MAX;
    let mut large_min = Duration::MAX;
    for pass in 0..5 {
        let (small, large) = if pass % 2 == 0 {
            (run(small_n), run(large_n))
        } else {
            let large = run(large_n);
            let small = run(small_n);
            (small, large)
        };
        small_min = small_min.min(small);
        large_min = large_min.min(large);
    }
    (small_min, large_min)
}

/// A quadratic implementation's ratio for a 4x size increase is 16x; a
/// linear one's is 4x. 10x leaves generous headroom above linear
/// variance while still catching quadratic long before 16x.
fn assert_linear_not_quadratic(label: &str, small: Duration, large: Duration) {
    let ratio = large.as_secs_f64() / small.as_secs_f64().max(0.000_001);
    assert!(
        ratio < 10.0,
        "{label}: a 4x wider list took {ratio:.1}x as long ({small:?} -> {large:?}) \
         -- that's closer to quadratic (16x) than linear (4x), suggesting the \
         container is being copied again on every call"
    );
}

#[test]
fn indexing_the_same_wide_list_on_every_call_is_linear_not_quadratic() {
    let (small, large) = min_of_five_interleaved(150_000, 600_000, |n| {
        format!("size(state.l.filter(x, state.l[0] == 0)) == {n}")
    });
    assert_linear_not_quadratic("indexing", small, large);
}

/// `0 in state.l`: a probe that matches the very first element scanned,
/// on every one of `N` calls -- O(1) per call without a copy, O(N) with
/// one. A fixed deadline alone would never have caught a regression
/// back to copying here: even the old per-call copy cost stayed under
/// a generous deadline at this test's own list size.
#[test]
fn in_finding_an_immediate_match_is_linear_not_quadratic() {
    let (small, large) = min_of_five_interleaved(150_000, 600_000, |n| {
        format!("size(state.l.filter(x, 0 in state.l)) == {n}")
    });
    assert_linear_not_quadratic("in (immediate match)", small, large);
}

/// `state.l != 0`: a list compared against a scalar, decided by
/// `equality.rs`'s own `spec_eq_from_context` from the two operands'
/// `Kind`s alone -- O(1) per call without ever converting the list at
/// all, O(N) if that short-circuit stopped running the full conversion
/// first.
#[test]
fn not_equal_short_circuit_is_linear_not_quadratic() {
    let (small, large) = min_of_five_interleaved(150_000, 600_000, |n| {
        format!("size(state.l.filter(x, state.l != 0)) == {n}")
    });
    assert_linear_not_quadratic("!= short-circuit", small, large);
}
