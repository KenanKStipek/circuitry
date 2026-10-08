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

fn nested_dict(depth: usize) -> Value {
    let mut v = Value::from(1_i64);
    for _ in 0..depth {
        let mut d = electricity_value::Dict::new();
        d.insert(Value::from("k"), v);
        v = Value::Dict(d);
    }
    v
}

fn hash_of(v: &Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// A `Dict` nested inside itself (as both key-chain and value-chain) also
/// has to drop iteratively — `Drop`'s own `pending` stack flattens both
/// a `Dict`'s keys and its values, not just a `List`'s items.
/// The shortcut that skips building an explicit `pending` stack for a
/// `List`/`Dict` with no container child (`lib.rs`'s own `Drop` docs)
/// must still drop every scalar field -- nothing here overflows a 2 MiB
/// stack either way (there is nothing to recurse into), so this is a
/// correctness check, not a stack-safety one.
#[test]
fn drop_of_a_flat_dict_and_list_drops_every_scalar() {
    let mut d = electricity_value::Dict::new();
    d.insert(Value::from("a"), Value::from(1_i64));
    d.insert(Value::from("b"), Value::Str("x".repeat(64)));
    drop(Value::Dict(d));
    drop(Value::List(vec![
        Value::from(1_i64),
        Value::Str("x".repeat(64)),
        Value::None,
    ]));
}

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

/// `py_str`/`py_repr`/equality/hashing/`py_partial_cmp` all still recurse
/// (`lib.rs`'s own module docs on the `depth ≤ MAX_DEPTH` invariant they
/// assume rather than enforce) -- unlike `Drop`, which is iterative
/// regardless of depth. A value at exactly `MAX_DEPTH` is the one depth
/// every caller that checks [`Value::depth`] first is expected to still
/// call these with, so each must stay safe there on the same 2 MiB
/// stack `Drop`'s own tests above use, for both a `List` and a `Dict`
/// chain.
#[test]
fn py_str_at_exactly_the_limit_on_a_2mib_stack() {
    run_on_2mib_stack(|| {
        assert!(!nested_list(MAX_DEPTH).py_str().is_empty());
        assert!(!nested_dict(MAX_DEPTH).py_str().is_empty());
    });
}

#[test]
fn py_repr_at_exactly_the_limit_on_a_2mib_stack() {
    run_on_2mib_stack(|| {
        assert!(!nested_list(MAX_DEPTH).py_repr().is_empty());
        assert!(!nested_dict(MAX_DEPTH).py_repr().is_empty());
    });
}

#[test]
fn equality_at_exactly_the_limit_on_a_2mib_stack() {
    run_on_2mib_stack(|| {
        assert!(nested_list(MAX_DEPTH).py_eq(&nested_list(MAX_DEPTH)));
        assert!(nested_dict(MAX_DEPTH).py_eq(&nested_dict(MAX_DEPTH)));
    });
}

#[test]
fn hash_at_exactly_the_limit_on_a_2mib_stack() {
    run_on_2mib_stack(|| {
        assert_eq!(
            hash_of(&nested_list(MAX_DEPTH)),
            hash_of(&nested_list(MAX_DEPTH))
        );
        assert_eq!(
            hash_of(&nested_dict(MAX_DEPTH)),
            hash_of(&nested_dict(MAX_DEPTH))
        );
    });
}

#[test]
fn py_partial_cmp_at_exactly_the_limit_on_a_2mib_stack() {
    run_on_2mib_stack(|| {
        assert_eq!(
            nested_list(MAX_DEPTH)
                .py_partial_cmp(&nested_list(MAX_DEPTH))
                .unwrap(),
            Some(std::cmp::Ordering::Equal)
        );
    });
}

#[test]
fn depth_of_nested_empty_lists_is_off_by_one_from_bracket_count() {
    // 513 nested lists where the innermost is empty: depth() == 512
    // (MAX_DEPTH), documented on `Value::depth` itself as one less than
    // the bracket count `electricity-json` would enforce for the same
    // shape.
    let mut v = Value::List(vec![]);
    for _ in 0..512 {
        v = Value::List(vec![v]);
    }
    assert_eq!(v.depth(), MAX_DEPTH);
}
