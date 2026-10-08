//! Pins the one documented divergence (`lib.rs`'s "Known divergences")
//! this crate's golden-corpus `known_divergence` kind can't express:
//! Circuitry's own loader *fails* on it, while electricity-yaml
//! successfully loads a value -- the opposite of every
//! `known_divergence` case in `tests/golden/corpus.json`, which all have
//! electricity-yaml on the failing side. D4 (PR #388's third review)
//! notes the corpus schema's own gap; this case is pinned here instead,
//! directly against the behaviour, rather than left undocumented.
//!
//! A lone `\r` (not followed by `\n`) is *not* a divergence -- both
//! loaders count it as a line break, same as `\n` (`char_traits.rs`'s
//! `is_break`, `Scanner::skip_linebreak`/`skip_nl`; PyYAML's own
//! `scan_line_break`) -- so a reused anchor across one is an ordinary
//! duplicate-anchor error in both, exercised as a parity case in
//! `generate_yaml_corpus.py`'s `anchor_cases` rather than here.

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
