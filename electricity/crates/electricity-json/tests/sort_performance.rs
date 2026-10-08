//! `sort_keys=True` must sort by key in O(n log n), not O(n^2): a run
//! state's saved dict (`core/saved_state.py`'s `json.dumps(saved,
//! indent=2, sort_keys=True)`) is written on every `--out --pretty` run
//! and can realistically hold tens of thousands of keys (DESIGN.md's
//! sort-algorithm decision for #377/#384).

use electricity_json::{WriteMode, dumps};
use electricity_value::{Dict, Value};
use std::time::{Duration, Instant};

#[test]
fn sorting_a_large_reverse_ordered_string_keyed_dict_is_fast() {
    const N: usize = 200_000;
    let mut dict: Dict = Dict::new();
    for i in (0..N).rev() {
        dict.insert(Value::Str(format!("key-{i:08}")), Value::from(i as i64));
    }
    let value = Value::Dict(dict);

    let start = Instant::now();
    let text = dumps(&value, WriteMode::PRETTY).expect("no NaN or incomparable keys here");
    let elapsed = start.elapsed();

    assert!(text.starts_with("{\n  \"key-00000000\": 0,"));
    assert!(
        elapsed < Duration::from_secs(5),
        "sorting {N} string keys took {elapsed:?}; an O(n^2) sort would take vastly longer"
    );
}
