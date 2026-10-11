//! A Rust port of `python-dotenv`'s `dotenv_values` (the pinned version
//! in `electricity/scripts/generator-constraints.txt`) -- the exact
//! parsing/interpolation rules `cli/config.py::load_user_env` (issue
//! #468) relies on: quoting, backslash escapes, the `export ` prefix,
//! comments, multi-line (double/single-quoted) values, and `${VAR}`
//! POSIX-style interpolation with an optional `:-default`, always on
//! (`dotenv_values`'s own `interpolate=True` default).
//!
//! [`parse`] is a line-oriented transliteration of `dotenv/parser.py`'s
//! `Reader`/`parse_binding` (regex-driven, anchored at the current
//! read position by re-slicing the remaining text rather than porting
//! `Reader`'s own `Position`/line-counting, which only python-dotenv's
//! *own* internal warning ever needed -- see the note below). A line
//! that fails to parse (an unterminated quote, stray text after a
//! quoted value, ...) is dropped silently, exactly as a real `cof`
//! load does: python-dotenv logs its own "could not parse statement"
//! warning through a `dotenv.main`-named logger, which `cli/
//! logging_setup.py::configure_cli_logging` never attaches a handler
//! to (it only touches the `"circuitry"` logger) -- so that warning
//! never reaches a real terminal, and this port reproduces the same
//! silence rather than inventing a warning cof never prints.
//!
//! [`interpolate`] ports `dotenv/main.py::resolve_variables` with
//! `override=True` -- `dotenv_values` always passes that literal, never
//! the caller-configurable value its `DotEnv.set_as_environment_
//! variables` leaves adjustable -- so a `${VAR}` reference resolves
//! against the real environment overridden by any name already
//! resolved earlier in the same file, in file order.
//!
//! [`dotenv_values`] -- this module's own entry point -- first runs
//! [`normalize_newlines`] on *text*, reproducing `DotEnv._get_stream`'s
//! own `open(self.dotenv_path, encoding=self.encoding)`: a Python text
//! file, opened with the default `newline=None`, translates `\r\n`
//! and a lone `\r` to `\n` as it reads, before `Reader` ever sees a
//! single character. `load_user_env`'s own `dotenv_values(path)` call
//! always goes through that file-path branch, never the raw-`stream`
//! one (which skips the translation) -- this port's sole caller reads
//! the file itself and hands over already-decoded text, so it has to
//! do the same translation explicitly, or a multi-line quoted value
//! saved on Windows (or any text with a lone `\r`) would carry a
//! stray `\r` cof's own copy never has.
//!
//! Every place the reference matches `\s`, or `[^\S\r\n]` (its own
//! negation), [`is_py_whitespace`]'s doc comment has the exact 29
//! codepoints that means, re-derived against the pinned Python/`re`
//! version rather than assumed: it is not Rust's `char::is_whitespace`
//! (missing four C0 control codes, `\x1c`-`\x1f`) and not `regex`'s own
//! Unicode `\s` either (same four missing, scoped to the `White_Space`
//! property). A key like `A\x1fB` -- containing one of those four --
//! is whitespace to Python's own parser (so `A\x1fB=1` fails to parse
//! as a binding, `unquoted_key`'s own `[^=\#\s]+` stopping dead at
//! `A`, with no `=` immediately after) and must be whitespace here
//! too, or this port would load a key cof's own copy silently drops.

use indexmap::IndexMap;
use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

/// One `KEY=value` (or bare `KEY`, value `None`) binding, in the order
/// the file defines it. A repeated key keeps its *first* position but
/// takes the new value -- an `IndexMap::insert` of an existing key
/// already behaves this way, matching a Python `dict`'s own
/// `__setitem__`.
pub type RawValues = IndexMap<String, Option<String>>;

struct Patterns {
    whitespace: Regex,
    export: Regex,
    single_quoted_key: Regex,
    unquoted_key: Regex,
    equal_sign: Regex,
    single_quoted_value: Regex,
    double_quoted_value: Regex,
    unquoted_value: Regex,
    comment: Regex,
    end_of_line: Regex,
    rest_of_line: Regex,
    double_quote_escapes: Regex,
    single_quote_escapes: Regex,
    unquoted_trailing_comment: Regex,
    posix_variable: Regex,
}

/// *pattern*, anchored to the very start of whatever text it is matched
/// against -- every call site below re-slices the remaining text to the
/// current read position first, so "anchored" here reproduces
/// `Reader.read_regex`'s own `Pattern.match(string, pos)` (which never
/// searches past *pos*) without porting `Reader`'s own `Position` type.
fn anchored(pattern: &str) -> Regex {
    Regex::new(&format!("^(?:{pattern})")).expect("valid regex literal")
}

/// `\r\n` or a lone `\r`, translated to `\n` -- `open(path,
/// encoding=...)`'s own universal-newline read, this module's doc
/// comment has the full reasoning. Order matters: `\r\n` must collapse
/// to a single `\n` first, or the lone-`\r` pass below would leave the
/// line doubled (`\r\n` -> `\n\n`).
fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Every codepoint Python's `re.match(r"\s", ...)` (`re.UNICODE`, the
/// implicit default `dotenv/parser.py`'s own `make_regex` always uses)
/// and `str.isspace()` agree is whitespace: the six ASCII controls
/// (`\t\n\x0b\x0c\r`, plus the four C0 separators `\x1c`-`\x1f` most
/// whitespace definitions skip), the space itself, NEL (`\u0085`), and
/// the Unicode `Zs`-category separators (NBSP, the `\u2000`-`\u200a`
/// family, line/paragraph separator, narrow/medium-mathematical space,
/// ideographic space) -- enumerated once by brute-force probing every
/// codepoint against the pinned CPython version, not read out of any
/// Unicode property table, since Python's own `\s` isn't exactly one
/// (`electricity/scripts/generate_dotenv_corpus.py`'s own whitespace
/// probe keeps that enumeration honest against a version bump). Rust's
/// `char::is_whitespace` and `regex`'s own Unicode `\s` both use the
/// `White_Space` Unicode property instead, which omits the four C0
/// separators -- the one place this set and that property disagree.
fn is_py_whitespace(c: char) -> bool {
    matches!(c, '\u{9}'..='\u{d}' | '\u{1c}'..='\u{20}' | '\u{85}' | '\u{a0}')
        || matches!(
            c,
            '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}'
        )
        || matches!(c, '\u{202f}' | '\u{205f}' | '\u{3000}')
}

/// `str.rstrip()` with no arguments, scoped to [`is_py_whitespace`] --
/// `parse_unquoted_value`'s own final step. The value it trims never
/// holds `\r`/`\n` (captured by `_unquoted_value`'s own `[^\r\n]*`,
/// before this ever runs), so trimming the full whitespace set here
/// rather than excluding those two changes nothing in practice; this
/// just avoids a second, narrower constant no call site would ever need.
fn py_trim_end(s: &str) -> &str {
    s.trim_end_matches(is_py_whitespace)
}

/// A regex character class matching exactly [`is_py_whitespace`]'s own
/// set, built once -- every pattern below that stands in for `\s` (or
/// its negation, `[^\S\r\n]`) is built from this string rather than
/// `regex`'s own `\s`, which disagrees on the four C0 separators (see
/// [`is_py_whitespace`]). `\x1c-\x1f` is a range between two `\xHH`
/// escapes (exactly two hex digits each); everything above `\x7f`
/// needs the brace form (`\u{HHHH}`).
const PY_WS_CLASS: &str = r"\t\n\x0b\x0c\r\x1c-\x1f \u{0085}\u{00a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}";

/// [`PY_WS_CLASS`] less `\r`/`\n` -- `[^\S\r\n]`, `dotenv/parser.py`'s
/// own negation of `\s` to mean "whitespace that isn't a newline".
const PY_WS_CLASS_NO_NEWLINE: &str = r"\t\x0b\x0c\x1c-\x1f \u{0085}\u{00a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}";

fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| Patterns {
        // `_whitespace` in dotenv/parser.py is `[^\S\r\n]*` (never
        // consumes a newline); `_multiline_whitespace` is `\s*` under
        // `re.MULTILINE` (which only changes `^`/`$`, matched nowhere
        // in either pattern), so it *does* consume one -- a blank line
        // simply vanishes before `parse_key` ever runs. This port uses
        // `[^\S\r\n]*` at both call sites instead: at
        // `_multiline_whitespace`'s own site (the very start of
        // `parse_binding`), a blank line is left for `parse_key` to
        // fail on (`unquoted_key`'s `[^=\#\s]+` can't match a leading
        // newline), which falls through to `error_binding`'s own
        // `_rest_of_line` read -- consuming exactly the same text
        // `_multiline_whitespace` would have, just one read later. The
        // two patterns would only ever disagree if a key could start
        // with `\r`/`\n`, which no unquoted or single-quoted key
        // pattern allows.
        whitespace: anchored(&format!("[{PY_WS_CLASS_NO_NEWLINE}]*")),
        export: anchored(&format!("(?:export[{PY_WS_CLASS_NO_NEWLINE}]+)?")),
        single_quoted_key: anchored(r"'([^']+)'"),
        unquoted_key: anchored(&format!("([^=\\#{PY_WS_CLASS}]+)")),
        equal_sign: anchored(&format!("(=[{PY_WS_CLASS_NO_NEWLINE}]*)")),
        single_quoted_value: anchored(r"(?s)'((?:\\.|[^'\\])*)'"),
        double_quoted_value: anchored(r#"(?s)"((?:\\.|[^"\\])*)""#),
        unquoted_value: anchored(r"([^\r\n]*)"),
        comment: anchored(&format!("(?:[{PY_WS_CLASS_NO_NEWLINE}]*#[^\\r\\n]*)?")),
        end_of_line: anchored(&format!("[{PY_WS_CLASS_NO_NEWLINE}]*(?:\\r\\n|\\n|\\r|$)")),
        rest_of_line: anchored(r"[^\r\n]*(?:\r\n|\n|\r)?"),
        double_quote_escapes: Regex::new(r#"\\[\\'"abfnrtv]"#).expect("valid regex literal"),
        single_quote_escapes: Regex::new(r"\\[\\']").expect("valid regex literal"),
        unquoted_trailing_comment: Regex::new(&format!("[{PY_WS_CLASS}]+#.*"))
            .expect("valid regex literal"),
        posix_variable: Regex::new(r"\$\{(?P<name>[^}:]*)(?::-(?P<default>[^}]*))?\}")
            .expect("valid regex literal"),
    })
}

/// `dotenv/parser.py::decode_escapes` -- replaces every match of *regex*
/// in *value* with its `codecs.decode(..., "unicode-escape")` result.
/// Only `\\`, `\'`, `\"`, `\a`, `\b`, `\f`, `\n`, `\r`, `\t`, `\v` are
/// ever matched (the two escape regexes above); any other backslash
/// sequence is left exactly as written, byte for byte, same as the
/// reference -- `decode_escapes` only ever substitutes a *match*, never
/// touches text outside one.
fn decode_escapes(regex: &Regex, value: &str) -> String {
    regex
        .replace_all(value, |caps: &regex::Captures| {
            let matched = &caps[0];
            let escaped = matched.chars().nth(1).expect("escape is backslash+char");
            match escaped {
                '\\' => "\\".to_string(),
                '\'' => "'".to_string(),
                '"' => "\"".to_string(),
                'a' => "\u{7}".to_string(),
                'b' => "\u{8}".to_string(),
                'f' => "\u{c}".to_string(),
                'n' => "\n".to_string(),
                'r' => "\r".to_string(),
                't' => "\t".to_string(),
                'v' => "\u{b}".to_string(),
                other => other.to_string(),
            }
        })
        .into_owned()
}

/// One attempted `KEY=value` line, starting at byte offset *pos* in
/// *text*. `None` is "no key on this line" (blank, a stray comment, or
/// a parse error) -- `dotenv/parser.py::parse_stream` only ever yields
/// a binding whose `key` is not `None`, so a `None` here is simply
/// never added to [`RawValues`].
struct Binding {
    key: Option<String>,
    value: Option<String>,
}

/// `dotenv/parser.py::parse_binding`, less its own line-number
/// bookkeeping (see this module's doc comment). Returns the binding and
/// the new cursor position.
fn parse_binding(text: &str, pos: usize) -> (Binding, usize) {
    let p = patterns();
    let mut cursor = pos;

    macro_rules! read {
        ($regex:expr) => {{
            match $regex.captures(&text[cursor..]) {
                Some(caps) => {
                    let whole = caps.get(0).expect("group 0 always present");
                    cursor += whole.end();
                    Some(caps)
                }
                None => None,
            }
        }};
    }

    // `reader.read_regex(_multiline_whitespace)` -- always matches
    // (`\s*`), so `read!` never returns `None` here.
    read!(p.whitespace);
    if cursor >= text.len() {
        return (
            Binding {
                key: None,
                value: None,
            },
            cursor,
        );
    }

    // `reader.read_regex(_export)` -- always matches (optional group).
    read!(p.export);

    // `parse_key`
    let key: Option<String> = if text[cursor..].starts_with('#') {
        None
    } else if text[cursor..].starts_with('\'') {
        match read!(p.single_quoted_key) {
            Some(caps) => Some(caps[1].to_string()),
            None => return error_binding(text, cursor),
        }
    } else {
        match read!(p.unquoted_key) {
            Some(caps) => Some(caps[1].to_string()),
            None => return error_binding(text, cursor),
        }
    };

    // `reader.read_regex(_whitespace)`
    read!(p.whitespace);

    let value: Option<String> = if text[cursor..].starts_with('=') {
        let equal_sign = read!(p.equal_sign).expect("= was just peeked");
        let equal_sign_text = &equal_sign[1];
        if equal_sign_text.len() > 1 && text[cursor..].starts_with('#') {
            Some(String::new())
        } else {
            match parse_value(text, &mut cursor) {
                Ok(value) => Some(value),
                Err(()) => return error_binding(text, cursor),
            }
        }
    } else {
        None
    };

    // `reader.read_regex(_comment)` -- always matches (optional group).
    read!(p.comment);
    // `reader.read_regex(_end_of_line)` -- the one read that can fail
    // (stray text after a quoted value with no `#` introducing it).
    if read!(p.end_of_line).is_none() {
        return error_binding(text, cursor);
    }

    (Binding { key, value }, cursor)
}

/// On any parse failure, `dotenv/parser.py::parse_binding` discards
/// everything read so far and consumes the rest of the physical line
/// instead (`_rest_of_line`) -- `reader`'s own position is wherever
/// the failing read left it (mid-line, not rewound to the line's own
/// start), which is exactly what *cursor* already is at every call
/// site below.
fn error_binding(text: &str, cursor: usize) -> (Binding, usize) {
    let p = patterns();
    let mut cursor = cursor;
    if let Some(m) = p.rest_of_line.find(&text[cursor..]) {
        cursor += m.end();
    }
    (
        Binding {
            key: None,
            value: None,
        },
        cursor,
    )
}

/// `parse_value` -- the character at *cursor* selects single-quoted,
/// double-quoted, "nothing on this line" or unquoted parsing. `Err(())`
/// is an unterminated quote (no matching closing quote before the file
/// ends), the one way this can fail.
fn parse_value(text: &str, cursor: &mut usize) -> Result<String, ()> {
    let p = patterns();
    let rest = &text[*cursor..];
    let first = rest.chars().next();
    match first {
        Some('\'') => match p.single_quoted_value.captures(rest) {
            Some(caps) => {
                *cursor += caps.get(0).unwrap().end();
                Ok(decode_escapes(&p.single_quote_escapes, &caps[1]))
            }
            None => Err(()),
        },
        Some('"') => match p.double_quoted_value.captures(rest) {
            Some(caps) => {
                *cursor += caps.get(0).unwrap().end();
                Ok(decode_escapes(&p.double_quote_escapes, &caps[1]))
            }
            None => Err(()),
        },
        None | Some('\n') | Some('\r') => Ok(String::new()),
        _ => {
            let caps = p
                .unquoted_value
                .captures(rest)
                .expect("[^\\r\\n]* always matches");
            *cursor += caps.get(0).unwrap().end();
            let part = &caps[1];
            let truncated = match p.unquoted_trailing_comment.find(part) {
                Some(m) => &part[..m.start()],
                None => part,
            };
            Ok(py_trim_end(truncated).to_string())
        }
    }
}

/// `dotenv/main.py::DotEnv.parse`: every `KEY=value` (or bare `KEY`)
/// binding in *text*, in file order, with a repeated key keeping its
/// first position but taking the later value. The UTF-8 BOM, if
/// present, is stripped once up front (`Reader.__init__`'s own
/// `removeprefix("\ufeff")`) -- nothing else in this module reads
/// *text* before this does. *text* is expected already
/// newline-normalized ([`dotenv_values`] is the only caller, and does
/// that first) -- `parse` on its own never sees a `\r`.
pub fn parse(text: &str) -> RawValues {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut result: RawValues = IndexMap::new();
    let mut pos = 0usize;
    while pos < text.len() {
        let (binding, next_pos) = parse_binding(text, pos);
        debug_assert!(
            next_pos > pos || pos >= text.len(),
            "parse_binding must always advance past a non-empty remainder"
        );
        if let Some(key) = binding.key {
            result.insert(key, binding.value);
        }
        if next_pos <= pos {
            // Guards against a pathological non-advancing match (would
            // otherwise loop forever); python-dotenv's own `Reader`
            // always advances because every one of its regexes either
            // consumes at least one character or is the final fallback
            // inside `error_binding`'s `_rest_of_line`, which does the
            // same -- not expected to trigger, kept for safety.
            break;
        }
        pos = next_pos;
    }
    result
}

/// `dotenv/main.py::resolve_variables` with `override=True` -- the one
/// literal `dotenv_values` ever calls it with (its own `DotEnv.dict`
/// takes `override` from `DotEnv.__init__`'s default of `True`, and
/// `dotenv_values` never overrides that default). A `${VAR}` reference
/// resolves against *real_env*, overridden by any name this file has
/// already defined earlier in iteration order (`new_values` winning
/// over `os.environ` is `resolve_variables`'s own `override=True`
/// branch) -- a name resolved to "no value" (a bare `KEY` with no `=`)
/// still counts as present, resolving a reference to it as `""` rather
/// than falling through to that reference's own `:-default`.
pub fn interpolate(raw: RawValues, real_env: &HashMap<String, String>) -> RawValues {
    let p = patterns();
    let mut resolved: RawValues = IndexMap::new();
    for (name, value) in raw {
        let new_value = value.map(|v| {
            let mut out = String::new();
            let mut cursor = 0usize;
            for caps in p.posix_variable.captures_iter(&v) {
                let whole = caps.get(0).expect("group 0 always present");
                out.push_str(&v[cursor..whole.start()]);
                let var_name = &caps["name"];
                let default = caps.name("default").map(|d| d.as_str());
                let looked_up: Option<Option<&str>> = if let Some(val) = resolved.get(var_name) {
                    Some(val.as_deref())
                } else if let Some(val) = real_env.get(var_name) {
                    Some(Some(val.as_str()))
                } else {
                    None
                };
                let piece = match looked_up {
                    Some(Some(val)) => val,
                    Some(None) => "",
                    None => default.unwrap_or(""),
                };
                out.push_str(piece);
                cursor = whole.end();
            }
            out.push_str(&v[cursor..]);
            out
        });
        resolved.insert(name, new_value);
    }
    resolved
}

/// `dotenv.dotenv_values(path)` -- [`normalize_newlines`], then
/// [`parse`], then [`interpolate`] against *real_env* (the real
/// process environment, as `cli/config.py::load_user_env`'s own
/// caller has it).
pub fn dotenv_values(text: &str, real_env: &HashMap<String, String>) -> RawValues {
    let text = normalize_newlines(text);
    interpolate(parse(&text), real_env)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn simple_key_value() {
        let values = dotenv_values("FOO=bar\n", &env(&[]));
        assert_eq!(values.get("FOO"), Some(&Some("bar".to_string())));
    }

    #[test]
    fn export_prefix_and_comments() {
        let values = dotenv_values("# a comment\nexport FOO=bar # trailing\n", &env(&[]));
        assert_eq!(values.get("FOO"), Some(&Some("bar".to_string())));
    }

    #[test]
    fn single_quotes_are_also_interpolated() {
        let values = dotenv_values("A=hello\nB='${A} world'\n", &env(&[]));
        assert_eq!(values.get("B"), Some(&Some("hello world".to_string())));
    }

    #[test]
    fn double_quote_escapes_decode() {
        let values = dotenv_values("FOO=\"a\\nb\"\n", &env(&[]));
        assert_eq!(values.get("FOO"), Some(&Some("a\nb".to_string())));
    }

    #[test]
    fn single_quote_only_decodes_backslash_and_quote() {
        let values = dotenv_values("FOO='a\\nb'\n", &env(&[]));
        assert_eq!(values.get("FOO"), Some(&Some("a\\nb".to_string())));
    }

    #[test]
    fn multiline_double_quoted_value() {
        let values = dotenv_values("FOO=\"line1\nline2\"\n", &env(&[]));
        assert_eq!(values.get("FOO"), Some(&Some("line1\nline2".to_string())));
    }

    #[test]
    fn interpolation_defaults_and_real_env() {
        let values = dotenv_values(
            "A=${REAL}\nB=${MISSING:-fallback}\n",
            &env(&[("REAL", "x")]),
        );
        assert_eq!(values.get("A"), Some(&Some("x".to_string())));
        assert_eq!(values.get("B"), Some(&Some("fallback".to_string())));
    }

    #[test]
    fn bare_key_resolves_empty_not_default() {
        let values = dotenv_values("A\nB=${A:-fallback}\n", &env(&[]));
        assert_eq!(values.get("A"), Some(&None));
        assert_eq!(values.get("B"), Some(&Some(String::new())));
    }

    #[test]
    fn repeated_key_keeps_first_position_but_last_value() {
        let values = dotenv_values("A=1\nB=2\nA=3\n", &env(&[]));
        let keys: Vec<&str> = values.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["A", "B"]);
        assert_eq!(values.get("A"), Some(&Some("3".to_string())));
    }

    #[test]
    fn malformed_line_is_dropped_silently() {
        let values = dotenv_values("FOO='unterminated\nBAR=baz\n", &env(&[]));
        assert_eq!(values.get("FOO"), None);
        assert_eq!(values.get("BAR"), Some(&Some("baz".to_string())));
    }

    #[test]
    fn crlf_inside_a_multiline_quoted_value_is_normalized_to_lf() {
        let values = dotenv_values("K=\"a\r\nb\"\r\n", &env(&[]));
        assert_eq!(values.get("K"), Some(&Some("a\nb".to_string())));
    }

    #[test]
    fn lone_cr_inside_a_quoted_value_is_also_normalized() {
        let values = dotenv_values("K='x\ry'\n", &env(&[]));
        assert_eq!(values.get("K"), Some(&Some("x\ny".to_string())));
    }

    #[test]
    fn a_literal_backslash_just_before_crlf_leaves_the_backslash_and_normalizes_the_newline() {
        let values = dotenv_values("K=\"a\\\r\nb\"\r\n", &env(&[]));
        assert_eq!(values.get("K"), Some(&Some("a\\\nb".to_string())));
    }

    #[test]
    fn a_c0_separator_in_an_unquoted_key_is_whitespace_like_python_and_drops_the_line() {
        let values = dotenv_values("A\u{1f}B=1\n", &env(&[]));
        assert!(values.is_empty());
    }
}
