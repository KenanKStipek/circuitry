//! Nesting must be bounded by the five caps `nesting.rs` tracks
//! (`MAX_PAREN_CALL_NESTING_DEPTH`, `MAX_LIST_MAP_NESTING_DEPTH`,
//! `MAX_CHAIN_NESTING_DEPTH`, `MAX_LOGICAL_UNARY_NESTING_DEPTH`,
//! `MAX_ACCESSOR_NESTING_DEPTH`), enforced before parsing (`nesting.rs`'s
//! module docs), not by whatever the test harness's own thread stack
//! happens to be. Every test here runs on an explicit 2 MiB stack -- the
//! default a spawned thread gets, where the runner does most of this
//! work -- in a debug build, which needs `Cargo.toml`'s
//! `[profile.dev.package.cel]`/`[profile.dev.package.antlr4rust]`
//! override to stay safe at all (see that file's own comment, and
//! `nesting.rs`'s module docs, for the measurements behind both).

use electricity_cel::{
    CelError, MAX_ACCESSOR_NESTING_DEPTH, MAX_CHAIN_NESTING_DEPTH,
    MAX_LIST_MAP_NESTING_DEPTH, MAX_LOGICAL_UNARY_NESTING_DEPTH, MAX_PAREN_CALL_NESTING_DEPTH,
    evaluate_condition, validate_syntax,
};
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

fn assert_not_rejected_for_nesting(result: &Result<bool, CelError>) {
    if let Err(e) = result {
        assert!(
            !e.is_too_deeply_nested(),
            "expected no nesting rejection, got {result:?}"
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
fn parens_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let expr = nested_parens(MAX_PAREN_CALL_NESTING_DEPTH);
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    // `1` is truthy, so the condition evaluates to `true`; the point is
    // that it parses and evaluates at all, rather than being rejected.
    assert_eq!(result, Ok(true));

    let expr = nested_parens(MAX_PAREN_CALL_NESTING_DEPTH + 1);
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn nested_global_calls_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let n = MAX_PAREN_CALL_NESTING_DEPTH;
    let expr = format!("{}1{}", "int(".repeat(n), ")".repeat(n));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);

    let n = MAX_PAREN_CALL_NESTING_DEPTH + 1;
    let expr = format!("{}1{}", "int(".repeat(n), ")".repeat(n));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn nested_list_literals_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let n = MAX_LIST_MAP_NESTING_DEPTH;
    let expr = format!("{}1{}", "[".repeat(n), "]".repeat(n));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_eq!(result, Ok(true));

    let n = MAX_LIST_MAP_NESTING_DEPTH + 1;
    let expr = format!("{}1{}", "[".repeat(n), "]".repeat(n));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn nested_map_literals_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let n = MAX_LIST_MAP_NESTING_DEPTH;
    let expr = format!("{}1{}", "{'a':".repeat(n), "}".repeat(n));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_eq!(result, Ok(true));

    let n = MAX_LIST_MAP_NESTING_DEPTH + 1;
    let expr = format!("{}1{}", "{'a':".repeat(n), "}".repeat(n));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn and_chain_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let expr = (0..=MAX_LOGICAL_UNARY_NESTING_DEPTH)
        .map(|_| "true")
        .collect::<Vec<_>>()
        .join(" && ");
    assert!(expr.chars().count() <= electricity_cel::MAX_EXPR_LENGTH);
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);

    let expr = (0..=MAX_LOGICAL_UNARY_NESTING_DEPTH + 1)
        .map(|_| "true")
        .collect::<Vec<_>>()
        .join(" && ");
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn or_chain_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let expr = (0..=MAX_LOGICAL_UNARY_NESTING_DEPTH)
        .map(|_| "true")
        .collect::<Vec<_>>()
        .join(" || ");
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);

    let expr = (0..=MAX_LOGICAL_UNARY_NESTING_DEPTH + 1)
        .map(|_| "true")
        .collect::<Vec<_>>()
        .join(" || ");
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn unary_not_run_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let expr = format!("{}true", "!".repeat(MAX_LOGICAL_UNARY_NESTING_DEPTH));
    assert!(expr.chars().count() <= electricity_cel::MAX_EXPR_LENGTH);
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);

    let expr = format!("{}true", "!".repeat(MAX_LOGICAL_UNARY_NESTING_DEPTH + 1));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn unary_minus_run_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let expr = format!("{}1", "-".repeat(MAX_LOGICAL_UNARY_NESTING_DEPTH));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);

    let expr = format!("{}1", "-".repeat(MAX_LOGICAL_UNARY_NESTING_DEPTH + 1));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn arithmetic_chain_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let expr = format!("0{}", "+1".repeat(MAX_CHAIN_NESTING_DEPTH));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);

    let expr = format!("0{}", "+1".repeat(MAX_CHAIN_NESTING_DEPTH + 1));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn division_chain_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let expr = format!("1{}", "/1".repeat(MAX_CHAIN_NESTING_DEPTH));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);

    let expr = format!("1{}", "/1".repeat(MAX_CHAIN_NESTING_DEPTH + 1));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn relation_chain_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let expr = format!("0{}", "<1".repeat(MAX_CHAIN_NESTING_DEPTH));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);

    let expr = format!("0{}", "<1".repeat(MAX_CHAIN_NESTING_DEPTH + 1));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn equality_chain_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let expr = format!("0{}", "==1".repeat(MAX_CHAIN_NESTING_DEPTH));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);

    let expr = format!("0{}", "==1".repeat(MAX_CHAIN_NESTING_DEPTH + 1));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn index_chain_exactly_at_the_limit_and_one_past_it_on_a_2mib_stack() {
    let expr = format!("state{}", "[0]".repeat(MAX_CHAIN_NESTING_DEPTH));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);

    let expr = format!("state{}", "[0]".repeat(MAX_CHAIN_NESTING_DEPTH + 1));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn select_chain_exactly_at_the_accessor_limit_and_one_past_it_at_compile_time() {
    // The one shape checked only at compile time, not by
    // `evaluate_condition` through the usual document-compile path
    // (`nesting.rs`'s module docs) -- `validate_syntax` is what a real
    // document goes through before it is ever accepted.
    let expr = format!("state{}", ".a".repeat(MAX_ACCESSOR_NESTING_DEPTH));
    assert_eq!(validate_syntax(&expr), Ok(()));

    let expr = format!("state{}", ".a".repeat(MAX_ACCESSOR_NESTING_DEPTH + 1));
    assert!(validate_syntax(&expr).is_err());
}

#[test]
fn select_chain_one_past_the_accessor_limit_is_also_rejected_calling_evaluate_condition_directly_on_a_2mib_stack()
 {
    // A caller reaching `evaluate_condition` directly, bypassing the
    // compiler's own `validate_syntax`, still gets a too-deeply-nested
    // rejection rather than a stack overflow -- matching Circuitry's own
    // `evaluate_cel`, which hits the same bare recursion a direct call
    // skipping `validate_cel_syntax` would (`nesting.rs`'s module docs).
    let expr = format!("state{}", ".a".repeat(MAX_ACCESSOR_NESTING_DEPTH + 1));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_rejected_for_nesting(&result);
}

#[test]
fn method_call_chain_exactly_at_the_accessor_limit_and_one_past_it_at_compile_time() {
    // A method-call chain counts via `accessor` too (the leading `.` of
    // each link), the same as a plain select chain -- its own `(`/`)`
    // are not counted separately (`nesting.rs`'s module docs).
    let expr = format!("state{}", ".f()".repeat(MAX_ACCESSOR_NESTING_DEPTH));
    assert_eq!(validate_syntax(&expr), Ok(()));

    let expr = format!("state{}", ".f()".repeat(MAX_ACCESSOR_NESTING_DEPTH + 1));
    assert!(validate_syntax(&expr).is_err());
}

#[test]
fn many_independent_and_joined_comparisons_are_not_rejected_on_a_2mib_stack() {
    // Realistic and wide, not deep: `MAX_CHAIN_NESTING_DEPTH` comparisons
    // ANDed together, each only one `.` and one `==` deep -- must not
    // be rejected just because there are more of them than
    // `MAX_CHAIN_NESTING_DEPTH` (`nesting.rs`'s module docs on why `&&`
    // resets `chain`/`accessor`, counting only into `logical_unary`
    // instead, whose own cap is far looser).
    let expr = (0..MAX_CHAIN_NESTING_DEPTH + 1)
        .map(|_| "state.a==1")
        .collect::<Vec<_>>()
        .join("&&");
    assert!(expr.chars().count() <= electricity_cel::MAX_EXPR_LENGTH);
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);
}

/// A mixed shape -- bracket nesting wrapped around a chain -- can
/// legitimately exceed either cap's own individual ceiling once both
/// contribute at once (`nesting.rs`'s module docs: this is a known,
/// accepted residual divergence from Circuitry, which would itself
/// reject a combination this deep). This crate's own stack stays safe
/// regardless (both caps individually have a wide margin under the
/// measured overflow floors), so this is exercised here only to pin
/// that it stays *accepted*, not to claim parity with Circuitry at this
/// specific combined depth.
#[test]
fn a_bracket_deep_expression_wrapping_a_chain_deep_one_still_evaluates_on_a_2mib_stack() {
    let expr = format!(
        "{}0{}{}",
        "(".repeat(MAX_PAREN_CALL_NESTING_DEPTH),
        "+1".repeat(MAX_CHAIN_NESTING_DEPTH),
        ")".repeat(MAX_PAREN_CALL_NESTING_DEPTH)
    );
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert!(result.is_ok(), "expected Ok, got {result:?}");
}
