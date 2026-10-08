//! Lane B: `load_document`, porting `cli/orchestration_loader.py`'s
//! `load_orchestration_file`.
//!
//! The suffix picks the format (`.yml`/`.yaml` -> `electricity-yaml`,
//! `.json` -> `electricity-json`, `.toon` refused); text is read with
//! universal-newline translation, and a duplicate-key message is
//! prefixed with the path as given.

use crate::CompileError;
use electricity_value::Value;
use std::path::Path;

/// `cli/orchestration_loader.ORCHESTRATION_SUFFIXES`, sorted the same
/// way `", ".join(sorted(ORCHESTRATION_SUFFIXES))` lists them in the
/// unsupported-format message.
const SUPPORTED_SUFFIXES: [&str; 4] = [".json", ".toon", ".yaml", ".yml"];

/// Loads *path* the way `cli/orchestration_loader.py::load_orchestration_file`
/// does: picks a parser by suffix, reads the file with Python's
/// universal-newline translation, and checks the result is a mapping at
/// the root.
///
/// Returns [`CompileError`] (not a dedicated error type) since every
/// caller -- `pipeline::check_report`/`pipeline::check_for_run` -- folds
/// a load failure into the same "one plain message, no further
/// structure" shape a `compile_document` error already has (`str(e)` in
/// Circuitry's own `validate`/`run`, which catch a load failure no
/// differently from a compile failure).
pub fn load_document(path: &Path) -> Result<Value, CompileError> {
    let suffix = suffix_lowercase(path);
    let text = read_universal_newlines(path)?;

    match suffix.as_str() {
        ".yml" | ".yaml" => {
            let value = electricity_yaml::load_yaml(&text).map_err(|err| {
                CompileError(match err {
                    electricity_yaml::YamlError::DuplicateKey { message } => {
                        format!("{}: {message}", path.display())
                    }
                    other => other.to_string(),
                })
            })?;
            let value = if is_truthy(&value) {
                value
            } else {
                Value::Dict(electricity_value::Dict::new())
            };
            require_mapping_root(value, "YAML")
        }
        ".json" => {
            let value = electricity_json::load_json(&text).map_err(|err| {
                CompileError(match err {
                    electricity_json::ReadError::DuplicateKey { message } => {
                        format!("{}: {message}", path.display())
                    }
                    other => other.to_string(),
                })
            })?;
            require_mapping_root(value, "JSON")
        }
        ".toon" => Err(CompileError(
            "TOON documents are not supported; convert to YAML or JSON".to_string(),
        )),
        _ => {
            let supported = SUPPORTED_SUFFIXES.join(", ");
            Err(CompileError(format!(
                "Unsupported orchestration format: {suffix}. Supported: {supported}"
            )))
        }
    }
}

fn suffix_lowercase(path: &Path) -> String {
    path.extension()
        .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()))
        .unwrap_or_default()
}

fn require_mapping_root(value: Value, label: &str) -> Result<Value, CompileError> {
    match value {
        Value::Dict(_) => Ok(value),
        _ => Err(CompileError(format!(
            "Orchestration {label} must be a mapping/object at the root."
        ))),
    }
}

/// Python truthiness (`load_yaml(raw) or {}`): `None`, `False`, a
/// numeric zero, and any empty `str`/`bytes`/`list`/`dict` are falsy;
/// everything else -- including a non-empty but non-mapping root, which
/// [`require_mapping_root`] still rejects afterward -- is truthy.
fn is_truthy(value: &Value) -> bool {
    match value {
        Value::None => false,
        Value::Bool(b) => *b,
        Value::Int(i) => !i.is_zero(),
        Value::Float(f) => *f != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::Bytes(b) => !b.is_empty(),
        Value::List(items) => !items.is_empty(),
        Value::Dict(d) => !d.is_empty(),
        Value::Date(_) | Value::DateTime(..) => true,
    }
}

/// Reads *path* the way Python's `Path.read_text(encoding="utf-8")`
/// does: UTF-8 decoded, then universal-newline translated (`\r\n` and a
/// lone `\r` both become `\n`) -- text-mode semantics, not the raw
/// bytes a digest needs (`digest::document_content_digest` reads the
/// file separately, by its own raw bytes).
pub(crate) fn read_universal_newlines(path: &Path) -> Result<String, CompileError> {
    let bytes =
        std::fs::read(path).map_err(|err| CompileError(format!("{}: {err}", path.display())))?;
    let text = String::from_utf8(bytes)
        .map_err(|err| CompileError(format!("{}: invalid UTF-8 ({err})", path.display())))?;
    Ok(translate_universal_newlines(&text))
}

/// `\r\n` and a lone `\r` both become `\n`; a lone `\n` is left alone.
fn translate_universal_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::translate_universal_newlines;

    #[test]
    fn crlf_becomes_lf() {
        assert_eq!(translate_universal_newlines("a\r\nb\r\n"), "a\nb\n");
    }

    #[test]
    fn lone_cr_becomes_lf() {
        assert_eq!(translate_universal_newlines("a\rb\r"), "a\nb\n");
    }

    #[test]
    fn mixed_line_endings_all_normalize() {
        assert_eq!(translate_universal_newlines("a\r\nb\rc\nd"), "a\nb\nc\nd");
    }

    #[test]
    fn plain_lf_is_unchanged() {
        assert_eq!(translate_universal_newlines("a\nb\n"), "a\nb\n");
    }

    #[test]
    fn no_newlines_is_unchanged() {
        assert_eq!(translate_universal_newlines("abc"), "abc");
    }
}
