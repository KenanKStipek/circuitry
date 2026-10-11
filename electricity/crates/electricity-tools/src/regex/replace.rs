//! `re.sub`'s own substitution semantics: the replacement-template
//! parser (`\1`-`\99`, `\g<n>`/`\g<name>`, the template's own escape
//! set, a non-participating group expanding to `''`), `count`'s
//! `int()` coercion (done by the caller, [`crate::regex`], through
//! `electricity_value::pycompat::py_int` -- this module takes the
//! already-coerced integer), and Python's own 3.7+ empty-match
//! iteration rule.
//!
//! Every parse error here propagates *unwrapped* -- no
//! `regex: invalid pattern: ` prefix, since Python's own `re.sub`
//! parses the template at call time, not at `re.compile` time
//! (issue #479's own "Template errors propagate unwrapped" rule); like
//! every other `re.error`-shaped text in this module, it's compared by
//! *location*, never byte for byte.

use electricity_value::Value;
use std::collections::HashMap;

/// One piece of a parsed replacement template.
#[derive(Debug)]
enum Part {
    Literal(String),
    /// `0` is the whole match (`\g<0>`); `n >= 1` is a numbered or
    /// named group, resolved to its number at parse time.
    Group(usize),
}

/// A replacement template, already parsed once (reused across every
/// match `substitute` finds, rather than re-parsed per match -- Python
/// itself does the same, caching `_compile_repl`'s own result for the
/// lifetime of one `sub` call).
#[derive(Debug)]
pub struct Template {
    parts: Vec<Part>,
}

impl Template {
    fn expand(&self, caps: &fancy_regex::Captures) -> String {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                Part::Literal(s) => out.push_str(s),
                Part::Group(n) => {
                    if let Some(m) = caps.get(*n) {
                        out.push_str(m.as_str());
                    }
                }
            }
        }
        out
    }
}

type PResult<T> = Result<T, String>;

/// Parses *repl* against a pattern with *group_count* capturing groups
/// and *group_names* (name -> number) -- both from
/// [`crate::regex::translate::Translated`].
pub fn parse_template(
    repl: &str,
    group_count: usize,
    group_names: &HashMap<String, usize>,
) -> PResult<Template> {
    let chars: Vec<char> = repl.chars().collect();
    let mut i = 0;
    let mut parts = Vec::new();
    let mut literal = String::new();

    fn flush(literal: &mut String, parts: &mut Vec<Part>) {
        if !literal.is_empty() {
            parts.push(Part::Literal(std::mem::take(literal)));
        }
    }

    while i < chars.len() {
        let c = chars[i];
        if c != '\\' {
            literal.push(c);
            i += 1;
            continue;
        }
        let backslash_pos = i;
        i += 1;
        let Some(c2) = chars.get(i).copied() else {
            return Err(format!(
                "bad escape (end of pattern) at position {backslash_pos}"
            ));
        };
        match c2 {
            'g' => {
                i += 1;
                if chars.get(i) != Some(&'<') {
                    return Err(format!("missing < at position {i}"));
                }
                i += 1;
                let name_start = i;
                while chars.get(i).is_some_and(|c| *c != '>') {
                    i += 1;
                }
                if chars.get(i) != Some(&'>') {
                    return Err(format!(
                        "missing >, unterminated name at position {name_start}"
                    ));
                }
                let name: String = chars[name_start..i].iter().collect();
                i += 1;
                if name.is_empty() {
                    return Err(format!("missing group name at position {name_start}"));
                }
                let is_identifier = name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_')
                    && name.chars().all(|c| c.is_alphanumeric() || c == '_');
                let index = if is_identifier && name.parse::<i64>().is_err() {
                    // CPython raises a bare `IndexError` here (not
                    // `re.error`), with no position in the text --
                    // confirmed directly; exact-matched, never
                    // location-compared (module doc comment).
                    *group_names.get(&name).ok_or_else(|| {
                        format!("unknown group name {}", Value::Str(name.clone()).py_repr())
                    })?
                } else {
                    let parsed: i64 = name.parse().map_err(|_| {
                        format!("bad character in group name {name:?} at position {name_start}")
                    })?;
                    if parsed < 0 {
                        return Err(format!(
                            "bad character in group name {name:?} at position {name_start}"
                        ));
                    }
                    parsed as usize
                };
                if index > group_count {
                    return Err(format!(
                        "invalid group reference {index} at position {name_start}"
                    ));
                }
                flush(&mut literal, &mut parts);
                parts.push(Part::Group(index));
            }
            '0' => {
                let mut digits = String::from("0");
                i += 1;
                for _ in 0..2 {
                    if chars.get(i).is_some_and(|c| ('0'..='7').contains(c)) {
                        digits.push(chars[i]);
                        i += 1;
                    } else {
                        break;
                    }
                }
                let value = u32::from_str_radix(&digits, 8).unwrap_or(0) & 0xff;
                literal.push(char::from_u32(value).unwrap_or('\0'));
            }
            '1'..='9' => {
                let mut digits = String::new();
                digits.push(c2);
                i += 1;
                let mut is_octal = false;
                if let Some(second) = chars.get(i).copied() {
                    if second.is_ascii_digit() {
                        let octal_prefix =
                            ('0'..='7').contains(&c2) && ('0'..='7').contains(&second);
                        let third = chars.get(i + 1).copied();
                        if octal_prefix && third.is_some_and(|c| ('0'..='7').contains(&c)) {
                            digits.push(second);
                            i += 1;
                            let third = chars[i];
                            digits.push(third);
                            i += 1;
                            is_octal = true;
                            let value = u32::from_str_radix(&digits, 8).unwrap_or(0);
                            if value > 0o377 {
                                return Err(format!(
                                    "octal escape value \\{digits} outside of range 0-0o377 at position {backslash_pos}"
                                ));
                            }
                            literal.push(char::from_u32(value).unwrap_or('\0'));
                        } else {
                            digits.push(second);
                            i += 1;
                        }
                    }
                }
                if !is_octal {
                    let index: usize = digits.parse().unwrap_or(usize::MAX);
                    if index > group_count {
                        return Err(format!(
                            "invalid group reference {index} at position {backslash_pos}"
                        ));
                    }
                    flush(&mut literal, &mut parts);
                    parts.push(Part::Group(index));
                }
            }
            'n' => {
                literal.push('\n');
                i += 1;
            }
            't' => {
                literal.push('\t');
                i += 1;
            }
            'r' => {
                literal.push('\r');
                i += 1;
            }
            'f' => {
                literal.push('\u{0c}');
                i += 1;
            }
            'v' => {
                literal.push('\u{0b}');
                i += 1;
            }
            'a' => {
                literal.push('\u{07}');
                i += 1;
            }
            'b' => {
                // The template's own `\b` is a literal backspace
                // (`ESCAPES['\\b']`), unlike the *pattern*'s `\b`
                // (a word-boundary assertion) -- confirmed directly
                // against CPython 3.11 (module doc comment).
                literal.push('\u{08}');
                i += 1;
            }
            '\\' => {
                literal.push('\\');
                i += 1;
            }
            other if other.is_ascii_alphabetic() => {
                return Err(format!("bad escape \\{other} at position {backslash_pos}"));
            }
            other => {
                // An unrecognized non-letter escape keeps *both* the
                // backslash and the character (confirmed directly:
                // `re.sub('a', r'\@', 'a')` gives `'\\@'`, not `'@'`
                // -- a template-only quirk; the *pattern* translator's
                // own `decode_escape_char` drops the backslash here).
                literal.push('\\');
                literal.push(other);
                i += 1;
            }
        }
    }
    flush(&mut literal, &mut parts);
    Ok(Template { parts })
}

/// `re.sub(pattern, repl, string, count=count)`, with Python's own
/// 3.7+ empty-match rule: an empty match immediately after a non-empty
/// one is still reported (the module doc comment's own worked example,
/// `re.sub('x*', '-', 'abxd')` == `'-a-b--d-'`) -- derived here from
/// the one scan rule both `sub` and `finditer` share (search from
/// *pos*; always emit; advance by one character past an empty match to
/// avoid looping forever, by one past the match's end otherwise),
/// rather than `fancy-regex`'s own iterator (`find_iter`/`replace_all`,
/// which skip exactly this case).
///
/// *count* follows CPython's own `Pattern.sub` rule, already applied
/// at the call site through `electricity_value::pycompat::py_int`:
/// `0` means "replace every match"; a negative count means "replace
/// none" (pinned against CPython 3.11 directly, not assumed).
// `fancy_regex::Error`'s own size, not ours to shrink -- it's reached
// only on a backtrack-limit or stack-overflow guard trip during
// matching, never on a hot path worth boxing for.
#[allow(clippy::result_large_err)]
pub fn substitute(
    re: &fancy_regex::Regex,
    template: &Template,
    text: &str,
) -> Result<String, fancy_regex::Error> {
    substitute_n(re, template, text, usize::MAX)
}

#[allow(clippy::result_large_err)]
pub fn substitute_n(
    re: &fancy_regex::Regex,
    template: &Template,
    text: &str,
    max: usize,
) -> Result<String, fancy_regex::Error> {
    if max == 0 {
        return Ok(text.to_string());
    }
    let mut out = String::new();
    let mut pos = 0usize;
    let mut last_copy = 0usize;
    let mut n = 0usize;
    while pos <= text.len() && n < max {
        let Some(caps) = re.captures_from_pos(text, pos)? else {
            break;
        };
        let m = caps.get(0).expect("the whole match is always present");
        out.push_str(&text[last_copy..m.start()]);
        out.push_str(&template.expand(&caps));
        last_copy = m.end();
        n += 1;
        pos = if m.end() == m.start() {
            match text[m.end()..].chars().next() {
                Some(c) => m.end() + c.len_utf8(),
                None => m.end() + 1,
            }
        } else {
            m.end()
        };
    }
    out.push_str(&text[last_copy..]);
    Ok(out)
}

/// Every match in *text*, in order, following the same scan rule
/// [`substitute_n`] does (so `findall`'s own group-shape loop sees
/// exactly the matches Python's `finditer`/`findall` would) --
/// eagerly collected, since a `regex` tool call is always over one
/// in-memory string, never a stream.
#[allow(clippy::result_large_err)]
pub fn find_iter<'t>(
    re: &fancy_regex::Regex,
    text: &'t str,
) -> Result<Vec<fancy_regex::Captures<'t>>, fancy_regex::Error> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos <= text.len() {
        let Some(caps) = re.captures_from_pos(text, pos)? else {
            break;
        };
        let m = caps.get(0).expect("the whole match is always present");
        let (start, end) = (m.start(), m.end());
        out.push(caps);
        pos = if end == start {
            match text[end..].chars().next() {
                Some(c) => end + c.len_utf8(),
                None => end + 1,
            }
        } else {
            end
        };
    }
    Ok(out)
}

/// Resolves `count` CPython's own way: `0` means unlimited, a negative
/// value means none at all.
pub fn max_replacements(count: i64) -> usize {
    if count == 0 {
        usize::MAX
    } else if count < 0 {
        0
    } else {
        count as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regex::translate::{ResolvedFlags, translate};

    fn compile(pattern: &str) -> (fancy_regex::Regex, usize, HashMap<String, usize>) {
        let t = translate(pattern, ResolvedFlags::default()).unwrap();
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        (re, t.group_count, t.group_names)
    }

    #[test]
    fn empty_match_after_nonempty_match_is_reported() {
        let (re, gc, names) = compile("x*");
        let tpl = parse_template("-", gc, &names).unwrap();
        let result = substitute(&re, &tpl, "abxd").unwrap();
        assert_eq!(result, "-a-b--d-");
    }

    #[test]
    fn numbered_backreference_in_template() {
        let (re, gc, names) = compile(r"(\w+)");
        let tpl = parse_template(r"[\1]", gc, &names).unwrap();
        let result = substitute(&re, &tpl, "hi").unwrap();
        assert_eq!(result, "[hi]");
    }

    #[test]
    fn named_group_reference_g_angle() {
        let (re, gc, names) = compile(r"(?P<word>\w+)");
        let tpl = parse_template(r"\g<word>!", gc, &names).unwrap();
        let result = substitute(&re, &tpl, "hi").unwrap();
        assert_eq!(result, "hi!");
    }

    #[test]
    fn g0_is_the_whole_match() {
        let (re, gc, names) = compile(r"\w+");
        let tpl = parse_template(r"[\g<0>]", gc, &names).unwrap();
        let result = substitute(&re, &tpl, "hi").unwrap();
        assert_eq!(result, "[hi]");
    }

    #[test]
    fn non_participating_group_expands_to_empty() {
        let (re, gc, names) = compile(r"(a)|(b)");
        let tpl = parse_template(r"[\1\2]", gc, &names).unwrap();
        let result = substitute(&re, &tpl, "b").unwrap();
        assert_eq!(result, "[b]"); // group 1 empty, group 2 == "b"
    }

    #[test]
    fn bad_escape_has_no_invalid_pattern_prefix() {
        let (_re, gc, names) = compile("a");
        let err = parse_template(r"\q", gc, &names).unwrap_err();
        assert!(err.contains("bad escape"));
        assert!(!err.contains("invalid pattern"));
    }

    #[test]
    fn invalid_group_reference_in_template() {
        let (_re, gc, names) = compile("(a)");
        let err = parse_template(r"\5", gc, &names).unwrap_err();
        assert!(err.contains("invalid group reference 5"));
    }

    #[test]
    fn unknown_group_name_in_template() {
        let (_re, gc, names) = compile("(a)");
        let err = parse_template(r"\g<name>", gc, &names).unwrap_err();
        assert!(err.contains("unknown group name"));
    }

    #[test]
    fn unrecognized_non_letter_escape_keeps_the_backslash() {
        let (re, gc, names) = compile("a");
        let tpl = parse_template(r"\@", gc, &names).unwrap();
        let result = substitute(&re, &tpl, "a").unwrap();
        assert_eq!(result, "\\@");
    }

    #[test]
    fn template_backslash_b_is_a_literal_backspace() {
        let (re, gc, names) = compile("a");
        let tpl = parse_template(r"\b", gc, &names).unwrap();
        let result = substitute(&re, &tpl, "a").unwrap();
        assert_eq!(result, "\u{08}");
    }

    #[test]
    fn negative_count_means_zero_replacements() {
        assert_eq!(max_replacements(-1), 0);
        assert_eq!(max_replacements(0), usize::MAX);
        assert_eq!(max_replacements(2), 2);
    }
}
