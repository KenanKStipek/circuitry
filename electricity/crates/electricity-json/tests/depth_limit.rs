//! Nesting depth must be bounded by a fixed, documented limit
//! ([`electricity_json::MAX_DEPTH`]), not by the thread's native stack
//! size — see the crate's module docs on why this diverges from
//! CPython's own `sys.getrecursionlimit()`-based limit. A plain `#[test]`
//! is enough here: these would abort the whole test process on a stack
//! overflow instead of merely failing, if the depth limit regressed.

use electricity_json::{MAX_DEPTH, ReadError, WriteError, WriteMode, dumps, load_json, loads};
use electricity_value::Value;

#[test]
fn loads_rejects_deep_nesting_without_overflowing_the_stack() {
    let text = "[".repeat(100_000);
    match loads(&text) {
        Err(ReadError::Depth { .. }) => {}
        other => panic!("expected Err(ReadError::Depth), got {other:?}"),
    }
}

#[test]
fn load_json_rejects_deep_nesting_without_overflowing_the_stack() {
    let text = "[".repeat(100_000);
    match load_json(&text) {
        Err(ReadError::Depth { .. }) => {}
        other => panic!("expected Err(ReadError::Depth), got {other:?}"),
    }
}

#[test]
fn loads_accepts_nesting_up_to_the_limit() {
    let text = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
    loads(&text).expect("nesting at exactly MAX_DEPTH must still parse");
}

/// Pins [`MAX_DEPTH`]'s safety margin against a worst-case 2MiB thread
/// stack explicitly, rather than trusting whatever stack size the test
/// harness happens to use on a given platform or `RUST_MIN_STACK` setting.
#[test]
fn loads_accepts_nesting_up_to_the_limit_on_a_2mib_stack() {
    let handle = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let text = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
            loads(&text).expect("nesting at exactly MAX_DEPTH must still parse on a 2MiB stack");
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn dumps_rejects_a_value_nested_past_the_limit() {
    let mut value = Value::List(Vec::new());
    for _ in 0..(MAX_DEPTH + 10) {
        value = Value::List(vec![value]);
    }
    match dumps(&value, WriteMode::COMPACT) {
        Err(WriteError::Depth) => {}
        other => panic!("expected Err(WriteError::Depth), got {other:?}"),
    }
}

#[test]
fn dumps_accepts_a_value_nested_up_to_the_limit() {
    let mut value = Value::List(Vec::new());
    for _ in 0..(MAX_DEPTH - 1) {
        value = Value::List(vec![value]);
    }
    dumps(&value, WriteMode::COMPACT).expect("nesting at exactly MAX_DEPTH must still write");
}

/// The 2MiB-stack proof above only covers [`loads`] parsing arrays. Every
/// recursive path in this crate needs the same margin: [`load_json`]
/// (its own, separate duplicate-key-checked materializer), parsing
/// *objects* rather than arrays (a different frame shape per level), and
/// [`dumps`]'s own recursive writer. This test runs all three, nested to
/// exactly `MAX_DEPTH`, on one 2MiB-stack thread.
#[test]
fn load_json_then_dumps_pretty_round_trip_at_the_limit_on_a_2mib_stack() {
    let handle = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let text = format!(
                "{}{}{}",
                "{\"a\": ".repeat(MAX_DEPTH),
                "0",
                "}".repeat(MAX_DEPTH)
            );
            let value = load_json(&text)
                .expect("object nesting at exactly MAX_DEPTH must still parse on a 2MiB stack");
            dumps(&value, WriteMode::PRETTY)
                .expect("the same nesting must still write on a 2MiB stack");
        })
        .unwrap();
    handle.join().unwrap();
}
