//! Python `re` pattern-syntax translation (#448 lane D2, issue #479),
//! shared by `electricity-tools::regex` (the `regex` tool plugin) and
//! `electricity-schema::engine` (JSON Schema `pattern`/
//! `patternProperties`/`propertyNames.pattern`, and the
//! `additionalProperties`+`patternProperties` filter) -- the one place
//! either crate needs to turn Python `re` syntax into `fancy-regex`
//! syntax, moved out from under `electricity-tools` (PR #502's round-4
//! fix) so `electricity-schema` can depend on it without
//! also pulling in every other tool plugin.
//!
//! See [`translate`]'s own module docs for what this does and does not
//! translate, and `electricity/DESIGN.md` §7.3/§16 for the accepted,
//! documented parity gaps.

mod backtrack;
mod regex_unicode;
pub mod translate;

pub use backtrack::backtrack_limit_for;

fn in_ranges(ranges: &[(u32, u32)], code: u32) -> bool {
    ranges
        .binary_search_by(|&(lo, hi)| {
            if code < lo {
                std::cmp::Ordering::Greater
            } else if code > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// `regex_unicode::WORD_RANGES` (CPython 3.11's `str.isalnum()`, kept up
/// to date by `generate_regex_unicode_tables.py`) plus `_`, as a direct
/// per-character membership test rather than a rendered character
/// class -- for a caller that needs to test one character at a time
/// (not build a pattern) without Rust's own `char::is_alphanumeric`,
/// a different, wider table than CPython's (it also counts combining
/// marks).
pub fn is_py_word_char(ch: char) -> bool {
    ch == '_' || in_ranges(regex_unicode::WORD_RANGES, ch as u32)
}
