//! Nesting must be bounded by [`electricity_cel::MAX_NESTING_DEPTH`],
//! enforced before parsing (`nesting.rs`'s module docs), not by whatever
//! the test harness's own thread stack happens to be. Every test here
//! runs on an explicit 2 MiB stack -- the default a spawned thread gets,
//! where the runner does most of this work -- in a debug build, which
//! needs `Cargo.toml`'s `[profile.dev.package.cel]`/
//! `[profile.dev.package.antlr4rust]` override to stay safe at all (see
//! that file's own comment, and `nesting.rs`'s module docs, for the
//! measurements behind both).

use electricity_cel::{CelError, MAX_NESTING_DEPTH, evaluate_condition};
use electricity_value::{Dict, Value};

fn run_on_2mib_stack<F: FnOnce() -> Result<bool, CelError> + Send + 'static>(
    f: F,
) -> Result<bool, CelError> {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

fn nested_parens(n: usize) -> String {
    format!("{}1{}", "(".repeat(n), ")".repeat(n))
}

#[test]
fn exactly_at_the_limit_parses_and_evaluates_on_a_2mib_stack() {
    let expr = nested_parens(MAX_NESTING_DEPTH);
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    // `1` is truthy, so the condition evaluates to `true`; the point is
    // that it parses and evaluates at all, rather than being rejected.
    assert_eq!(result, Ok(true));
}

#[test]
fn one_past_the_limit_is_a_distinct_nesting_error_on_a_2mib_stack() {
    let expr = nested_parens(MAX_NESTING_DEPTH + 1);
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    match result {
        Err(e) if e.is_too_deeply_nested() => {}
        other => panic!("expected a too-deeply-nested CelError, got {other:?}"),
    }
}

#[test]
fn nested_list_literals_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let n = MAX_NESTING_DEPTH;
    let expr = format!("{}1{}", "[".repeat(n), "]".repeat(n));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_eq!(result, Ok(true));

    let n = MAX_NESTING_DEPTH + 1;
    let expr = format!("{}1{}", "[".repeat(n), "]".repeat(n));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    match result {
        Err(e) if e.is_too_deeply_nested() => {}
        other => panic!("expected a too-deeply-nested CelError, got {other:?}"),
    }
}

#[test]
fn nested_map_literals_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let n = MAX_NESTING_DEPTH;
    let expr = format!("{}1{}", "{'a':".repeat(n), "}".repeat(n));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_eq!(result, Ok(true));

    let n = MAX_NESTING_DEPTH + 1;
    let expr = format!("{}1{}", "{'a':".repeat(n), "}".repeat(n));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    match result {
        Err(e) if e.is_too_deeply_nested() => {}
        other => panic!("expected a too-deeply-nested CelError, got {other:?}"),
    }
}

/// The two operator shapes `MAX_NESTING_DEPTH` deliberately never counts
/// (`nesting.rs`'s module docs): a long chain of either stays safe well
/// past any bracket-nesting depth this crate would ever allow, bounded
/// only by [`electricity_cel::MAX_EXPR_LENGTH`].
#[test]
fn a_long_chain_of_unary_not_is_not_bounded_by_nesting_depth() {
    let expr = format!("{}true", "!".repeat(1300));
    assert!(expr.chars().count() <= electricity_cel::MAX_EXPR_LENGTH);
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert!(result.is_ok());
}

#[test]
fn a_long_and_chain_is_not_bounded_by_nesting_depth() {
    let expr = (0..500).map(|_| "true").collect::<Vec<_>>().join(" && ");
    assert!(expr.chars().count() <= electricity_cel::MAX_EXPR_LENGTH);
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_eq!(result, Ok(true));
}

// The shapes below are exactly the ones `nesting.rs`'s module docs
// measured: unlike bracket nesting, none of these grows `bracket_depth`
// on its own (an arithmetic/relation chain opens no bracket at all; an
// index/select/method-call chain opens and closes one `[`/`(` at a time,
// never two at once) -- before this crate counted operator/accessor
// chains too, `check_nesting_depth` let every one of them straight
// through regardless of length. `0+1+1+...` measured as overflowing a 2
// MiB debug *and* release stack somewhere between 1600 and 1700 terms,
// and `state[0][0][0]...` at 1300 -- both far below
// `electricity_cel::MAX_EXPR_LENGTH` (4096 chars) can reach (about 2047
// and 1365 terms respectively). `state.a.a.a...`, `0<1<2<...` and
// `state.f().f().f()...` did not overflow even at the deepest either can
// reach under the character cap (about 2045, 1080 and 1020 terms) -- but
// that is a coincidence of today's stack-frame sizes against today's
// character cap, not a structural difference (`nesting.rs`'s module
// docs), so they are bounded the same way.

fn assert_not_rejected_for_nesting(result: &Result<bool, CelError>) {
    if let Err(e) = result {
        assert!(
            !e.is_too_deeply_nested(),
            "expected no nesting rejection at exactly MAX_NESTING_DEPTH, got {result:?}"
        );
    }
}

fn assert_rejected_for_nesting(result: &Result<bool, CelError>) {
    match result {
        Err(e) if e.is_too_deeply_nested() => {}
        other => panic!("expected a too-deeply-nested CelError, got {other:?}"),
    }
}

#[test]
fn arithmetic_chain_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let expr = format!("0{}", "+1".repeat(MAX_NESTING_DEPTH));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);

    let expr = format!("0{}", "+1".repeat(MAX_NESTING_DEPTH + 1));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn relation_chain_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let expr = format!("0{}", "<1".repeat(MAX_NESTING_DEPTH));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);

    let expr = format!("0{}", "<1".repeat(MAX_NESTING_DEPTH + 1));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn select_chain_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let expr = format!("state{}", ".a".repeat(MAX_NESTING_DEPTH));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);

    let expr = format!("state{}", ".a".repeat(MAX_NESTING_DEPTH + 1));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn index_chain_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let expr = format!("state{}", "[0]".repeat(MAX_NESTING_DEPTH));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);

    let expr = format!("state{}", "[0]".repeat(MAX_NESTING_DEPTH + 1));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn method_call_chain_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let expr = format!("state{}", ".f()".repeat(MAX_NESTING_DEPTH));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);

    let expr = format!("state{}", ".f()".repeat(MAX_NESTING_DEPTH + 1));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn many_independent_and_joined_comparisons_are_not_rejected_on_a_2mib_stack() {
    // Realistic and wide, not deep: `MAX_NESTING_DEPTH` comparisons
    // ANDed together, each only one `.` and one `==` deep -- must not
    // be rejected just because there are more of them than
    // `MAX_NESTING_DEPTH` (`nesting.rs`'s module docs on why `&&`
    // resets the chain).
    let expr = (0..MAX_NESTING_DEPTH + 1)
        .map(|i| format!("state.a == {i}"))
        .collect::<Vec<_>>()
        .join(" && ");
    assert!(expr.chars().count() <= electricity_cel::MAX_EXPR_LENGTH);
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);
}
