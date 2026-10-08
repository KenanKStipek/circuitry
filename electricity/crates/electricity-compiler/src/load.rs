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

    match suffix.as_str() {
        ".yml" | ".yaml" => {
            let text = read_universal_newlines(path)?;
            let value = electricity_yaml::load_yaml(&text).map_err(|err| {
                CompileError(match err {
                    electricity_yaml::YamlError::DuplicateKey { message } => {
                        format!("{}: {message}", pathlib_str(path))
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
            let text = read_universal_newlines(path)?;
            let value = electricity_json::load_json(&text).map_err(|err| {
                CompileError(match err {
                    electricity_json::ReadError::DuplicateKey { message } => {
                        format!("{}: {message}", pathlib_str(path))
                    }
                    other => other.to_string(),
                })
            })?;
            require_mapping_root(value, "JSON")
        }
        // `.toon` is refused outright (a documented divergence, issue
        // #408's Scope section) without reading the file at all --
        // unlike Python's own `load_orchestration_file`, which reads it
        // unconditionally before even checking `toon_format` is
        // installed. Not read here either way, since the fallback
        // message never depends on the file's contents.
        ".toon" => Err(CompileError(
            "TOON documents are not supported; convert to YAML or JSON".to_string(),
        )),
        // The suffix is checked *before* any read, matching
        // `orchestration_loader.py::load_orchestration_file`'s own
        // order -- an unsupported suffix never reaches an OS error (a
        // missing file) or a UTF-8 decode error (non-text bytes) first.
        _ => {
            let supported = SUPPORTED_SUFFIXES.join(", ");
            Err(CompileError(format!(
                "Unsupported orchestration format: {suffix}. Supported: {supported}"
            )))
        }
    }
}

/// `str(Path(given))`: drops a `.` component, collapses repeated `/`,
/// and strips a trailing `/` -- pathlib's own normalization, which
/// Rust's `Path::components()` mostly already applies (a trailing `/`
/// and repeated `//` never produce separate components), except a
/// leading/mid-path `.` component, which `components()` keeps and this
/// strips to match (`./doc.yml` -> `doc.yml`, `a/./b.yml` -> `a/b.yml`).
/// Unlike pathlib, never resolves `..` -- pathlib doesn't either.
fn pathlib_str(path: &Path) -> String {
    use std::path::Component;
    let mut is_absolute = false;
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir => is_absolute = true,
            Component::CurDir => {}
            Component::ParentDir => parts.push("..".to_string()),
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::Prefix(prefix) => {
                parts.push(prefix.as_os_str().to_string_lossy().into_owned())
            }
        }
    }
    if parts.is_empty() {
        return ".".to_string();
    }
    let joined = parts.join("/");
    if is_absolute {
        format!("/{joined}")
    } else {
        joined
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
    use super::{pathlib_str, translate_universal_newlines};
    use std::path::Path;

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

    #[test]
    fn pathlib_str_drops_a_leading_dot_component() {
        assert_eq!(pathlib_str(Path::new("./doc.yml")), "doc.yml");
    }

    #[test]
    fn pathlib_str_collapses_repeated_slashes() {
        assert_eq!(pathlib_str(Path::new("a//b.yml")), "a/b.yml");
    }

    #[test]
    fn pathlib_str_drops_a_mid_path_dot_component() {
        assert_eq!(pathlib_str(Path::new("./a/./b.yml")), "a/b.yml");
    }

    #[test]
    fn pathlib_str_strips_a_trailing_slash() {
        assert_eq!(pathlib_str(Path::new("a/b/")), "a/b");
    }

    #[test]
    fn pathlib_str_does_not_resolve_parent_components() {
        assert_eq!(pathlib_str(Path::new("a/../b.yml")), "a/../b.yml");
    }

    #[test]
    fn pathlib_str_keeps_a_leading_slash_absolute() {
        assert_eq!(pathlib_str(Path::new("/a//b.yml")), "/a/b.yml");
    }

    #[test]
    fn pathlib_str_plain_relative_path_is_unchanged() {
        assert_eq!(pathlib_str(Path::new("doc.yml")), "doc.yml");
    }
}
