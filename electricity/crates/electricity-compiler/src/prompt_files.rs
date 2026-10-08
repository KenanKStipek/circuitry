//! Lane D: ports `core/prompt_files.py` — reading a `{file: ...}`
//! prompt source at compile time: literal/relative paths with no
//! `{{ }}`, confinement to the nearest `circuitry.config.json`/
//! `config.json` (checked after following symlinks, non-strict
//! resolve, before the existence check), missing/not-a-regular-file/
//! over-1-MiB/not-UTF-8/unreadable errors, universal-newline
//! translation, and the refusal for [`crate::DocumentOrigin::Generated`].
//!
//! Called first from `compile::compile_document` (lane C), matching
//! `core/compiler.py::compile_orchestration`'s order: declared prompts
//! read before the composition checks ([`crate::compose`]) before any
//! effect compiles.
//!
//! Confinement is checked against `origin`'s own `confinement_root` —
//! this module never walks for the nearest `circuitry.config.json`/
//! `config.json` itself (lane B's own job, building
//! [`crate::DocumentOrigin`]).
//!
//! # Known divergences
//!
//! - `core/prompt_files.py::resolve_prompt_file_path` lets a
//!   confinement root's own `Path.resolve(strict=False)` raise an
//!   *uncaught* `RuntimeError` (only the candidate path's own resolve
//!   is wrapped in a `PromptFileError`) — effectively a latent bug,
//!   reachable only by a confinement root itself behind a symlink
//!   *loop* (on CPython 3.11, that is the only case `resolve(strict=
//!   False)` raises in at all; a merely dangling/broken chain with no
//!   cycle resolves fine, leaving the missing target as a literal
//!   trailing path segment — see [`resolve_non_strict`]'s own tests).
//!   This port wraps both the same way, as a `CompileError` rather than
//!   an uncaught panic, since Rust has no equivalent of letting an
//!   arbitrary exception type propagate out of a `Result`-returning
//!   function. Not pinned by a corpus case: the exact OS error text
//!   this produces is errno-message dependent even in CPython itself.
//! - [`resolve_non_strict`] caps a symlink chain at 40 hops (looping or
//!   not), failing with "too many levels of symbolic links" past that —
//!   an approximation of the OS's own `ELOOP`, not a byte-for-byte port
//!   of CPython's own structural loop detection (which raises
//!   `RuntimeError` as soon as it revisits a path, regardless of chain
//!   length) or its error text.
//! - [`resolve_prompt_file`]'s "could not be read" message reports the
//!   OS error text verbatim after that prefix — `std::io::Error`'s own
//!   `Display`, never a byte-for-byte match for Python's
//!   `OSError.__str__` (Rust's `Permission denied (os error 13)` vs.
//!   Python's `[Errno 13] Permission denied: '<path>'`, say). Only the
//!   Circuitry-owned prefix in front of it is pinned exactly by a
//!   corpus case (`tests/golden_compose.rs`'s own
//!   `prompts_file_unreadable_matches_circuitrys_own_prefix`); the
//!   OS-specific suffix is compared by location only (DESIGN.md §1,
//!   §12).

use crate::{CompileError, DocumentOrigin};
use electricity_value::Value;
use indexmap::IndexMap;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};

/// A prompt file larger than this is rejected at compile time —
/// `core/prompt_files.py::MAX_PROMPT_FILE_BYTES`.
pub(crate) const MAX_PROMPT_FILE_BYTES: u64 = 1024 * 1024;

/// The top-level `prompts:` map, read into raw text with any `{file:
/// ...}` source already resolved -- `Program.prompts`.
pub(crate) fn compile_declared_prompts(
    document: &Value,
    origin: &DocumentOrigin,
) -> Result<IndexMap<String, String>, CompileError> {
    let raw = match dict_get(document, "prompts") {
        None | Some(Value::None) => return Ok(IndexMap::new()),
        Some(value) => value,
    };
    let Value::Dict(raw_dict) = raw else {
        return Err(CompileError(
            "'prompts' must be a mapping of name to template.".to_string(),
        ));
    };

    let mut declared = IndexMap::new();
    for (key, value) in raw_dict {
        let name = match key {
            Value::Str(s) if is_declared_prompt_name_shape(s) => s.clone(),
            _ => {
                return Err(CompileError(format!(
                    "'prompts' key {} must be a valid name ([A-Za-z_][A-Za-z0-9_]*, no '.').",
                    key.py_repr()
                )));
            }
        };
        let field = format!("prompts.{name}");
        let text = resolve_text_or_file(value, &field, origin)?;
        declared.insert(name, text);
    }
    Ok(declared)
}

/// A declared-prompt key: `^[A-Za-z_][A-Za-z0-9_]*$` -- `core/prompt_
/// compose.py`'s `_NAME_SHAPE` further allows dotted segments for a
/// `{{> name}}` *reference*, but `compile_declared_prompts` additionally
/// rejects any `.` in a *key*, so the two conditions together reduce to
/// this plain, dot-free shape.
///
/// A single trailing `\n` is accepted on the key itself (not `\r\n`,
/// not two or more) -- Python's own `$` (without `re.MULTILINE`)
/// matches at the end of the string *or* immediately before one
/// trailing newline there, so `_NAME_SHAPE.match("greeting\n")` still
/// succeeds. A key with an embedded `\n` anywhere else still fails,
/// since the character class after it excludes `\n` either way.
fn is_declared_prompt_name_shape(name: &str) -> bool {
    let candidate = name.strip_suffix('\n').unwrap_or(name);
    let mut chars = candidate.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn dict_get<'a>(document: &'a Value, key: &str) -> Option<&'a Value> {
    document.as_dict()?.get(&Value::Str(key.to_string()))
}

/// A template/content field's text: *value* itself, or a `{file: ...}`'s
/// -- `core/prompt_files.py::resolve_text_or_file`.
///
/// Used both by [`compile_declared_prompts`] (every `prompts:` entry)
/// and -- once lane C wires it in -- by a `prompt`/`yield` effect's own
/// `template`/`messages[].content`, the only other fields `{file: ...}`
/// is allowed on.
pub(crate) fn resolve_text_or_file(
    value: &Value,
    field: &str,
    origin: &DocumentOrigin,
) -> Result<String, CompileError> {
    if let Value::Str(s) = value {
        return Ok(s.clone());
    }
    if let Some(file_value) = file_shaped(value) {
        let DocumentOrigin::File {
            document_dir,
            confinement_root,
        } = origin
        else {
            return Err(CompileError(format!(
                "{field}: 'file:' cannot be used here — this document has no \
                 file of its own (it was generated at run time, or has no \
                 path: stdin, an SDK string, a 'use: inline' child)."
            )));
        };
        return resolve_prompt_file(file_value, field, document_dir, confinement_root);
    }
    Err(CompileError(format!(
        "{field} must be a string or {{file: <path>}}."
    )))
}

/// The resolved, confined filesystem path a `{file: <path_str>}` names,
/// plus *path_str* itself (every caller needs it again for a later
/// error message) -- `core/prompt_files.py::resolve_prompt_file_path`.
pub(crate) fn resolve_prompt_file_path(
    file_value: &Value,
    field: &str,
    document_dir: &Path,
    confinement_root: &Path,
) -> Result<(String, PathBuf), CompileError> {
    let path_str = match file_value {
        Value::Str(s) if !s.trim().is_empty() => s.clone(),
        _ => {
            return Err(CompileError(format!(
                "{field}: 'file' must be a non-empty string path."
            )));
        }
    };
    if path_str.contains("{{") || path_str.contains("}}") {
        return Err(CompileError(format!(
            "{field}: 'file' must be a literal path — it cannot contain a \
             template tag: {}.",
            repr_str(&path_str)
        )));
    }
    let raw = Path::new(&path_str);
    if raw.is_absolute() {
        return Err(CompileError(format!(
            "{field}: 'file' must be a relative path, got absolute path {}.",
            repr_str(&path_str)
        )));
    }

    let candidate = document_dir.join(raw);
    let resolved = resolve_non_strict(&candidate).map_err(|err| {
        CompileError(format!(
            "{field}: could not resolve 'file' {}: {err}",
            repr_str(&path_str)
        ))
    })?;
    let confinement_resolved = resolve_non_strict(confinement_root).map_err(|err| {
        CompileError(format!(
            "{field}: could not resolve 'file' {}: {err}",
            repr_str(&path_str)
        ))
    })?;

    if resolved != confinement_resolved && !resolved.starts_with(&confinement_resolved) {
        return Err(CompileError(format!(
            "{field}: 'file' {} resolves outside the project ({}) — 'file:' \
             paths must stay inside it.",
            repr_str(&path_str),
            confinement_resolved.display()
        )));
    }
    Ok((path_str, resolved))
}

/// Reads and returns the text of a `{file: <path_str>}` prompt source --
/// `core/prompt_files.py::resolve_prompt_file`.
fn resolve_prompt_file(
    file_value: &Value,
    field: &str,
    document_dir: &Path,
    confinement_root: &Path,
) -> Result<String, CompileError> {
    let (path_str, resolved) =
        resolve_prompt_file_path(file_value, field, document_dir, confinement_root)?;

    if !resolved.exists() {
        return Err(CompileError(format!(
            "{field}: 'file' {} does not exist.",
            repr_str(&path_str)
        )));
    }
    if !resolved.is_file() {
        return Err(CompileError(format!(
            "{field}: 'file' {} is not a regular file.",
            repr_str(&path_str)
        )));
    }

    let metadata = std::fs::metadata(&resolved).map_err(|err| {
        CompileError(format!(
            "{field}: 'file' {} could not be read: {err}",
            repr_str(&path_str)
        ))
    })?;
    let size = metadata.len();
    if size > MAX_PROMPT_FILE_BYTES {
        return Err(CompileError(format!(
            "{field}: 'file' {} is {size} bytes, over the {MAX_PROMPT_FILE_BYTES}-byte \
             limit for a prompt file.",
            repr_str(&path_str)
        )));
    }

    let bytes = std::fs::read(&resolved).map_err(|err| {
        CompileError(format!(
            "{field}: 'file' {} could not be read: {err}",
            repr_str(&path_str)
        ))
    })?;
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text.to_string(),
        Err(_) => {
            return Err(CompileError(format!(
                "{field}: 'file' {} is not valid UTF-8: {}",
                repr_str(&path_str),
                describe_utf8_error(&bytes)
            )));
        }
    };
    Ok(universal_newlines(&text))
}

/// Python `repr()` of a plain string -- every `{path_str!r}` in
/// `core/prompt_files.py`'s own messages.
fn repr_str(s: &str) -> String {
    Value::Str(s.to_string()).py_repr()
}

/// *value* if it is shaped exactly `{file: ...}` (one key, named
/// `"file"`) -- the same shape test [`resolve_text_or_file`] and
/// `core/prompt_compose.py`'s own `resolve()`/`_resolve_file_field_text`
/// helpers use, factored out for [`crate::digest`]'s best-effort
/// prompt-file scan, which (like those two) does not care *why* a value
/// isn't `{file: ...}`-shaped, only whether it is.
pub(crate) fn file_shaped(value: &Value) -> Option<&Value> {
    let Value::Dict(dict) = value else {
        return None;
    };
    let file_key = Value::Str("file".to_string());
    if dict.len() == 1 {
        dict.get(&file_key)
    } else {
        None
    }
}

/// `\r\n` and a lone `\r` both become `\n` -- Python's universal-newline
/// text-mode translation (`Path.read_text`'s default), applied in two
/// passes so a run like `\r\r\n` (a lone-CR line ending immediately
/// followed by a CRLF one) still produces two line breaks, not one.
fn universal_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// `Path.resolve(strict=False)`: resolves `..`/`.` and every symlink
/// along *path* that actually exists, leaving any trailing nonexistent
/// component(s) as literal, normalized path segments rather than
/// failing -- unlike `fs::canonicalize`, which requires the whole path
/// to exist.
///
/// *path* is made absolute against the current working directory first
/// if it is not already (matching `Path.resolve`, which always returns
/// an absolute path regardless of its input). Symlink chains are capped
/// at 40 hops, erroring instead of looping forever on a cycle -- an
/// approximation of the OS's own `ELOOP`, not a byte-for-byte port of
/// CPython's error text for that case (see this module's "Known
/// divergence").
pub(crate) fn resolve_non_strict(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };

    let mut queue: VecDeque<OsString> = VecDeque::new();
    push_components(&absolute, &mut queue);

    let mut resolved = PathBuf::from("/");
    let mut link_count = 0usize;
    const MAX_LINKS: usize = 40;

    while let Some(part) = queue.pop_front() {
        if part == ".." {
            resolved.pop();
            continue;
        }
        let candidate = resolved.join(&part);
        match std::fs::symlink_metadata(&candidate) {
            Ok(meta) if meta.file_type().is_symlink() => {
                link_count += 1;
                if link_count > MAX_LINKS {
                    return Err(io::Error::other("too many levels of symbolic links"));
                }
                let target = std::fs::read_link(&candidate)?;
                if target.is_absolute() {
                    resolved = PathBuf::from("/");
                }
                let mut target_queue: VecDeque<OsString> = VecDeque::new();
                push_components(&target, &mut target_queue);
                target_queue.extend(queue.drain(..));
                queue = target_queue;
            }
            _ => {
                resolved = candidate;
            }
        }
    }
    Ok(resolved)
}

fn push_components(path: &Path, queue: &mut VecDeque<OsString>) {
    for component in path.components() {
        match component {
            Component::Normal(part) => queue.push_back(part.to_os_string()),
            Component::ParentDir => queue.push_back(OsString::from("..")),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
}

/// Why *bytes* is not valid UTF-8, formatted exactly like CPython's
/// `UnicodeDecodeError.__str__` (`'utf-8' codec can't decode byte 0xXX
/// in position N: <reason>` for a single bad byte, or `can't decode
/// bytes in position N-M: <reason>` when one or more otherwise-valid
/// continuation bytes were already read before the failure) -- the
/// exact table CPython's own UTF-8 decoder uses: a lead byte in
/// `0x80..=0xC1`/`0xF5..=0xFF` is invalid outright ("invalid start
/// byte"); `0xE0`/`0xED`/`0xF0`/`0xF4` narrow their *first* continuation
/// byte's valid range to exclude overlong encodings, surrogates, and
/// codepoints past `U+10FFFF`; every other continuation byte must fall
/// in `0x80..=0xBF`.
fn describe_utf8_error(bytes: &[u8]) -> String {
    let (start, consumed, reason) =
        find_utf8_error(bytes).expect("describe_utf8_error called on valid UTF-8");
    if consumed == 1 {
        format!(
            "'utf-8' codec can't decode byte 0x{:02x} in position {start}: {reason}",
            bytes[start]
        )
    } else {
        format!(
            "'utf-8' codec can't decode bytes in position {start}-{}: {reason}",
            start + consumed - 1
        )
    }
}

/// The first invalid byte sequence in *bytes*: its start index, how many
/// leading bytes of that sequence (the lead byte plus any valid
/// continuation bytes already read) were consumed before the failure,
/// and why. `None` when *bytes* is entirely valid UTF-8.
fn find_utf8_error(bytes: &[u8]) -> Option<(usize, usize, &'static str)> {
    let mut i = 0;
    while i < bytes.len() {
        let lead = bytes[i];
        if lead < 0x80 {
            i += 1;
            continue;
        }
        let Some((total_len, min1, max1)) = utf8_lead_shape(lead) else {
            return Some((i, 1, "invalid start byte"));
        };
        // `seq_idx` doubles as the already-validated byte count (lead
        // plus every continuation byte validated so far) when a check
        // below fails at it: at `seq_idx == 1` only the lead byte has
        // been read, matching the single-byte "byte 0xXX" message form.
        for seq_idx in 1..total_len {
            let Some(&cont) = bytes.get(i + seq_idx) else {
                return Some((i, seq_idx, "unexpected end of data"));
            };
            let (lo, hi) = if seq_idx == 1 {
                (min1, max1)
            } else {
                (0x80, 0xBF)
            };
            if cont < lo || cont > hi {
                return Some((i, seq_idx, "invalid continuation byte"));
            }
        }
        i += total_len;
    }
    None
}

/// For a valid UTF-8 lead byte: `(total sequence length, first
/// continuation byte's min, first continuation byte's max)` -- `None`
/// for a byte that can never lead a sequence (`0x80..=0xC1`, a stray
/// continuation byte or an overlong 2-byte lead; `0xF5..=0xFF`, past the
/// max codepoint).
fn utf8_lead_shape(lead: u8) -> Option<(usize, u8, u8)> {
    match lead {
        0xC2..=0xDF => Some((2, 0x80, 0xBF)),
        0xE0 => Some((3, 0xA0, 0xBF)),
        0xE1..=0xEC => Some((3, 0x80, 0xBF)),
        0xED => Some((3, 0x80, 0x9F)),
        0xEE..=0xEF => Some((3, 0x80, 0xBF)),
        0xF0 => Some((4, 0x90, 0xBF)),
        0xF1..=0xF3 => Some((4, 0x80, 0xBF)),
        0xF4 => Some((4, 0x80, 0x8F)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;
    use std::fs;
    use std::ops::Deref;
    use std::os::unix::fs::symlink;

    fn origin_at(dir: &Path) -> DocumentOrigin {
        DocumentOrigin::File {
            document_dir: dir.to_path_buf(),
            confinement_root: dir.to_path_buf(),
        }
    }

    /// A temporary directory removed when dropped, so a test's own
    /// scratch tree doesn't outlive it on disk.
    struct TempDir(PathBuf);

    impl Deref for TempDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(tag: &str) -> TempDir {
        let root = std::env::temp_dir().join(format!(
            "electricity-prompt-files-test-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        fs::create_dir_all(&root).unwrap();
        TempDir(fs::canonicalize(&root).unwrap())
    }

    fn prompts_document(entries: Vec<(&str, Value)>) -> Value {
        let mut dict = Dict::new();
        let mut prompts = Dict::new();
        for (k, v) in entries {
            prompts.insert(Value::Str(k.to_string()), v);
        }
        dict.insert(Value::Str("prompts".to_string()), Value::Dict(prompts));
        Value::Dict(dict)
    }

    fn file_value(path: &str) -> Value {
        let mut dict = Dict::new();
        dict.insert(Value::Str("file".to_string()), Value::Str(path.to_string()));
        Value::Dict(dict)
    }

    #[test]
    fn document_with_no_prompts_key_passes_through() {
        let document = Value::Dict(Dict::new());
        let dir = temp_dir("no-prompts");
        let result = compile_declared_prompts(&document, &origin_at(&dir));
        assert_eq!(result, Ok(IndexMap::new()));
    }

    #[test]
    fn null_prompts_key_passes_through() {
        let mut dict = Dict::new();
        dict.insert(Value::Str("prompts".to_string()), Value::None);
        let dir = temp_dir("null-prompts");
        let result = compile_declared_prompts(&Value::Dict(dict), &origin_at(&dir));
        assert_eq!(result, Ok(IndexMap::new()));
    }

    #[test]
    fn non_mapping_prompts_is_rejected() {
        let mut dict = Dict::new();
        dict.insert(
            Value::Str("prompts".to_string()),
            Value::Str("x".to_string()),
        );
        let dir = temp_dir("non-mapping-prompts");
        let err = compile_declared_prompts(&Value::Dict(dict), &origin_at(&dir)).unwrap_err();
        assert_eq!(err.0, "'prompts' must be a mapping of name to template.");
    }

    #[test]
    fn inline_text_prompt_passes_through_unchanged() {
        let document = prompts_document(vec![("greeting", Value::Str("hi there".to_string()))]);
        let dir = temp_dir("inline-text");
        let result = compile_declared_prompts(&document, &origin_at(&dir)).unwrap();
        assert_eq!(result.get("greeting").unwrap(), "hi there");
    }

    #[test]
    fn dotted_prompt_name_is_rejected() {
        let document = prompts_document(vec![("a.b", Value::Str("x".to_string()))]);
        let dir = temp_dir("dotted-name");
        let err = compile_declared_prompts(&document, &origin_at(&dir)).unwrap_err();
        assert_eq!(
            err.0,
            "'prompts' key 'a.b' must be a valid name ([A-Za-z_][A-Za-z0-9_]*, no '.')."
        );
    }

    #[test]
    fn a_single_trailing_newline_in_a_prompt_key_is_accepted() {
        // Python's own `$` (no `re.MULTILINE`) matches just before one
        // trailing newline too, so `_NAME_SHAPE.match("greeting\n")`
        // still succeeds -- this port must accept the same key.
        let document = prompts_document(vec![("greeting\n", Value::Str("hi".to_string()))]);
        let dir = temp_dir("trailing-newline-key");
        let result = compile_declared_prompts(&document, &origin_at(&dir)).unwrap();
        assert_eq!(result.get("greeting\n").unwrap(), "hi");
    }

    #[test]
    fn two_trailing_newlines_in_a_prompt_key_are_rejected() {
        let document = prompts_document(vec![("greeting\n\n", Value::Str("hi".to_string()))]);
        let dir = temp_dir("double-trailing-newline-key");
        let err = compile_declared_prompts(&document, &origin_at(&dir)).unwrap_err();
        assert_eq!(
            err.0,
            "'prompts' key 'greeting\\n\\n' must be a valid name ([A-Za-z_][A-Za-z0-9_]*, no '.')."
        );
    }

    #[test]
    fn non_string_prompt_key_is_rejected() {
        let mut prompts = Dict::new();
        prompts.insert(Value::Int(5.into()), Value::Str("x".to_string()));
        let mut dict = Dict::new();
        dict.insert(Value::Str("prompts".to_string()), Value::Dict(prompts));
        let dir = temp_dir("non-string-key");
        let err = compile_declared_prompts(&Value::Dict(dict), &origin_at(&dir)).unwrap_err();
        assert_eq!(
            err.0,
            "'prompts' key 5 must be a valid name ([A-Za-z_][A-Za-z0-9_]*, no '.')."
        );
    }

    #[test]
    fn file_prompt_reads_and_translates_newlines() {
        let dir = temp_dir("file-newlines");
        fs::write(dir.join("voice.md"), "line one\r\nline two\rline three").unwrap();
        let document = prompts_document(vec![("voice", file_value("voice.md"))]);
        let result = compile_declared_prompts(&document, &origin_at(&dir)).unwrap();
        assert_eq!(
            result.get("voice").unwrap(),
            "line one\nline two\nline three"
        );
    }

    #[test]
    fn missing_file_is_an_error() {
        let dir = temp_dir("missing-file");
        let document = prompts_document(vec![("voice", file_value("nope.md"))]);
        let err = compile_declared_prompts(&document, &origin_at(&dir)).unwrap_err();
        assert_eq!(err.0, "prompts.voice: 'file' 'nope.md' does not exist.");
    }

    #[test]
    fn directory_as_file_is_not_a_regular_file() {
        let dir = temp_dir("dir-as-file");
        fs::create_dir(dir.join("sub")).unwrap();
        let document = prompts_document(vec![("voice", file_value("sub"))]);
        let err = compile_declared_prompts(&document, &origin_at(&dir)).unwrap_err();
        assert_eq!(err.0, "prompts.voice: 'file' 'sub' is not a regular file.");
    }

    #[test]
    fn oversized_file_is_rejected() {
        let dir = temp_dir("oversized");
        fs::write(
            dir.join("big.md"),
            vec![b'x'; (MAX_PROMPT_FILE_BYTES + 1) as usize],
        )
        .unwrap();
        let document = prompts_document(vec![("voice", file_value("big.md"))]);
        let err = compile_declared_prompts(&document, &origin_at(&dir)).unwrap_err();
        assert_eq!(
            err.0,
            format!(
                "prompts.voice: 'file' 'big.md' is {} bytes, over the {MAX_PROMPT_FILE_BYTES}-byte \
                 limit for a prompt file.",
                MAX_PROMPT_FILE_BYTES + 1
            )
        );
    }

    #[test]
    fn non_utf8_file_reports_cpythons_message() {
        let dir = temp_dir("non-utf8");
        fs::write(dir.join("bad.md"), [b'a', b'b', b'c', 0xff, b'd']).unwrap();
        let document = prompts_document(vec![("voice", file_value("bad.md"))]);
        let err = compile_declared_prompts(&document, &origin_at(&dir)).unwrap_err();
        assert_eq!(
            err.0,
            "prompts.voice: 'file' 'bad.md' is not valid UTF-8: 'utf-8' codec can't \
             decode byte 0xff in position 3: invalid start byte"
        );
    }

    #[test]
    fn absolute_file_path_is_rejected() {
        let dir = temp_dir("absolute-path");
        let document = prompts_document(vec![("voice", file_value("/etc/passwd"))]);
        let err = compile_declared_prompts(&document, &origin_at(&dir)).unwrap_err();
        assert_eq!(
            err.0,
            "prompts.voice: 'file' must be a relative path, got absolute path '/etc/passwd'."
        );
    }

    #[test]
    fn template_tag_in_path_is_rejected() {
        let dir = temp_dir("template-tag");
        let document = prompts_document(vec![("voice", file_value("{{input.name}}.md"))]);
        let err = compile_declared_prompts(&document, &origin_at(&dir)).unwrap_err();
        assert_eq!(
            err.0,
            "prompts.voice: 'file' must be a literal path — it cannot contain a \
             template tag: '{{input.name}}.md'."
        );
    }

    #[test]
    fn symlink_escaping_the_project_is_rejected() {
        let project = temp_dir("symlink-project");
        let outside = temp_dir("symlink-outside");
        fs::write(outside.join("secret.md"), "shh").unwrap();
        symlink(outside.join("secret.md"), project.join("link.md")).unwrap();
        let document = prompts_document(vec![("voice", file_value("link.md"))]);
        let err = compile_declared_prompts(&document, &origin_at(&project)).unwrap_err();
        assert!(
            err.0.contains("resolves outside the project"),
            "unexpected error: {}",
            err.0
        );
    }

    #[test]
    fn symlink_staying_inside_the_project_is_allowed() {
        let project = temp_dir("symlink-inside");
        fs::create_dir(project.join("sub")).unwrap();
        fs::write(project.join("sub/real.md"), "hi").unwrap();
        symlink(project.join("sub/real.md"), project.join("link.md")).unwrap();
        let document = prompts_document(vec![("voice", file_value("link.md"))]);
        let result = compile_declared_prompts(&document, &origin_at(&project)).unwrap();
        assert_eq!(result.get("voice").unwrap(), "hi");
    }

    #[test]
    fn file_on_a_generated_document_is_refused() {
        let document = prompts_document(vec![("voice", file_value("voice.md"))]);
        let err = compile_declared_prompts(&document, &DocumentOrigin::Generated).unwrap_err();
        assert!(
            err.0.contains("has no file of its own"),
            "unexpected error: {}",
            err.0
        );
    }

    #[test]
    fn non_string_file_value_is_rejected() {
        let mut file_dict = Dict::new();
        file_dict.insert(Value::Str("file".to_string()), Value::Int(5.into()));
        let document = prompts_document(vec![("voice", Value::Dict(file_dict))]);
        let dir = temp_dir("non-string-file-value");
        let err = compile_declared_prompts(&document, &origin_at(&dir)).unwrap_err();
        assert_eq!(
            err.0,
            "prompts.voice: 'file' must be a non-empty string path."
        );
    }

    #[test]
    fn non_string_non_file_value_is_rejected() {
        let document = prompts_document(vec![("voice", Value::Int(5.into()))]);
        let dir = temp_dir("non-string-non-file-value");
        let err = compile_declared_prompts(&document, &origin_at(&dir)).unwrap_err();
        assert_eq!(err.0, "prompts.voice must be a string or {file: <path>}.");
    }

    #[test]
    fn resolve_non_strict_leaves_a_missing_trailing_component_literal() {
        let dir = temp_dir("non-strict-missing");
        let resolved = resolve_non_strict(&dir.join("does/not/exist.md")).unwrap();
        assert_eq!(resolved, dir.join("does/not/exist.md"));
    }

    #[test]
    fn resolve_non_strict_normalizes_dot_dot() {
        let dir = temp_dir("non-strict-dotdot");
        fs::create_dir(dir.join("sub")).unwrap();
        let resolved = resolve_non_strict(&dir.join("sub/../sub/x.md")).unwrap();
        assert_eq!(resolved, dir.join("sub/x.md"));
    }

    #[test]
    fn universal_newlines_handles_mixed_line_endings() {
        assert_eq!(universal_newlines("a\r\nb\rc\nd"), "a\nb\nc\nd");
        assert_eq!(universal_newlines("a\r\r\nb"), "a\n\nb");
    }
}
