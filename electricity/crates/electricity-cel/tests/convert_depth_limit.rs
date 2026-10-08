//! `convert::to_cel`'s own recursion must stay bounded by
//! [`electricity_value::MAX_DEPTH`] (`convert.rs`'s module docs) -- the
//! one depth limit in this crate that *is* the shared one, unlike
//! [`electricity_cel::MAX_NESTING_DEPTH`] (`nesting_limit.rs`), because
//! this bounds a `Value` tree, not CEL expression syntax. Reachable only
//! through a run-time-built `state`/`value`/`meta`: this workspace's own
//! JSON/YAML readers can never produce a `Value` this deep to begin
//! with. Every test runs on an explicit 2 MiB stack, the default a
//! spawned thread gets.

use electricity_cel::evaluate_condition;
use electricity_value::{MAX_DEPTH, Value};

fn nested_list_state(depth: usize) -> Value {
    let mut v = Value::from(1_i64);
    for _ in 0..depth {
        v = Value::List(vec![v]);
    }
    v
}

fn run_on_2mib_stack<F: FnOnce() + Send + 'static>(f: F) {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn state_nested_exactly_to_the_limit_converts_on_a_2mib_stack() {
    run_on_2mib_stack(|| {
        let state = nested_list_state(MAX_DEPTH);
        // `size(state)` only needs to read the root, but evaluation
        // still converts the whole bound `state` argument up front.
        let result = evaluate_condition("size(state) >= 0", &state, false);
        assert_eq!(result, Ok(true));
    });
}

#[test]
fn state_nested_one_past_the_limit_is_a_distinct_nesting_error_on_a_2mib_stack() {
    run_on_2mib_stack(|| {
        let state = nested_list_state(MAX_DEPTH + 1);
        let result = evaluate_condition("size(state) >= 0", &state, false);
        match result {
            Err(e) if e.is_too_deeply_nested() => {}
            other => panic!("expected a too-deeply-nested CelError, got {other:?}"),
        }
    });
}

// A `state` orders of magnitude deeper than `MAX_DEPTH` (tested here:
// 200_000 levels) overflows this 2 MiB stack before `to_cel`'s own
// depth check ever runs, in `Value`'s own (derived, still recursive)
// `Clone` impl: `evaluate_condition` clones `state` -- or the narrower
// `paths::project`ed slice of it -- before converting it at all. This
// is not a regression this crate introduces (`Clone` was always
// recursive) and not a path this fix claims to cover: #394 lists
// `Drop`/`py_str`/`py_repr`/equality/hashing/ordering as the
// `electricity-value` operations needing a depth-safe treatment, not
// `Clone`, and a `Value` this much deeper than `MAX_DEPTH` is reachable
// only by constructing one directly against the `electricity-value`
// API (no reader in this workspace can produce one, and nothing else in
// this workspace builds one at run time yet). Left as a known,
// documented limitation rather than fixed here.
