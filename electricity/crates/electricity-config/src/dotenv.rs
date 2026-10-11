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

fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| Patterns {
        // `_whitespace`/`_multiline_whitespace` in dotenv/parser.py --
        // `\s` already spans newlines, so the two patterns (one is
        // `re.MULTILINE`, which only changes `^`/`$`, present nowhere
        // in either) are identical; one pattern covers both call sites.
        whitespace: anchored(r"[^\S\r\n]*"),
        export: anchored(r"(?:export[^\S\r\n]+)?"),
        single_quoted_key: anchored(r"'([^']+)'"),
        unquoted_key: anchored(r"([^=\#\s]+)"),
        equal_sign: anchored(r"(=[^\S\r\n]*)"),
        single_quoted_value: anchored(r"(?s)'((?:\\.|[^'\\])*)'"),
        double_quoted_value: anchored(r#"(?s)"((?:\\.|[^"\\])*)""#),
        unquoted_value: anchored(r"([^\r\n]*)"),
        comment: anchored(r"(?:[^\S\r\n]*#[^\r\n]*)?"),
        end_of_line: anchored(r"[^\S\r\n]*(?:\r\n|\n|\r|$)"),
        rest_of_line: anchored(r"[^\r\n]*(?:\r\n|\n|\r)?"),
        double_quote_escapes: Regex::new(r#"\\[\\'"abfnrtv]"#).expect("valid regex literal"),
        single_quote_escapes: Regex::new(r"\\[\\']").expect("valid regex literal"),
        unquoted_trailing_comment: Regex::new(r"\s+#.*").expect("valid regex literal"),
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
            Ok(truncated.trim_end().to_string())
        }
    }
}

/// `dotenv/main.py::DotEnv.parse`: every `KEY=value` (or bare `KEY`)
/// binding in *text*, in file order, with a repeated key keeping its
/// first position but taking the later value. The UTF-8 BOM, if
/// present, is stripped once up front (`Reader.__init__`'s own
/// `removeprefix("\ufeff")`) -- nothing else in this module reads
/// *text* before this does.
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

/// `dotenv.dotenv_values(path)` -- [`parse`] then [`interpolate`]
/// against *real_env* (the real process environment, as
/// `cli/config.py::load_user_env`'s own caller has it).
pub fn dotenv_values(text: &str, real_env: &HashMap<String, String>) -> RawValues {
    interpolate(parse(text), real_env)
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
}
