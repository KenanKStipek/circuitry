//! `core/prompt.py::PromptRuntime._decode_output`/`_parse_json`, ported.

use electricity_bytecode::effects::PromptType;
use electricity_value::Value;

use super::answers::{parse_boolean_answer, parse_number_answer};

/// `_decode_output`. `Ok(Value::None)` for the empty-text cases (never
/// `boolean`/`number`, which raise instead -- `parse_boolean_answer`/
/// `parse_number_answer` already reject an empty string).
pub(crate) fn decode_output(prompt_type: PromptType, text: &str) -> Result<Value, String> {
    if !matches!(prompt_type, PromptType::Boolean | PromptType::Number) && text.is_empty() {
        return Ok(Value::None);
    }
    let text = electricity_value::pycompat::py_strip(text);
    match prompt_type {
        PromptType::Text | PromptType::Tool => Ok(Value::Str(text.to_string())),
        PromptType::Boolean => parse_boolean_answer(text).map(Value::Bool),
        PromptType::Number => parse_number_answer(text),
        PromptType::Json | PromptType::Object | PromptType::Array => {
            Ok(parse_json(text).unwrap_or(Value::None))
        }
    }
}

/// `_parse_json`: a direct parse, then the first match of a fenced
/// ```` ```json ``` ```` or ```` ``` ``` ```` block, then the widest
/// `{...}`/`[...]` slice, else `None`.
pub(crate) fn parse_json(text: &str) -> Option<Value> {
    if let Ok(value) = electricity_json::loads(text) {
        return Some(value);
    }
    for fenced in ["```json", "```"] {
        if let Some(body) = extract_fenced(text, fenced) {
            if let Ok(value) = electricity_json::loads(&body) {
                return Some(value);
            }
        }
    }
    for (open, close) in [('{', '}'), ('[', ']')] {
        if let Some(value) = extract_bracketed(text, open, close) {
            return Some(value);
        }
    }
    None
}

/// The first match of ```` <fence>\s*([\s\S]*?)\s*``` ```` (Python
/// `\s`, [`electricity_value::pycompat::is_py_whitespace`]), *fence*
/// being `` ```json `` or `` ``` ``. A non-greedy body, so the match
/// stops at the *first* closing ```` ``` ```` after it.
fn extract_fenced(text: &str, fence: &str) -> Option<String> {
    let mut search_from = 0usize;
    while let Some(rel_start) = text[search_from..].find(fence) {
        let fence_start = search_from + rel_start;
        let mut pos = fence_start + fence.len();
        let bytes = text.as_bytes();
        while pos < bytes.len()
            && electricity_value::pycompat::is_py_whitespace(text[pos..].chars().next().unwrap())
        {
            pos += text[pos..].chars().next().unwrap().len_utf8();
        }
        let body_start = pos;
        if let Some(rel_close) = text[body_start..].find("```") {
            let close_start = body_start + rel_close;
            let mut body_end = close_start;
            while body_end > body_start {
                let prev_char = text[..body_end].chars().next_back().unwrap();
                if electricity_value::pycompat::is_py_whitespace(prev_char) {
                    body_end -= prev_char.len_utf8();
                } else {
                    break;
                }
            }
            return Some(text[body_start..body_end].to_string());
        }
        search_from = fence_start + fence.len();
    }
    None
}

/// `text[find(open):rfind(close)+1]`, parsed as JSON -- `None` when
/// either delimiter is absent, out of order, or the slice fails to
/// parse.
fn extract_bracketed(text: &str, open: char, close: char) -> Option<Value> {
    let start = text.find(open)?;
    let end = text.rfind(close)?;
    if end <= start {
        return None;
    }
    let slice = &text[start..end + close.len_utf8()];
    electricity_json::loads(slice).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_gives_none_for_text_and_json() {
        assert_eq!(decode_output(PromptType::Text, ""), Ok(Value::None));
        assert_eq!(decode_output(PromptType::Json, ""), Ok(Value::None));
    }

    #[test]
    fn boolean_and_number_reject_an_empty_reply() {
        assert!(decode_output(PromptType::Boolean, "").is_err());
        assert!(decode_output(PromptType::Number, "").is_err());
    }

    #[test]
    fn text_and_tool_strip_and_pass_through() {
        assert_eq!(
            decode_output(PromptType::Text, "  hi  "),
            Ok(Value::Str("hi".to_string()))
        );
        assert_eq!(
            decode_output(PromptType::Tool, "  hi  "),
            Ok(Value::Str("hi".to_string()))
        );
    }

    #[test]
    fn direct_json_parses() {
        assert!(parse_json(r#"{"a": 1}"#).is_some());
        assert!(parse_json("[1, 2, 3]").is_some());
    }

    #[test]
    fn json_fence_parses() {
        let text = "here:\n```json\n{\"a\": 1}\n```\nthanks";
        let value = parse_json(text).unwrap();
        assert_eq!(value.as_dict().unwrap().len(), 1);
    }

    #[test]
    fn bare_fence_parses() {
        let text = "```\n[1, 2]\n```";
        assert!(parse_json(text).is_some());
    }

    #[test]
    fn bracket_slice_fallback_parses() {
        let text = "sure, here you go: {\"a\": 1} -- hope that helps";
        let value = parse_json(text).unwrap();
        assert_eq!(value.as_dict().unwrap().len(), 1);
    }

    #[test]
    fn nothing_parseable_gives_none() {
        assert_eq!(parse_json("not json at all"), None);
    }
}
