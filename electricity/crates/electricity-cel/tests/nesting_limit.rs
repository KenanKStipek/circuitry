//! Nesting must be bounded by one explicit limit
//! (`electricity_cel::MAX_CEL_NESTING_DEPTH`), checked right after an
//! expression parses, before anything evaluates it -- the same number
//! on both engines (`nesting.rs`'s module docs) -- enforced before
//! parsing ever even completes for a shape that would genuinely risk
//! overflowing the real stack, not by whatever the test harness's own
//! thread stack happens to be. Every test here runs on an explicit 2
//! MiB stack -- a deliberately conservative floor, not a measured
//! figure for how this crate is actually run: `electricity-cli`'s own
//! `main.rs` evaluates CEL on the process's main thread, under a
//! `tokio::runtime::Builder::new_current_thread()`, whose stack is the
//! OS default (far bigger than 2 MiB), not a spawned thread at all. In
//! a debug build, this 2 MiB floor needs `Cargo.toml`'s
//! `[profile.dev.package.cel]`/`[profile.dev.package.antlr4rust]`
//! override to stay safe at all -- see that file's own comment for the
//! measurement behind it (the primary source for that number; not
//! re-measured here).

use electricity_cel::{CelError, MAX_CEL_NESTING_DEPTH, evaluate_condition, validate_syntax};
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

macro_rules! at_limit_and_one_past_it {
    ($name:ident, $shape:expr) => {
        #[test]
        fn $name() {
            let shape: fn(usize) -> String = $shape;
            let expr = shape(MAX_CEL_NESTING_DEPTH);
            assert_eq!(validate_syntax(&expr), Ok(()));
            let result = run_on_2mib_stack(move || {
                evaluate_condition(
                    &shape(MAX_CEL_NESTING_DEPTH),
                    &Value::Dict(Dict::new()),
                    false,
                )
            });
            assert_not_rejected_for_nesting(&result);

            let over = shape(MAX_CEL_NESTING_DEPTH + 1);
            assert!(validate_syntax(&over).is_err());
            let result = run_on_2mib_stack(move || {
                evaluate_condition(
                    &shape(MAX_CEL_NESTING_DEPTH + 1),
                    &Value::Dict(Dict::new()),
                    false,
                )
            });
            assert_rejected_for_nesting(&result);
        }
    };
}

at_limit_and_one_past_it!(grouping_parens, |n| format!(
    "{}1{}",
    "(".repeat(n),
    ")".repeat(n)
));
at_limit_and_one_past_it!(nested_global_calls, |n| format!(
    "{}1{}",
    "int(".repeat(n),
    ")".repeat(n)
));
at_limit_and_one_past_it!(nested_list_literals, |n| format!(
    "{}1{}",
    "[".repeat(n),
    "]".repeat(n)
));
at_limit_and_one_past_it!(nested_map_literals, |n| format!(
    "{}1{}",
    "{'a':".repeat(n),
    "}".repeat(n)
));
at_limit_and_one_past_it!(member_access_chain, |n| format!("state{}", ".a".repeat(n)));
at_limit_and_one_past_it!(index_chain, |n| format!("state{}", "[0]".repeat(n)));
at_limit_and_one_past_it!(method_call_chain, |n| format!("state{}", ".f()".repeat(n)));
at_limit_and_one_past_it!(and_chain, |n| (0..=n)
    .map(|_| "true")
    .collect::<Vec<_>>()
    .join(" && "));
at_limit_and_one_past_it!(or_chain, |n| (0..=n)
    .map(|_| "true")
    .collect::<Vec<_>>()
    .join(" || "));
at_limit_and_one_past_it!(unary_not_run, |n| format!("{}true", "!".repeat(n)));
at_limit_and_one_past_it!(unary_minus_run, |n| format!("{}1", ("- ").repeat(n)));
at_limit_and_one_past_it!(arithmetic_chain, |n| format!("0{}", "+1".repeat(n)));
at_limit_and_one_past_it!(division_chain, |n| format!("1{}", "/1".repeat(n)));
at_limit_and_one_past_it!(relation_chain, |n| format!("0{}", "<1".repeat(n)));
at_limit_and_one_past_it!(equality_chain, |n| format!("0{}", "==1".repeat(n)));

#[test]
fn many_independent_list_elements_are_not_rejected_on_a_2mib_stack() {
    // Realistic and wide, not deep: a list of a thousand short,
    // independent elements must not be rejected just because there
    // are more of them than MAX_CEL_NESTING_DEPTH -- only genuine
    // nesting counts, never sibling count.
    let elements = (0..1000).map(|_| "1").collect::<Vec<_>>().join(",");
    let expr = format!("[{elements}]");
    assert!(expr.chars().count() <= electricity_cel::MAX_EXPR_LENGTH);
    assert_eq!(validate_syntax(&expr), Ok(()));
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert_not_rejected_for_nesting(&result);
}

/// A grouping parenthesis leaves no trace in `cel`'s own parsed tree
/// (`nesting.rs`'s module docs), so a mix of grouping parens and some
/// other construct is checked as two independent maxima, not one
/// combined path -- a known, accepted imprecision for a shape this far
/// outside anything an ordinary orchestration writes (module docs:
/// this crate's own stack stays safe regardless, since both numbers
/// individually have a wide margin under the real overflow floor).
#[test]
fn a_paren_deep_expression_wrapping_a_chain_deep_one_still_evaluates_on_a_2mib_stack() {
    let expr = format!(
        "{}0{}{}",
        "(".repeat(MAX_CEL_NESTING_DEPTH),
        "+1".repeat(MAX_CEL_NESTING_DEPTH),
        ")".repeat(MAX_CEL_NESTING_DEPTH)
    );
    let result =
        run_on_2mib_stack(move || evaluate_condition(&expr, &Value::Dict(Dict::new()), false));
    assert!(result.is_ok(), "expected Ok, got {result:?}");
}
