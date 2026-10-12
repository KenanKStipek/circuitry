//! `core/answers.py`, ported -- lenient boolean/number parsing for a
//! model's free-text reply. `pub(crate)` (not private to `prompt.rs`
//! alone) so G2's own model-mode `if`/`while`/`expect` can reuse it
//! directly rather than growing a second copy.

const QUOTE_CHARS: &[char] = &['"', '\'', '`'];
const EMPHASIS_MARKERS: &[&str] = &["**", "__", "*", "_", "`"];
const TRAILING_PUNCT: &[char] = &['.', ',', '!', '?', ';', ':'];

const TRUE_WORDS: &[&str] = &["true", "yes", "y", "1"];
const FALSE_WORDS: &[&str] = &["false", "no", "n", "0"];

/// `_strip_wrapping`: strip whitespace, then any surrounding quote or
/// markdown-emphasis pair, repeating so `'**"yes"**'` unwraps fully.
fn strip_wrapping(text: &str) -> String {
    let mut current = electricity_value::pycompat::py_strip(text).to_string();
    loop {
        let trimmed = electricity_value::pycompat::py_strip(&current).to_string();
        let chars: Vec<char> = trimmed.chars().collect();
        let mut next: Option<String> = None;
        if chars.len() >= 2 && chars[0] == chars[chars.len() - 1] && QUOTE_CHARS.contains(&chars[0])
        {
            next = Some(chars[1..chars.len() - 1].iter().collect());
        } else {
            for marker in EMPHASIS_MARKERS {
                let width = marker.chars().count();
                if chars.len() >= 2 * width
                    && trimmed.starts_with(marker)
                    && trimmed.ends_with(marker)
                {
                    let start: usize = chars[..width].iter().map(|c| c.len_utf8()).sum();
                    let end_from: usize = chars[chars.len() - width..]
                        .iter()
                        .map(|c| c.len_utf8())
                        .sum();
                    next = Some(trimmed[start..trimmed.len() - end_from].to_string());
                    break;
                }
            }
        }
        match next {
            Some(next) => current = next,
            None => return trimmed,
        }
    }
}

/// `_split_leading_token`: `(leading token, remainder)` after stripping
/// wrapping. The token has its own wrapping quotes/emphasis and
/// trailing punctuation removed; the remainder is whatever
/// whitespace-separated content follows it, unprocessed.
fn split_leading_token(text: &str) -> (String, String) {
    let stripped = strip_wrapping(text);
    if stripped.is_empty() {
        return (String::new(), String::new());
    }
    let mut parts = stripped.splitn(2, electricity_value::pycompat::is_py_whitespace);
    let first = parts.next().unwrap_or("");
    // `str.split(None, 1)` treats a run of whitespace as one
    // separator and drops leading/trailing empties -- `splitn` on a
    // predicate instead splits at the *first* whitespace char only,
    // so a leading run of several whitespace chars before the next
    // token would otherwise leave `remainder` with stray leading
    // whitespace of its own; strip it here to match.
    let remainder = parts
        .next()
        .map(electricity_value::pycompat::py_strip)
        .unwrap_or("");
    let token = first.trim_end_matches(TRAILING_PUNCT);
    let token = strip_wrapping(token);
    (token, remainder.to_string())
}

/// `parse_boolean_answer`. `Err` carries the message text
/// `AnswerParseError` would -- the raw reply is the caller's own
/// (already has `text`).
pub(crate) fn parse_boolean_answer(text: &str) -> Result<bool, String> {
    let (token, _remainder) = split_leading_token(text);
    let lowered = token.to_lowercase();
    if TRUE_WORDS.contains(&lowered.as_str()) {
        return Ok(true);
    }
    if FALSE_WORDS.contains(&lowered.as_str()) {
        return Ok(false);
    }
    Err(format!(
        "could not parse a yes/no answer from {}",
        electricity_value::Value::Str(text.to_string()).py_repr()
    ))
}

/// `parse_number_answer`. Returns a [`electricity_value::Value::Int`]
/// when the token parses as a bare integer (`int(token)` succeeds
/// first, exactly as Python tries `int` before `float`), else a
/// [`electricity_value::Value::Float`].
pub(crate) fn parse_number_answer(text: &str) -> Result<electricity_value::Value, String> {
    let (token, remainder) = split_leading_token(text);
    let err = || {
        format!(
            "could not parse a number from {}",
            electricity_value::Value::Str(text.to_string()).py_repr()
        )
    };
    if token.is_empty() || !remainder.is_empty() || token.contains('_') {
        return Err(err());
    }
    if let Some(int_value) = electricity_value::IntValue::parse_decimal(&token) {
        return Ok(electricity_value::Value::Int(int_value));
    }
    match token.parse::<f64>() {
        Ok(value) if value.is_finite() => Ok(electricity_value::Value::Float(value)),
        _ => Err(err()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::{IntValue, Value};

    #[test]
    fn plain_words_parse() {
        assert_eq!(parse_boolean_answer("yes"), Ok(true));
        assert_eq!(parse_boolean_answer("no"), Ok(false));
        assert_eq!(parse_boolean_answer("Y"), Ok(true));
        assert_eq!(parse_boolean_answer("N"), Ok(false));
        assert_eq!(parse_boolean_answer("1"), Ok(true));
        assert_eq!(parse_boolean_answer("0"), Ok(false));
    }

    #[test]
    fn trailing_punctuation_and_case_are_stripped() {
        assert_eq!(parse_boolean_answer("Yes."), Ok(true));
        assert_eq!(parse_boolean_answer("false!"), Ok(false));
        assert_eq!(parse_boolean_answer("yes, because ..."), Ok(true));
    }

    #[test]
    fn markdown_emphasis_and_quotes_unwrap() {
        assert_eq!(parse_boolean_answer("**TRUE**"), Ok(true));
        assert_eq!(
            parse_boolean_answer("'**\"yes\"**'".trim_matches('\'')),
            Ok(true)
        );
        assert_eq!(parse_boolean_answer("**\"yes\"**"), Ok(true));
    }

    #[test]
    fn an_unreadable_answer_raises() {
        let err = parse_boolean_answer("maybe").unwrap_err();
        assert_eq!(err, "could not parse a yes/no answer from 'maybe'");
    }

    #[test]
    fn an_empty_reply_raises() {
        assert!(parse_boolean_answer("").is_err());
    }

    #[test]
    fn integers_parse_as_int() {
        assert_eq!(
            parse_number_answer("42").unwrap(),
            Value::Int(IntValue::Small(42))
        );
        assert_eq!(parse_number_answer("42.").unwrap(), Value::Float(42.0));
        assert_eq!(
            parse_number_answer("-1").unwrap(),
            Value::Int(IntValue::Small(-1))
        );
    }

    #[test]
    fn floats_and_exponents_parse() {
        assert_eq!(parse_number_answer("3.5").unwrap(), Value::Float(3.5));
        assert_eq!(parse_number_answer("1e3").unwrap(), Value::Float(1000.0));
    }

    #[test]
    fn trailing_words_are_rejected() {
        assert!(parse_number_answer("about 42").is_err());
        assert!(parse_number_answer("42 degrees").is_err());
    }

    #[test]
    fn underscore_digit_grouping_is_rejected() {
        assert!(parse_number_answer("1_000").is_err());
    }

    #[test]
    fn non_finite_is_rejected() {
        assert!(parse_number_answer("inf").is_err());
        assert!(parse_number_answer("nan").is_err());
    }

    #[test]
    fn empty_reply_is_rejected() {
        assert!(parse_number_answer("").is_err());
    }
}
