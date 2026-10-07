//! `repr(str)` and `repr(bytes)` parity with CPython 3.11
//! (`Objects/unicodeobject.c::unicode_repr`, `Objects/bytesobject.c::PyBytes_Repr`).

use crate::generated::is_py_printable;

/// Picks `'` unless `s` contains `'` and no `"`, matching CPython's own
/// quote-selection rule for both `str` and `bytes` repr.
fn pick_quote(contains_single: bool, contains_double: bool) -> char {
    if contains_single && !contains_double {
        '"'
    } else {
        '\''
    }
}

/// Formats `s` exactly as CPython 3.11's `repr(s)` for a `str`.
pub fn py_str_repr(s: &str) -> String {
    let quote = pick_quote(s.contains('\''), s.contains('"'));
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if is_py_printable(c) => out.push(c),
            c => {
                let cp = c as u32;
                if cp <= 0xff {
                    out.push_str(&format!("\\x{cp:02x}"));
                } else if cp <= 0xffff {
                    out.push_str(&format!("\\u{cp:04x}"));
                } else {
                    out.push_str(&format!("\\U{cp:08x}"));
                }
            }
        }
    }
    out.push(quote);
    out
}

/// Formats `bytes` exactly as CPython 3.11's `repr(b)` for a `bytes` object:
/// a `b` prefix, then quoting/escaping like `str` repr but over raw bytes
/// (ASCII-printable literal, everything else — including every non-ASCII
/// byte, since `bytes` has no notion of a printable Unicode character —
/// hex-escaped).
pub fn py_bytes_repr(bytes: &[u8]) -> String {
    let quote = pick_quote(bytes.contains(&b'\''), bytes.contains(&b'"'));
    let mut out = String::with_capacity(bytes.len() + 3);
    out.push('b');
    out.push(quote);
    for &b in bytes {
        match b {
            b if b == quote as u8 => {
                out.push('\\');
                out.push(b as char);
            }
            b'\\' => out.push_str("\\\\"),
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out.push(quote);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_quote_is_single() {
        assert_eq!(py_str_repr("abc"), "'abc'");
    }

    #[test]
    fn switches_to_double_quote_when_containing_single_but_not_double() {
        assert_eq!(py_str_repr("it's"), "\"it's\"");
    }

    #[test]
    fn keeps_single_quote_when_containing_both() {
        assert_eq!(py_str_repr("it's \"ok\""), "'it\\'s \"ok\"'");
    }

    #[test]
    fn escapes_backslash_and_mnemonics() {
        assert_eq!(py_str_repr("a\\b\tc\nd\re"), "'a\\\\b\\tc\\nd\\re'");
    }

    #[test]
    fn escapes_control_chars_as_hex() {
        assert_eq!(py_str_repr("\u{0}\u{1}\u{7f}"), "'\\x00\\x01\\x7f'");
    }

    #[test]
    fn keeps_printable_non_ascii_literal() {
        assert_eq!(py_str_repr("café"), "'café'");
    }

    #[test]
    fn escapes_non_printable_above_ascii() {
        // U+00AD SOFT HYPHEN is category Cf (Format), non-printable.
        assert_eq!(py_str_repr("\u{ad}"), "'\\xad'");
        // U+200B ZERO WIDTH SPACE is category Cf, non-printable, > 0xff.
        assert_eq!(py_str_repr("\u{200b}"), "'\\u200b'");
        // U+1FFFE is unassigned (Cn, as of Unicode 14) and non-printable.
        assert_eq!(py_str_repr("\u{1fffe}"), "'\\U0001fffe'");
    }

    #[test]
    fn bytes_repr_default_quote() {
        assert_eq!(py_bytes_repr(b"abc"), "b'abc'");
    }

    #[test]
    fn bytes_repr_escapes_non_ascii() {
        assert_eq!(py_bytes_repr(&[0xff, b'\n', b'\t']), "b'\\xff\\n\\t'");
    }

    #[test]
    fn bytes_repr_switches_quote() {
        assert_eq!(py_bytes_repr(b"it's"), "b\"it's\"");
    }
}
