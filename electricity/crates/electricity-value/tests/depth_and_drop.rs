//! `Value::depth` and `Drop` must both stay safe on a `Value` nested far
//! past [`MAX_DEPTH`] — the one case this crate's own readers (JSON/YAML)
//! can never produce, but a run-time construction (a CEL evaluation
//! result, a future state merge or loop) can (#394, `lib.rs`'s own docs
//! on `MAX_DEPTH`/`Drop`). A plain `#[test]` is enough for `depth()` (it
//! never recurses), but `Drop` would abort the whole test process on a
//! stack overflow instead of merely failing if its iterative rewrite
//! regressed back to the default, recursive glue — every `Drop` case
//! here runs on an explicit 2MiB-stack thread, the default a spawned
//! thread (where the runner does most of its work) gets.

use electricity_value::{MAX_DEPTH, Value};

fn nested_list(depth: usize) -> Value {
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
fn depth_of_a_scalar_is_zero() {
    assert_eq!(Value::from(1_i64).depth(), 0);
    assert_eq!(Value::None.depth(), 0);
}

#[test]
fn depth_counts_the_deepest_branch() {
    // A list holding a scalar and a nested list: depth comes from the
    // deeper branch, not the shallower one.
    let shallow = Value::from("a");
    let deep = Value::List(vec![Value::List(vec![Value::from(1_i64)])]);
    let value = Value::List(vec![shallow, deep]);
    assert_eq!(value.depth(), 3);
}

#[test]
fn depth_exactly_at_the_limit_on_a_2mib_stack() {
    run_on_2mib_stack(|| {
        let value = nested_list(MAX_DEPTH);
        assert_eq!(value.depth(), MAX_DEPTH);
    });
}

#[test]
fn depth_one_past_the_limit_on_a_2mib_stack() {
    run_on_2mib_stack(|| {
        let value = nested_list(MAX_DEPTH + 1);
        assert_eq!(value.depth(), MAX_DEPTH + 1);
    });
}

/// `depth()` itself never recurses, so it must stay cheap and crash-free
/// far past any limit a caller would actually enforce — the explicit
/// work stack is heap-allocated, not call-stack-bound.
#[test]
fn depth_survives_far_past_the_limit_on_a_2mib_stack() {
    run_on_2mib_stack(|| {
        let value = nested_list(200_000);
        assert_eq!(value.depth(), 200_000);
    });
}

#[test]
fn drop_at_exactly_the_limit_does_not_overflow_a_2mib_stack() {
    run_on_2mib_stack(|| {
        drop(nested_list(MAX_DEPTH));
    });
}

#[test]
fn drop_one_past_the_limit_does_not_overflow_a_2mib_stack() {
    run_on_2mib_stack(|| {
        drop(nested_list(MAX_DEPTH + 1));
    });
}

/// The real point of the iterative rewrite: a `Value` far deeper than
/// anything this workspace's own readers could ever produce (so deep that
/// the *default*, recursive `Drop` glue would abort this entire test
/// process, not just fail one assertion) still drops cleanly on the
/// default 2MiB stack a spawned thread gets.
#[test]
fn drop_of_a_pathologically_deep_value_does_not_overflow_a_2mib_stack() {
    run_on_2mib_stack(|| {
        drop(nested_list(500_000));
    });
}

/// A `Dict` nested inside itself (as both key-chain and value-chain) also
/// has to drop iteratively — `Drop`'s own `pending` stack flattens both
/// a `Dict`'s keys and its values, not just a `List`'s items.
#[test]
fn drop_of_a_deeply_nested_dict_does_not_overflow_a_2mib_stack() {
    run_on_2mib_stack(|| {
        let mut v = Value::from(1_i64);
        for _ in 0..500_000 {
            let mut d = electricity_value::Dict::new();
            d.insert(Value::from("k"), v);
            v = Value::Dict(d);
        }
        drop(v);
    });
}
