//! `runtime.state.prompt_sent` (`core/prompt_record.py`, #577): how much of
//! a prompt's text a `prompt` effect keeps in `meta.prompt_sent`.
//!
//! - `full` (the default): the whole text.
//! - `sha256`: `null`.
//! - `head:<N>` (N >= 1): the first N characters (Unicode code points), then
//!   a marker saying how many were left out; a text of N characters or fewer
//!   is kept whole.

use electricity_value::Value;

/// The key's path, as the error text names it.
const SETTING_PATH: &str = "runtime.state.prompt_sent";

/// A parsed `runtime.state.prompt_sent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PromptSentSetting {
    #[default]
    Full,
    Sha256,
    /// `head:<N>`: keep the first N code points. N saturates at `usize::MAX`
    /// (a text that long is kept whole either way).
    Head(usize),
}

/// N of a `head:<N>`: ASCII digits only, at least 1; saturates at `usize::MAX`.
fn head_count(digits: &str) -> Option<usize> {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let trimmed = digits.trim_start_matches('0');
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.parse::<usize>().unwrap_or(usize::MAX))
}

/// `parse_prompt_sent_setting`: `None`/absent is `full`; anything that is not
/// one of the three forms (`head:0` included) is the error text.
fn parse_value(value: Option<&Value>) -> Result<PromptSentSetting, String> {
    let invalid = |value: &Value| {
        format!(
            "{SETTING_PATH} must be 'full', 'sha256' or 'head:<N>' (N a positive integer), got {}.",
            value.py_repr()
        )
    };
    match value {
        None | Some(Value::None) => Ok(PromptSentSetting::Full),
        Some(value) => {
            if let Value::Str(text) = value {
                if text == "full" {
                    return Ok(PromptSentSetting::Full);
                }
                if text == "sha256" {
                    return Ok(PromptSentSetting::Sha256);
                }
                if let Some(count) = text.strip_prefix("head:").and_then(head_count) {
                    return Ok(PromptSentSetting::Head(count));
                }
            }
            Err(invalid(value))
        }
    }
}

/// `runtime.state.prompt_sent` in *runtime*, or an error. A `state` that is
/// not a mapping is ignored, like Python's.
pub fn prompt_sent_setting(runtime: Option<&Value>) -> Result<PromptSentSetting, String> {
    let state = runtime
        .and_then(Value::as_dict)
        .and_then(|d| d.get(&Value::Str("state".to_string())))
        .and_then(Value::as_dict);
    match state {
        Some(state) => parse_value(state.get(&Value::Str("prompt_sent".to_string()))),
        None => Ok(PromptSentSetting::Full),
    }
}

/// `prompt_sent_errors`: the configuration errors, zero or one.
pub fn prompt_sent_errors(runtime: Option<&Value>) -> Vec<String> {
    prompt_sent_setting(runtime).err().into_iter().collect()
}

impl PromptSentSetting {
    /// What this setting leaves of *text* in a state record: all of it, a
    /// head with its marker, or `None`.
    pub fn record_text(self, text: &str) -> Option<String> {
        match self {
            PromptSentSetting::Full => Some(text.to_string()),
            PromptSentSetting::Sha256 => None,
            PromptSentSetting::Head(keep) => {
                let total = text.chars().count();
                if total <= keep {
                    return Some(text.to_string());
                }
                let head: String = text.chars().take(keep).collect();
                Some(format!(
                    "{head}\n[… {} more characters not recorded]",
                    total - keep
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    fn runtime_with(value: Value) -> Value {
        let mut state = Dict::new();
        state.insert(Value::Str("prompt_sent".to_string()), value);
        let mut runtime = Dict::new();
        runtime.insert(Value::Str("state".to_string()), Value::Dict(state));
        Value::Dict(runtime)
    }

    fn parse(text: &str) -> Result<PromptSentSetting, String> {
        prompt_sent_setting(Some(&runtime_with(Value::Str(text.to_string()))))
    }

    #[test]
    fn absent_is_full() {
        assert_eq!(prompt_sent_setting(None), Ok(PromptSentSetting::Full));
        assert_eq!(
            prompt_sent_setting(Some(&Value::Dict(Dict::new()))),
            Ok(PromptSentSetting::Full)
        );
    }

    #[test]
    fn the_three_forms_parse() {
        assert_eq!(parse("full"), Ok(PromptSentSetting::Full));
        assert_eq!(parse("sha256"), Ok(PromptSentSetting::Sha256));
        assert_eq!(parse("head:1"), Ok(PromptSentSetting::Head(1)));
        assert_eq!(parse("head:007"), Ok(PromptSentSetting::Head(7)));
        assert_eq!(
            parse("head:99999999999999999999999"),
            Ok(PromptSentSetting::Head(usize::MAX))
        );
    }

    #[test]
    fn anything_else_is_an_error_naming_the_key() {
        for bad in [
            "head:0", "head:-3", "head:", "head:x", "head:1.5", "head:+2", "HEAD:3", "none", "",
        ] {
            let err = parse(bad).unwrap_err();
            assert!(
                err.starts_with("runtime.state.prompt_sent must be 'full', 'sha256' or 'head:<N>'"),
                "{bad}: {err}"
            );
        }
        assert_eq!(
            prompt_sent_errors(Some(&runtime_with(Value::Bool(true)))),
            vec![
                "runtime.state.prompt_sent must be 'full', 'sha256' or 'head:<N>' (N a positive integer), got True."
                    .to_string()
            ]
        );
    }

    #[test]
    fn head_counts_code_points_and_keeps_a_short_text_whole() {
        let head = PromptSentSetting::Head(3);
        assert_eq!(
            head.record_text("a🙂bcdef").as_deref(),
            Some("a🙂b\n[… 4 more characters not recorded]")
        );
        assert_eq!(head.record_text("abc").as_deref(), Some("abc"));
        assert_eq!(PromptSentSetting::Sha256.record_text("abc"), None);
    }
}
