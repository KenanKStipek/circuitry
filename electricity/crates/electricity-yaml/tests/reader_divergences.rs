//! Pins the two documented divergences (`lib.rs`'s "Known divergences")
//! this crate's golden-corpus `known_divergence` kind can't express:
//! Circuitry's own loader *fails* on each of these, while
//! electricity-yaml successfully loads a value -- the opposite of every
//! `known_divergence` case in `tests/golden/corpus.json`, which all have
//! electricity-yaml on the failing side. D4 (PR #388's third review)
//! notes the corpus schema's own gap; these two cases are pinned here
//! instead, directly against the behaviour, rather than left undocumented.

use electricity_value::Value;
use electricity_yaml::load_yaml;

/// A line-break character *inside a plain scalar* (not a comment):
/// Circuitry's own scanner treats U+2028 (LINE SEPARATOR) as a line
/// break even mid-scalar, which a plain (unquoted) scalar can't contain
/// without folding -- a `ScannerError` ("could not find expected ':'").
/// `saphyr-parser` 0.1.0 only ever counts `\r`/`\n` as line breaks, so
/// it reads the whole thing as one scalar, U+2028 included.
#[test]
fn unicode_line_separator_inside_a_plain_scalar_loads_here_but_not_in_circuitry() {
    let value =
        load_yaml("a: x\u{2028}y\n").expect("saphyr-parser doesn't treat U+2028 as a break");
    assert_eq!(value.py_repr(), "{'a': 'x\\u2028y'}");
}

/// A lone `\r` (not followed by `\n`) separating two block-mapping
/// entries: Circuitry's own reader counts it as a line break (same as
/// `\n`), so `a: &x 1` and `b: &x 2` are two separate lines, and
/// `&x` reused on the second is Circuitry's own duplicate-anchor error.
/// `saphyr-parser` 0.1.0 doesn't count a lone `\r` as a line break at
/// all, so both entries read as a single line and `&x`'s second use
/// here is an ordinary (non-conflicting) anchor on an entirely
/// different key, not a reuse.
#[test]
fn lone_cr_is_not_a_line_break_here_so_the_anchor_reuse_circuitry_rejects_loads() {
    let value = load_yaml("a: &x 1\rb: &x 2\r").expect("saphyr-parser doesn't split on a lone \\r");
    assert_eq!(
        value,
        Value::Dict(dict_from([
            ("a", Value::Int(1.into())),
            ("b", Value::Int(2.into()))
        ]))
    );
}

fn dict_from(pairs: impl IntoIterator<Item = (&'static str, Value)>) -> electricity_value::Dict {
    let mut dict = electricity_value::Dict::new();
    for (k, v) in pairs {
        dict.insert(Value::Str(k.to_string()), v);
    }
    dict
}
