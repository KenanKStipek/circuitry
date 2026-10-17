//! `runtime.state.parsed_stdout` (`core/parsed_stdout.py`, #592): whether a
//! `parse: json` step keeps its raw stdout in `meta.stdout`.
//!
//! - `full` (the default): the raw text.
//! - `drop`: a step whose stdout parsed records `meta.stdout: null`, plus
//!   `meta.stdout_chars` and `meta.stdout_sha256`. A failed parse already
//!   records no stdout, so failures are unchanged.

use electricity_value::Value;

/// The key's path, as the error text names it.
const SETTING_PATH: &str = "runtime.state.parsed_stdout";

/// `_parse`: `None`/absent and `full` keep the text, `drop` drops it, anything
/// else is the error text.
fn parse_value(value: Option<&Value>) -> Result<bool, String> {
    match value {
        None | Some(Value::None) => Ok(false),
        Some(Value::Str(text)) if text == "full" => Ok(false),
        Some(Value::Str(text)) if text == "drop" => Ok(true),
        Some(value) => Err(format!(
            "{SETTING_PATH} must be 'full' or 'drop', got {}.",
            value.py_repr()
        )),
    }
}

/// `drops_parsed_stdout`: `true` for `drop`, or an error. A `state` that is
/// not a mapping is ignored, like Python's.
pub fn drops_parsed_stdout(runtime: Option<&Value>) -> Result<bool, String> {
    let state = runtime
        .and_then(Value::as_dict)
        .and_then(|d| d.get(&Value::Str("state".to_string())))
        .and_then(Value::as_dict);
    match state {
        Some(state) => parse_value(state.get(&Value::Str("parsed_stdout".to_string()))),
        None => Ok(false),
    }
}

/// `parsed_stdout_errors`: the configuration errors, zero or one.
pub fn parsed_stdout_errors(runtime: Option<&Value>) -> Vec<String> {
    drops_parsed_stdout(runtime).err().into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    fn runtime_with(value: Value) -> Value {
        let mut state = Dict::new();
        state.insert(Value::Str("parsed_stdout".to_string()), value);
        let mut runtime = Dict::new();
        runtime.insert(Value::Str("state".to_string()), Value::Dict(state));
        Value::Dict(runtime)
    }

    #[test]
    fn absent_and_full_keep_the_text_and_drop_drops_it() {
        assert_eq!(drops_parsed_stdout(None), Ok(false));
        assert_eq!(
            drops_parsed_stdout(Some(&Value::Dict(Dict::new()))),
            Ok(false)
        );
        assert_eq!(
            drops_parsed_stdout(Some(&runtime_with(Value::None))),
            Ok(false)
        );
        assert_eq!(
            drops_parsed_stdout(Some(&runtime_with(Value::from("full")))),
            Ok(false)
        );
        assert_eq!(
            drops_parsed_stdout(Some(&runtime_with(Value::from("drop")))),
            Ok(true)
        );
    }

    #[test]
    fn any_other_value_is_an_error_naming_the_key() {
        assert_eq!(
            parsed_stdout_errors(Some(&runtime_with(Value::from("sha256")))),
            vec!["runtime.state.parsed_stdout must be 'full' or 'drop', got 'sha256'.".to_string()]
        );
        assert_eq!(
            parsed_stdout_errors(Some(&runtime_with(Value::from(5_i64)))),
            vec!["runtime.state.parsed_stdout must be 'full' or 'drop', got 5.".to_string()]
        );
        assert_eq!(
            parsed_stdout_errors(Some(&runtime_with(Value::Bool(true)))),
            vec!["runtime.state.parsed_stdout must be 'full' or 'drop', got True.".to_string()]
        );
    }

    #[test]
    fn a_state_that_is_not_a_mapping_is_ignored() {
        let mut runtime = Dict::new();
        runtime.insert(Value::Str("state".to_string()), Value::from("drop"));
        assert_eq!(drops_parsed_stdout(Some(&Value::Dict(runtime))), Ok(false));
    }
}
