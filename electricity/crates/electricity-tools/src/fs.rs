//! `plugins/fs.py::FsPlugin`, ported -- `read`/`write`/`append`/`list`/
//! `stat`/`exists`/`delete`, byte-identical error texts and `raw` shapes.
//!
//! This file also carries the shared helpers `env_vars.rs`/`hash.rs`/
//! `uuid.rs`/`clock.rs` use too (`as_bool`/`py_truthy`/`get`/the codec
//! table/the `[Errno N] <strerror>: '<path>'` OS-error formatter) --
//! `plugins/base.py::_as_bool` and Python's own string-codec machinery
//! have no other natural home among this lane's own five files, and
//! splitting them into a brand-new module would add a `mod` line to
//! `lib.rs`'s own list (vm-lanes.md: this lane only ever fills a body
//! in a file the gate already created).

use crate::{CheckResult, PyExc, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::{Dict, Value};
use std::io;
use std::path::{Component, Path, PathBuf};

// ---------------------------------------------------------------------
// Shared helpers (used by this file and env_vars.rs/hash.rs/uuid.rs)
// ---------------------------------------------------------------------

/// `params.get(key)`, by `Value::Str` key -- `plugins/json.py`'s own
/// `get` helper, duplicated here since every plugin in this lane reads
/// its own params dict the same way.
pub(crate) fn get<'a>(params: &'a Value, key: &str) -> Option<&'a Value> {
    params.as_dict()?.get(&Value::Str(key.to_string()))
}

/// A plain `ValueError`-classed [`ToolError`] -- every validation
/// failure in this lane's five plugins (`plugins/fs.py`/`env_vars.py`/
/// `hash.py`/`uuid.py`/`clock.py` all raise bare `ValueError`s for their
/// own param checks).
pub(crate) fn value_error(message: impl Into<String>) -> ToolError {
    ToolError {
        py_class: PyExc::ValueError,
        message: message.into(),
        cause: crate::ErrorCause::None,
    }
}

/// Python truthiness (`electricity-template::render::truthy`'s own
/// twin -- every crate that needs this keeps its own small copy rather
/// than share one, per this workspace's own convention): `None`,
/// `False`, a numeric zero, and any empty `str`/`bytes`/`list`/`dict`
/// are falsy; everything else (including `NaN`) is truthy.
pub(crate) fn py_truthy(value: &Value) -> bool {
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

/// `plugins/base.py::_as_bool`: `None` (key absent, or an explicit
/// `null`) returns *default*; a `bool` passes through; a `str` is
/// false for `false`/`0`/`no`/`off`/`n`/empty (after `strip().lower()`,
/// Python whitespace rules via [`electricity_value::pycompat::py_strip`]);
/// anything else falls back to Python truthiness.
pub(crate) fn as_bool(value: Option<&Value>, default: bool) -> bool {
    match value {
        None | Some(Value::None) => default,
        Some(Value::Bool(b)) => *b,
        Some(Value::Str(s)) => {
            let normalized = electricity_value::pycompat::py_strip(s).to_lowercase();
            !matches!(normalized.as_str(), "false" | "0" | "no" | "off" | "n" | "")
        }
        Some(other) => py_truthy(other),
    }
}

/// `exc.strerror`: the bare OS error message, without Rust's own
/// `io::Error::to_string()` " (os error N)" suffix -- the same
/// technique `electricity-config::config::strerror` uses (duplicated
/// here: that crate isn't this lane's to edit, and isn't a dependency
/// of this one).
fn strerror(err: &io::Error) -> String {
    let text = err.to_string();
    match text.rfind(" (os error ") {
        Some(idx) if text.ends_with(')') => text[..idx].to_string(),
        _ => text,
    }
}

/// CPython's `OSError.__str__` for an errno-carrying exception:
/// `[Errno N] <strerror>: '<path>'`. The `py_class` this maps to is
/// reproduced exactly for `ENOENT`/`EACCES`/`EISDIR`/`ENOTDIR`
/// (`FileNotFoundError`/`PermissionError`/`IsADirectoryError`/
/// `NotADirectoryError`); every other errno is a bare `OSError` --
/// issue #460's own acceptance text: "Reproduce ENOENT/EISDIR/EACCES/
/// ENOTDIR exactly; compare the rest by location."
pub(crate) fn os_error(err: &io::Error, path_display: &str) -> ToolError {
    let errno = err.raw_os_error().unwrap_or(0);
    let py_class = match errno {
        1 => PyExc::PermissionError,     // EPERM
        2 => PyExc::FileNotFoundError,   // ENOENT
        13 => PyExc::PermissionError,    // EACCES
        21 => PyExc::IsADirectoryError,  // EISDIR
        20 => PyExc::NotADirectoryError, // ENOTDIR
        _ => PyExc::OSError,
    };
    ToolError {
        py_class,
        message: format!("[Errno {errno}] {}: '{path_display}'", strerror(err)),
        cause: crate::ErrorCause::None,
    }
}

// ---------------------------------------------------------------------
// Path handling: `_validate_path` (`Path(raw).expanduser()`)
// ---------------------------------------------------------------------

/// A path normalized the way `pathlib.PurePath` parses one (collapsing
/// repeated separators, dropping `.` components and a trailing slash,
/// keeping `..` as a literal component -- never resolving symlinks or
/// an actual filesystem lookup) plus `expanduser()`'s own leading-`~`
/// substitution.
pub(crate) struct FsPath {
    pub(crate) buf: PathBuf,
    /// `str(path)` -- what every `fs`/`hash` message embeds.
    pub(crate) display: String,
}

fn split_components(raw: &str) -> (bool, Vec<String>) {
    let mut absolute = false;
    let mut parts = Vec::new();
    for component in Path::new(raw).components() {
        match component {
            Component::RootDir => absolute = true,
            Component::CurDir => {}
            Component::ParentDir => parts.push("..".to_string()),
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::Prefix(_) => {}
        }
    }
    (absolute, parts)
}

fn render_path(absolute: bool, parts: &[String]) -> String {
    if absolute {
        if parts.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", parts.join("/"))
        }
    } else if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// `Path(raw).expanduser()` -- only the plain `~` (current user's own
/// `$HOME`) form is substituted; a `~username` form is left unchanged
/// (a documented, narrow divergence from `pwd`-backed resolution of
/// another account's home directory, never exercised by a hermetic
/// test run under a temp `$HOME`).
pub(crate) fn expand_user_path(raw: &str) -> FsPath {
    let (mut absolute, mut parts) = split_components(raw);
    if !absolute && parts.first().map(String::as_str) == Some("~") {
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                let (home_absolute, home_parts) = split_components(&home);
                parts.remove(0);
                let mut merged = home_parts;
                merged.extend(parts);
                parts = merged;
                absolute = home_absolute;
            }
        }
    }
    let display = render_path(absolute, &parts);
    FsPath {
        buf: PathBuf::from(&display),
        display,
    }
}

/// `plugins/fs.py::_validate_path`.
pub(crate) fn validate_path(raw: Option<&Value>) -> Result<FsPath, ToolError> {
    let s = match raw {
        Some(Value::Str(s)) if !s.is_empty() => s,
        _ => {
            return Err(value_error(
                "fs: params['path'] must be a non-empty string.",
            ));
        }
    };
    if s.contains('\0') {
        return Err(value_error("fs: path contains null byte."));
    }
    Ok(expand_user_path(s))
}

// ---------------------------------------------------------------------
// Codec table: `encoding = str(params.get("encoding") or "utf-8")`
// ---------------------------------------------------------------------

/// The one Python codec family this lane actually implements --
/// `utf-8`, `ascii` and `latin-1`, plus every alias of theirs
/// `encodings.aliases.aliases` lists. Any other real Python codec name
/// (`cp1252`, `utf-16`, `shift_jis`, ...) is a documented narrowing:
/// this plugin reports `LookupError: unknown encoding: x` for a codec
/// CPython itself supports, rather than reproducing its own encode/
/// decode byte-for-byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Codec {
    Utf8,
    Ascii,
    Latin1,
}

impl Codec {
    fn display_name(self) -> &'static str {
        match self {
            Codec::Utf8 => "utf-8",
            Codec::Ascii => "ascii",
            Codec::Latin1 => "latin-1",
        }
    }
}

/// `encodings.normalize_encoding`, folded together with the
/// lowercasing/space-to-dash pass `codecs.lookup` itself does first
/// (`Python/codecs.c::normalizestring`) -- net effect: lowercase, then
/// collapse every run of characters that isn't `[a-z0-9.]` into a
/// single underscore, trimmed from both ends.
fn normalize_codec_name(name: &str) -> String {
    let mut out = String::new();
    let mut pending_sep = false;
    for ch in name.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() || lower == '.' {
            if pending_sep && !out.is_empty() {
                out.push('_');
            }
            out.push(lower);
            pending_sep = false;
        } else {
            pending_sep = true;
        }
    }
    out
}

/// `encodings.aliases.aliases`, narrowed to the entries that resolve to
/// one of the three codecs this lane implements.
const UTF8_ALIASES: &[&str] = &["utf8", "u8", "utf", "cp65001", "utf8_ucs2", "utf8_ucs4"];
const ASCII_ALIASES: &[&str] = &[
    "646",
    "ansi_x3.4_1968",
    "ansi_x3.4_1986",
    "ansi_x3_4_1968",
    "cp367",
    "csascii",
    "ibm367",
    "iso646_us",
    "iso_646.irv_1991",
    "iso_ir_6",
    "us",
    "us_ascii",
];
const LATIN1_ALIASES: &[&str] = &[
    "8859",
    "cp819",
    "csisolatin1",
    "ibm819",
    "iso8859",
    "iso8859_1",
    "iso_8859_1",
    "iso_8859_1_1987",
    "iso_ir_100",
    "l1",
    "latin",
    "latin1",
];

pub(crate) fn resolve_codec(name: &str) -> Option<Codec> {
    let normalized = normalize_codec_name(name);
    match normalized.as_str() {
        "utf_8" => Some(Codec::Utf8),
        "ascii" => Some(Codec::Ascii),
        "latin_1" => Some(Codec::Latin1),
        other => {
            if UTF8_ALIASES.contains(&other) {
                Some(Codec::Utf8)
            } else if ASCII_ALIASES.contains(&other) {
                Some(Codec::Ascii)
            } else if LATIN1_ALIASES.contains(&other) {
                Some(Codec::Latin1)
            } else {
                None
            }
        }
    }
}

/// `codecs.lookup(x)`'s own failure: `LookupError: unknown encoding: x`
/// -- *original* is the un-normalized string the caller passed in.
pub(crate) fn unknown_encoding(original: &str) -> ToolError {
    ToolError {
        py_class: PyExc::LookupError,
        message: format!("unknown encoding: {original}"),
        cause: crate::ErrorCause::None,
    }
}

/// `\xXX`/`\uXXXX`/`\UXXXXXXXX`, quoted -- the character-repr form
/// CPython's `Unicode_EncodeError`/`DecodeError` messages embed
/// (`'%c'` precision rules in `Objects/exceptions.c::
/// UnicodeEncodeError_str`): a codepoint `<= 0xFF` uses the 2-digit
/// `\x` form, `<= 0xFFFF` the 4-digit `\u` form, else the 8-digit `\U`
/// form.
fn py_char_repr(ch: char) -> String {
    let cp = ch as u32;
    if cp <= 0xFF {
        format!("'\\x{cp:02x}'")
    } else if cp <= 0xFFFF {
        format!("'\\u{cp:04x}'")
    } else {
        format!("'\\U{cp:08x}'")
    }
}

/// CPython's own encoder scans forward from the first unencodable
/// character to the end of the *run* of characters that are all also
/// unencodable, and invokes the error handler once for that whole run
/// -- `'ascii' codec can't encode characters in position 0-1: ...` for
/// `"\u65e5\u672c".encode("ascii")`, not one error per character.
/// *chars* is the run (`len() >= 1`); *start*/*end* are its inclusive
/// bounds in the original string.
fn unicode_encode_error_run(
    codec: Codec,
    chars: &[char],
    start: usize,
    end: usize,
    limit: u32,
) -> ToolError {
    let body = if end > start {
        format!("characters in position {start}-{end}")
    } else {
        format!("character {} in position {start}", py_char_repr(chars[0]))
    };
    ToolError {
        py_class: PyExc::UnicodeEncodeError,
        message: format!(
            "'{}' codec can't encode {body}: ordinal not in range({limit})",
            codec.display_name()
        ),
        cause: crate::ErrorCause::None,
    }
}

/// Why *bytes* is not valid UTF-8, formatted exactly like CPython's
/// `UnicodeDecodeError.__str__` -- the exact table CPython's own
/// UTF-8 decoder uses (duplicated from `electricity-compiler::
/// prompt_files::describe_utf8_error`/`find_utf8_error`/
/// `utf8_lead_shape`: that crate isn't this lane's to edit, and isn't
/// a dependency of this one).
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

/// `bytes.decode(encoding)`, strict errors.
pub(crate) fn decode_text(codec: Codec, bytes: &[u8]) -> Result<String, ToolError> {
    match codec {
        Codec::Utf8 => std::str::from_utf8(bytes)
            .map(str::to_string)
            .map_err(|_| ToolError {
                py_class: PyExc::UnicodeDecodeError,
                message: describe_utf8_error(bytes),
                cause: crate::ErrorCause::None,
            }),
        Codec::Ascii => {
            if let Some((position, &byte)) = bytes.iter().enumerate().find(|&(_, &b)| b >= 0x80) {
                return Err(ToolError {
                    py_class: PyExc::UnicodeDecodeError,
                    message: format!(
                        "'ascii' codec can't decode byte 0x{byte:02x} in position {position}: ordinal not in range(128)"
                    ),
                    cause: crate::ErrorCause::None,
                });
            }
            Ok(bytes.iter().map(|&b| b as char).collect())
        }
        Codec::Latin1 => Ok(bytes.iter().map(|&b| b as char).collect()),
    }
}

/// `str.encode(encoding)`, strict errors.
pub(crate) fn encode_text(codec: Codec, text: &str) -> Result<Vec<u8>, ToolError> {
    match codec {
        Codec::Utf8 => Ok(text.as_bytes().to_vec()),
        Codec::Ascii => encode_single_byte(codec, text, 128),
        Codec::Latin1 => encode_single_byte(codec, text, 256),
    }
}

/// Shared by [`Codec::Ascii`] (limit 128) and [`Codec::Latin1`] (limit
/// 256): both store one byte per codepoint, differing only in which
/// codepoints are in range.
fn encode_single_byte(codec: Codec, text: &str, limit: u32) -> Result<Vec<u8>, ToolError> {
    let chars: Vec<char> = text.chars().collect();
    for (i, &ch) in chars.iter().enumerate() {
        if (ch as u32) >= limit {
            let mut end = i;
            while end + 1 < chars.len() && (chars[end + 1] as u32) >= limit {
                end += 1;
            }
            return Err(unicode_encode_error_run(
                codec,
                &chars[i..=end],
                i,
                end,
                limit,
            ));
        }
    }
    Ok(chars.into_iter().map(|c| c as u8).collect())
}

/// What a requested codec name resolves to, three ways -- #460's own
/// lane D1 fix pass ("R2"): a name CPython itself would also reject
/// gets `cof run`'s own `LookupError: unknown encoding: x`
/// ([`Unknown`](EncodingOutcome::Unknown)); a name CPython accepts but
/// this preview doesn't implement gets the preview marker instead of a
/// false `unknown encoding` claim ([`NotImplemented`](EncodingOutcome::NotImplemented)).
pub(crate) enum EncodingOutcome {
    Supported(Codec),
    NotImplemented(String),
    Unknown(String),
}

/// `str(params.get("encoding") or "utf-8")` -- the raw name every
/// `fs`/`hash` encoding param resolves to before classification.
pub(crate) fn encoding_param_name(params: &Value) -> String {
    let raw = get(params, "encoding").cloned().unwrap_or(Value::None);
    if py_truthy(&raw) {
        raw.py_str()
    } else {
        "utf-8".to_string()
    }
}

/// Classifies *name* per [`EncodingOutcome`], against
/// `generate_codec_names.py`'s own table of every name CPython's
/// `codecs.lookup` accepts.
pub(crate) fn classify_encoding(name: &str) -> EncodingOutcome {
    if let Some(codec) = resolve_codec(name) {
        return EncodingOutcome::Supported(codec);
    }
    let normalized = normalize_codec_name(name);
    if crate::generated::python_codec_names::PYTHON_CODEC_NAMES.contains(&normalized.as_str()) {
        EncodingOutcome::NotImplemented(name.to_string())
    } else {
        EncodingOutcome::Unknown(name.to_string())
    }
}

/// The preview-marker refusal for a codec name real `cof run` supports
/// but this preview doesn't implement -- raised *before* anything
/// touches the filesystem, unlike the `Unknown`/`UnicodeEncodeError`
/// cases below, which (R2's own ruling) reproduce `cof run`'s own
/// filesystem effects first.
pub(crate) fn preview_marker_codec_error(name: &str) -> ToolError {
    ToolError {
        py_class: PyExc::LookupError,
        message: format!(
            "electricity {} is a preview and can't run this orchestration: the '{name}' codec is not supported; use `cof run` instead",
            env!("CARGO_PKG_VERSION")
        ),
        cause: crate::ErrorCause::None,
    }
}

/// `read_text()`'s own universal-newline translation: `\r\n` and a
/// lone `\r` both become `\n`.
fn universal_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn stat_dict(path: &Path, display: &str, metadata: &std::fs::Metadata) -> Result<Value, ToolError> {
    use std::os::unix::fs::MetadataExt;
    let mut dict = Dict::new();
    dict.insert(
        Value::Str("size".to_string()),
        Value::from(metadata.len() as i64),
    );
    dict.insert(
        Value::Str("mtime".to_string()),
        Value::from(py_mtime(metadata)),
    );
    dict.insert(
        Value::Str("mode".to_string()),
        Value::from(metadata.mode() as i64),
    );
    dict.insert(
        Value::Str("is_dir".to_string()),
        Value::from(py_is_dir(path, display)?),
    );
    dict.insert(
        Value::Str("is_file".to_string()),
        Value::from(py_is_file(path, display)?),
    );
    Ok(Value::Dict(dict))
}

/// `int(st_mtime)` -- Python's own truncation toward zero of the
/// *float* seconds-since-epoch, which for a pre-1970 timestamp with a
/// nonzero fractional (nanosecond) part gives a *less* negative result
/// than `st_mtime`'s own whole-seconds part (`MetadataExt::mtime()`,
/// always rounded toward negative infinity: the kernel's `tv_nsec` is
/// never negative, so `tv_sec` alone floors the true value).
fn py_mtime(metadata: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    truncate_toward_zero(metadata.mtime(), metadata.mtime_nsec())
}

/// The pure arithmetic `py_mtime` applies -- split out so a test can
/// check it directly against synthetic `(seconds, nanos)` pairs,
/// rather than relying on a real filesystem's own sub-second mtime
/// storage/rounding (which, in a CI sandbox, isn't guaranteed to keep
/// the exact value this is testing for).
fn truncate_toward_zero(seconds: i64, nanos: i64) -> i64 {
    if seconds < 0 && nanos != 0 {
        seconds + 1
    } else {
        seconds
    }
}

/// The errno set `pathlib.Path.exists`/`is_file`/`is_dir` (CPython
/// 3.11/3.12; 3.13 swallows every `OSError` instead, a documented
/// narrowing -- `docs/spec/tools-adapters-plugins.md` §2.3) re-raise
/// *through*, rather than reporting `False` for: everything except a
/// missing path, a path through a non-directory, a bad descriptor or
/// too many symlink levels.
fn ignorable_stat_errno(errno: i32) -> bool {
    matches!(
        errno,
        libc::ENOENT | libc::ENOTDIR | libc::EBADF | libc::ELOOP
    )
}

pub(crate) fn py_exists(path: &Path, display: &str) -> Result<bool, ToolError> {
    match std::fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if ignorable_stat_errno(err.raw_os_error().unwrap_or(-1)) => Ok(false),
        Err(err) => Err(os_error(&err, display)),
    }
}

pub(crate) fn py_is_dir(path: &Path, display: &str) -> Result<bool, ToolError> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(err) if ignorable_stat_errno(err.raw_os_error().unwrap_or(-1)) => Ok(false),
        Err(err) => Err(os_error(&err, display)),
    }
}

pub(crate) fn py_is_file(path: &Path, display: &str) -> Result<bool, ToolError> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(err) if ignorable_stat_errno(err.raw_os_error().unwrap_or(-1)) => Ok(false),
        Err(err) => Err(os_error(&err, display)),
    }
}

/// `Path.mkdir(parents=True, exist_ok=True)` (CPython's own
/// `pathlib._local.Path.mkdir`), ported exactly so a failure names the
/// *directory that actually failed* rather than always the original
/// write's own leaf path (issue #460's own fix-pass finding 7): try
/// creating *path* itself first; on `ENOENT` (a missing parent),
/// recurse into the parent and retry; on any other `OSError`, swallow
/// it only if *path* already is a directory (the race-safe `exist_ok`
/// case), else report *path* itself.
fn mkdir_parents(path: &Path) -> Result<(), ToolError> {
    let display = path.to_string_lossy().into_owned();
    match std::fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(err) if err.raw_os_error() == Some(libc::ENOENT) => match path.parent() {
            Some(parent) if parent != path && !parent.as_os_str().is_empty() => {
                mkdir_parents(parent)?;
                match std::fs::create_dir(path) {
                    Ok(()) => Ok(()),
                    Err(retry_err) => mkdir_exist_ok_or_raise(path, &display, retry_err),
                }
            }
            _ => Err(os_error(&err, &display)),
        },
        Err(err) => mkdir_exist_ok_or_raise(path, &display, err),
    }
}

fn mkdir_exist_ok_or_raise(path: &Path, display: &str, err: io::Error) -> Result<(), ToolError> {
    if py_is_dir(path, display)? {
        Ok(())
    } else {
        Err(os_error(&err, display))
    }
}

pub struct FsPlugin;

#[async_trait(?Send)]
impl ToolPlugin for FsPlugin {
    fn name(&self) -> &str {
        "fs"
    }

    async fn execute(&self, params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        // `mode = str(params.get("mode", "read")).lower()` -- the
        // default applies only when the key is *absent*; an explicit
        // `mode: null` becomes the literal string `"none"`.
        let mode_value = get(&params, "mode")
            .cloned()
            .unwrap_or_else(|| Value::Str("read".to_string()));
        let mode = mode_value.py_str().to_lowercase();

        let fs_path = validate_path(get(&params, "path"))?;
        let path = fs_path.buf.as_path();
        let display = fs_path.display.clone();

        let mut raw = Dict::new();
        raw.insert(Value::Str("mode".to_string()), Value::Str(mode.clone()));
        raw.insert(Value::Str("path".to_string()), Value::Str(display.clone()));

        let value: Value = match mode.as_str() {
            "read" => {
                if !py_is_file(path, &display)? {
                    return Err(ToolError {
                        py_class: PyExc::FileNotFoundError,
                        message: format!("fs: not a file: {display}"),
                        cause: crate::ErrorCause::None,
                    });
                }
                let encoding_name = encoding_param_name(&params);
                let codec = match classify_encoding(&encoding_name) {
                    EncodingOutcome::Supported(codec) => codec,
                    EncodingOutcome::NotImplemented(name) => {
                        return Err(preview_marker_codec_error(&name));
                    }
                    EncodingOutcome::Unknown(name) => return Err(unknown_encoding(&name)),
                };
                let bytes = std::fs::read(path).map_err(|err| os_error(&err, &display))?;
                let text = decode_text(codec, &bytes)?;
                Value::Str(universal_newlines(&text))
            }
            "write" | "append" => {
                let content = match get(&params, "content") {
                    Some(Value::Str(s)) => s.clone(),
                    _ => {
                        return Err(value_error(
                            "fs: write/append requires params['content'] as str.",
                        ));
                    }
                };
                // Codec classification happens before anything touches
                // the filesystem, but only a `NotImplemented` name is
                // actually raised this early (R2): a name CPython
                // itself would reject, or one we do support, instead
                // waits until after `mkdir`/`open` below, so a failure
                // leaves the same filesystem state `cof run` does
                // (issue #460's own fix-pass finding 6 -- CPython's
                // `open(path, "w", encoding=...)` already created or
                // truncated the file before either exception could
                // fire).
                let encoding_name = encoding_param_name(&params);
                let encoding_outcome = classify_encoding(&encoding_name);
                if let EncodingOutcome::NotImplemented(name) = &encoding_outcome {
                    return Err(preview_marker_codec_error(name));
                }

                if as_bool(get(&params, "create_dirs"), true) {
                    if let Some(parent) = path.parent() {
                        if !parent.as_os_str().is_empty() {
                            mkdir_parents(parent)?;
                        }
                    }
                }
                let mut open_options = std::fs::OpenOptions::new();
                open_options.write(true).create(true);
                if mode == "append" {
                    open_options.append(true);
                } else {
                    open_options.truncate(true);
                }
                let mut file = open_options
                    .open(path)
                    .map_err(|err| os_error(&err, &display))?;

                let codec = match encoding_outcome {
                    EncodingOutcome::Supported(codec) => codec,
                    EncodingOutcome::Unknown(name) => return Err(unknown_encoding(&name)),
                    EncodingOutcome::NotImplemented(_) => unreachable!("returned above"),
                };
                let encoded = encode_text(codec, &content)?;
                {
                    use std::io::Write;
                    file.write_all(&encoded)
                        .map_err(|err| os_error(&err, &display))?;
                }
                raw.insert(
                    Value::Str("bytes_written".to_string()),
                    Value::from(encoded.len() as i64),
                );
                Value::Str(display.clone())
            }
            "list" => {
                if !py_is_dir(path, &display)? {
                    return Err(ToolError {
                        py_class: PyExc::NotADirectoryError,
                        message: format!("fs: not a directory: {display}"),
                        cause: crate::ErrorCause::None,
                    });
                }
                let entries = std::fs::read_dir(path).map_err(|err| os_error(&err, &display))?;
                let mut names = Vec::new();
                for entry in entries {
                    let entry = entry.map_err(|err| os_error(&err, &display))?;
                    names.push(entry.file_name().to_string_lossy().into_owned());
                }
                names.sort();
                raw.insert(
                    Value::Str("count".to_string()),
                    Value::from(names.len() as i64),
                );
                Value::List(names.into_iter().map(Value::Str).collect())
            }
            "stat" => {
                if !py_exists(path, &display)? {
                    return Err(ToolError {
                        py_class: PyExc::FileNotFoundError,
                        message: format!("fs: path does not exist: {display}"),
                        cause: crate::ErrorCause::None,
                    });
                }
                let metadata = std::fs::metadata(path).map_err(|err| os_error(&err, &display))?;
                stat_dict(path, &display, &metadata)?
            }
            "exists" => Value::from(py_exists(path, &display)?),
            "delete" => {
                if !py_exists(path, &display)? {
                    Value::from(false)
                } else if py_is_dir(path, &display)? {
                    if !as_bool(get(&params, "recursive"), false) {
                        return Err(ToolError {
                            py_class: PyExc::IsADirectoryError,
                            message: format!(
                                "fs: refusing to delete directory without recursive=True: {display}"
                            ),
                            cause: crate::ErrorCause::None,
                        });
                    }
                    let is_symlink = std::fs::symlink_metadata(path)
                        .map(|m| m.file_type().is_symlink())
                        .unwrap_or(false);
                    if is_symlink {
                        // `shutil.rmtree`'s own exact text for this
                        // case (`Lib/shutil.py::rmtree`): no path
                        // embedded.
                        return Err(ToolError {
                            py_class: PyExc::OSError,
                            message: "Cannot call rmtree on a symbolic link".to_string(),
                            cause: crate::ErrorCause::None,
                        });
                    }
                    std::fs::remove_dir_all(path).map_err(|err| os_error(&err, &display))?;
                    Value::from(true)
                } else {
                    std::fs::remove_file(path).map_err(|err| os_error(&err, &display))?;
                    Value::from(true)
                }
            }
            other => {
                return Err(value_error(format!(
                    "fs: unknown mode {}",
                    Value::Str(other.to_string()).py_repr()
                )));
            }
        };

        Ok(ToolResult::new(value, Value::Dict(raw)))
    }

    fn check(&self) -> CheckResult {
        CheckResult {
            ok: true,
            missing: Vec::new(),
            message: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::CancellationToken;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A scratch directory under the process's own temp dir, never the
    /// real `$HOME` -- removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("electricity-fs-test-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn dict(pairs: Vec<(&str, Value)>) -> Value {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(d)
    }

    async fn run(params: Value) -> Result<ToolResult, ToolError> {
        let token = CancellationToken::new();
        let call = ToolCall {
            timeout_seconds: 30,
            config: &Value::None,
            token: &token,
            armed: false,
        };
        FsPlugin.execute(params, &call).await
    }

    #[tokio::test]
    async fn write_then_read_round_trips_and_translates_crlf() {
        let dir = TempDir::new();
        let path = dir.0.join("greeting.txt");
        let path_str = path.to_str().unwrap().to_string();
        run(dict(vec![
            ("mode", Value::from("write")),
            ("path", Value::from(path_str.as_str())),
            ("content", Value::from("hi\r\nthere\r")),
        ]))
        .await
        .unwrap();
        let result = run(dict(vec![
            ("mode", Value::from("read")),
            ("path", Value::from(path_str.as_str())),
        ]))
        .await
        .unwrap();
        assert_eq!(result.value, Value::from("hi\nthere\n"));
    }

    #[tokio::test]
    async fn read_of_a_missing_file_gives_the_exact_text() {
        let dir = TempDir::new();
        let path = dir.0.join("missing.txt");
        let path_str = path.to_str().unwrap().to_string();
        let err = run(dict(vec![
            ("mode", Value::from("read")),
            ("path", Value::from(path_str.as_str())),
        ]))
        .await
        .unwrap_err();
        assert_eq!(err.message, format!("fs: not a file: {path_str}"));
        assert_eq!(err.py_class, PyExc::FileNotFoundError);
    }

    #[tokio::test]
    async fn list_mode_sorts_names_by_code_point() {
        let dir = TempDir::new();
        for name in ["b.txt", "a.txt", "c.txt"] {
            std::fs::write(dir.0.join(name), b"").unwrap();
        }
        let path_str = dir.0.to_str().unwrap().to_string();
        let result = run(dict(vec![
            ("mode", Value::from("list")),
            ("path", Value::from(path_str.as_str())),
        ]))
        .await
        .unwrap();
        assert_eq!(
            result.value,
            Value::List(vec![
                Value::from("a.txt"),
                Value::from("b.txt"),
                Value::from("c.txt"),
            ])
        );
    }

    #[tokio::test]
    async fn list_of_a_file_is_not_a_directory() {
        let dir = TempDir::new();
        let path = dir.0.join("f.txt");
        std::fs::write(&path, b"").unwrap();
        let path_str = path.to_str().unwrap().to_string();
        let err = run(dict(vec![
            ("mode", Value::from("list")),
            ("path", Value::from(path_str.as_str())),
        ]))
        .await
        .unwrap_err();
        assert_eq!(err.message, format!("fs: not a directory: {path_str}"));
        assert_eq!(err.py_class, PyExc::NotADirectoryError);
    }

    #[tokio::test]
    async fn delete_of_a_missing_path_is_false_not_an_error() {
        let dir = TempDir::new();
        let path = dir.0.join("missing");
        let path_str = path.to_str().unwrap().to_string();
        let result = run(dict(vec![
            ("mode", Value::from("delete")),
            ("path", Value::from(path_str.as_str())),
        ]))
        .await
        .unwrap();
        assert_eq!(result.value, Value::from(false));
    }

    #[tokio::test]
    async fn delete_of_a_directory_without_recursive_raises() {
        let dir = TempDir::new();
        let sub = dir.0.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let path_str = sub.to_str().unwrap().to_string();
        let err = run(dict(vec![
            ("mode", Value::from("delete")),
            ("path", Value::from(path_str.as_str())),
        ]))
        .await
        .unwrap_err();
        assert_eq!(
            err.message,
            format!("fs: refusing to delete directory without recursive=True: {path_str}")
        );
        assert_eq!(err.py_class, PyExc::IsADirectoryError);
    }

    #[tokio::test]
    async fn delete_of_a_directory_with_recursive_succeeds() {
        let dir = TempDir::new();
        let sub = dir.0.join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("f.txt"), b"x").unwrap();
        let path_str = sub.to_str().unwrap().to_string();
        let result = run(dict(vec![
            ("mode", Value::from("delete")),
            ("path", Value::from(path_str.as_str())),
            ("recursive", Value::from(true)),
        ]))
        .await
        .unwrap();
        assert_eq!(result.value, Value::from(true));
        assert!(!sub.exists());
    }

    #[tokio::test]
    async fn an_explicit_null_mode_is_not_the_default() {
        let dir = TempDir::new();
        let path_str = dir.0.to_str().unwrap().to_string();
        let err = run(dict(vec![
            ("mode", Value::None),
            ("path", Value::from(path_str.as_str())),
        ]))
        .await
        .unwrap_err();
        assert_eq!(err.message, "fs: unknown mode 'none'");
    }

    #[tokio::test]
    async fn path_must_be_a_non_empty_string() {
        let err = run(dict(vec![("mode", Value::from("exists"))]))
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "fs: params['path'] must be a non-empty string."
        );
    }

    #[tokio::test]
    async fn path_rejects_a_null_byte() {
        let err = run(dict(vec![
            ("mode", Value::from("exists")),
            ("path", Value::from("a\0b")),
        ]))
        .await
        .unwrap_err();
        assert_eq!(err.message, "fs: path contains null byte.");
    }

    #[tokio::test]
    async fn write_requires_str_content() {
        let dir = TempDir::new();
        let path = dir.0.join("f.txt");
        let path_str = path.to_str().unwrap().to_string();
        let err = run(dict(vec![
            ("mode", Value::from("write")),
            ("path", Value::from(path_str.as_str())),
            ("content", Value::from(1i64)),
        ]))
        .await
        .unwrap_err();
        assert_eq!(
            err.message,
            "fs: write/append requires params['content'] as str."
        );
    }

    #[tokio::test]
    async fn append_mode_appends_and_counts_bytes_written() {
        let dir = TempDir::new();
        let path = dir.0.join("log.txt");
        let path_str = path.to_str().unwrap().to_string();
        run(dict(vec![
            ("mode", Value::from("write")),
            ("path", Value::from(path_str.as_str())),
            ("content", Value::from("a")),
        ]))
        .await
        .unwrap();
        let result = run(dict(vec![
            ("mode", Value::from("append")),
            ("path", Value::from(path_str.as_str())),
            ("content", Value::from("bb")),
        ]))
        .await
        .unwrap();
        assert_eq!(
            result
                .raw
                .as_dict()
                .unwrap()
                .get(&Value::Str("bytes_written".to_string())),
            Some(&Value::from(2i64))
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "abb");
    }

    #[tokio::test]
    async fn exists_follows_symlinks_and_a_dangling_link_is_false() {
        let dir = TempDir::new();
        let target = dir.0.join("target");
        let link = dir.0.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let path_str = link.to_str().unwrap().to_string();
        let result = run(dict(vec![
            ("mode", Value::from("exists")),
            ("path", Value::from(path_str.as_str())),
        ]))
        .await
        .unwrap();
        assert_eq!(result.value, Value::from(false));
    }

    #[tokio::test]
    async fn delete_refuses_a_symlink_to_a_directory_even_with_recursive() {
        let dir = TempDir::new();
        let target = dir.0.join("target");
        std::fs::create_dir(&target).unwrap();
        let link = dir.0.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let path_str = link.to_str().unwrap().to_string();
        let err = run(dict(vec![
            ("mode", Value::from("delete")),
            ("path", Value::from(path_str.as_str())),
            ("recursive", Value::from(true)),
        ]))
        .await
        .unwrap_err();
        assert_eq!(err.py_class, PyExc::OSError);
        assert!(target.exists(), "the real directory must survive");
    }

    #[test]
    fn resolve_codec_accepts_common_aliases() {
        assert_eq!(resolve_codec("UTF-8"), Some(Codec::Utf8));
        assert_eq!(resolve_codec("utf8"), Some(Codec::Utf8));
        assert_eq!(resolve_codec("Latin-1"), Some(Codec::Latin1));
        assert_eq!(resolve_codec("latin1"), Some(Codec::Latin1));
        assert_eq!(resolve_codec("ASCII"), Some(Codec::Ascii));
        assert_eq!(resolve_codec("us-ascii"), Some(Codec::Ascii));
        assert_eq!(resolve_codec("bogus-codec"), None);
    }

    #[test]
    fn ascii_decode_reports_cpythons_own_text() {
        let err = decode_text(Codec::Ascii, &[0x61, 0x80]).unwrap_err();
        assert_eq!(
            err.message,
            "'ascii' codec can't decode byte 0x80 in position 1: ordinal not in range(128)"
        );
        assert_eq!(err.py_class, PyExc::UnicodeDecodeError);
    }

    #[test]
    fn ascii_encode_reports_cpythons_own_text() {
        let err = encode_text(Codec::Ascii, "a\u{e9}").unwrap_err();
        assert_eq!(
            err.message,
            "'ascii' codec can't encode character '\\xe9' in position 1: ordinal not in range(128)"
        );
        assert_eq!(err.py_class, PyExc::UnicodeEncodeError);
    }

    #[test]
    fn latin1_encode_rejects_beyond_0xff() {
        let err = encode_text(Codec::Latin1, "\u{1f600}").unwrap_err();
        assert_eq!(
            err.message,
            "'latin-1' codec can't encode character '\\U0001f600' in position 0: ordinal not in range(256)"
        );
    }

    #[test]
    fn utf8_decode_reports_cpythons_own_text() {
        let err = decode_text(Codec::Utf8, &[0xff]).unwrap_err();
        assert_eq!(
            err.message,
            "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte"
        );
    }

    #[test]
    fn expand_user_path_substitutes_home_for_a_bare_tilde() {
        // SAFETY: this test's own process, not the suite -- restored
        // immediately after, never touching the real `$HOME` the rest
        // of the suite (or another thread) might read concurrently in
        // a way this test would otherwise race.
        let dir = TempDir::new();
        let previous = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &dir.0);
        }
        let resolved = expand_user_path("~/x.txt");
        match previous {
            Some(home) => unsafe { std::env::set_var("HOME", home) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        assert_eq!(
            resolved.display,
            format!("{}/x.txt", dir.0.to_str().unwrap())
        );
    }

    #[test]
    fn normalize_path_drops_dot_segments_and_trailing_slash() {
        let resolved = expand_user_path("foo/./bar/");
        assert_eq!(resolved.display, "foo/bar");
    }

    #[test]
    fn ascii_encode_reports_the_whole_run_of_unencodable_characters() {
        // CPython's own encoder invokes the error handler once per
        // *run* of consecutive unencodable characters, not once per
        // character (`"\u65e5\u672c".encode("ascii")`'s own text) --
        // issue #460's own fix-pass finding 3.
        let err = encode_text(Codec::Ascii, "\u{65e5}\u{672c}").unwrap_err();
        assert_eq!(
            err.message,
            "'ascii' codec can't encode characters in position 0-1: ordinal not in range(128)"
        );
    }

    #[test]
    fn latin1_encode_reports_the_whole_run_of_unencodable_characters() {
        let err = encode_text(Codec::Latin1, "a\u{65e5}\u{672c}b").unwrap_err();
        assert_eq!(
            err.message,
            "'latin-1' codec can't encode characters in position 1-2: ordinal not in range(256)"
        );
    }

    #[tokio::test]
    async fn mkdir_dash_p_through_a_file_names_the_directory_that_actually_failed() {
        // `<root>/file.txt` is a regular file; `mkdir -p
        // <root>/file.txt/sub` can never succeed, and CPython's own
        // `Path.mkdir(parents=True, exist_ok=True)` names *that*
        // directory in its error, not the write's own leaf path --
        // issue #460's own fix-pass finding 7.
        let dir = TempDir::new();
        let blocker = dir.0.join("file.txt");
        std::fs::write(&blocker, b"").unwrap();
        let leaf = blocker.join("sub").join("x.txt");
        let leaf_str = leaf.to_str().unwrap().to_string();
        let err = run(dict(vec![
            ("mode", Value::from("write")),
            ("path", Value::from(leaf_str.as_str())),
            ("content", Value::from("x")),
        ]))
        .await
        .unwrap_err();
        let failing_dir = blocker.join("sub");
        assert!(
            err.message.contains(failing_dir.to_str().unwrap()),
            "expected the failing directory {:?} in {:?}",
            failing_dir,
            err.message
        );
        assert!(
            !err.message.contains(leaf_str.as_str()),
            "expected the write's own leaf path NOT in {:?}",
            err.message
        );
    }

    #[tokio::test]
    async fn a_write_that_fails_to_encode_leaves_the_file_truncated_like_cpython() {
        // CPython's own `open(path, "w", encoding=...)` already
        // creates/truncates the file before `fh.write(content)` can
        // raise `UnicodeEncodeError` -- issue #460's own fix-pass
        // finding 6: a prior write's content must not survive a later
        // failed one.
        let dir = TempDir::new();
        let path = dir.0.join("f.txt");
        let path_str = path.to_str().unwrap().to_string();
        run(dict(vec![
            ("mode", Value::from("write")),
            ("path", Value::from(path_str.as_str())),
            ("content", Value::from("old")),
        ]))
        .await
        .unwrap();
        let err = run(dict(vec![
            ("mode", Value::from("write")),
            ("path", Value::from(path_str.as_str())),
            ("content", Value::from("caf\u{e9}")),
            ("encoding", Value::from("ascii")),
        ]))
        .await
        .unwrap_err();
        assert_eq!(err.py_class, PyExc::UnicodeEncodeError);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    #[tokio::test]
    async fn a_write_with_an_unknown_codec_also_leaves_the_file_truncated() {
        let dir = TempDir::new();
        let path = dir.0.join("f.txt");
        let path_str = path.to_str().unwrap().to_string();
        run(dict(vec![
            ("mode", Value::from("write")),
            ("path", Value::from(path_str.as_str())),
            ("content", Value::from("old")),
        ]))
        .await
        .unwrap();
        let err = run(dict(vec![
            ("mode", Value::from("write")),
            ("path", Value::from(path_str.as_str())),
            ("content", Value::from("x")),
            ("encoding", Value::from("bogus-codec")),
        ]))
        .await
        .unwrap_err();
        assert_eq!(err.py_class, PyExc::LookupError);
        assert_eq!(err.message, "unknown encoding: bogus-codec");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    #[tokio::test]
    async fn a_write_with_a_real_but_unimplemented_codec_touches_nothing() {
        // "utf-16" is a real Python codec (`cof run` would succeed);
        // this preview doesn't implement it, and refuses up front --
        // R2's own ruling: no file created at all, unlike the
        // `bogus-codec`/`UnicodeEncodeError` cases above.
        let dir = TempDir::new();
        let path = dir.0.join("never-created.txt");
        let path_str = path.to_str().unwrap().to_string();
        let err = run(dict(vec![
            ("mode", Value::from("write")),
            ("path", Value::from(path_str.as_str())),
            ("content", Value::from("x")),
            ("encoding", Value::from("utf-16")),
        ]))
        .await
        .unwrap_err();
        assert_eq!(err.py_class, PyExc::LookupError);
        assert!(
            err.message
                .contains("is a preview and can't run this orchestration")
        );
        assert!(err.message.contains("'utf-16'"));
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn a_read_with_a_real_but_unimplemented_codec_gets_the_preview_marker() {
        let dir = TempDir::new();
        let path = dir.0.join("f.txt");
        std::fs::write(&path, b"hi").unwrap();
        let path_str = path.to_str().unwrap().to_string();
        let err = run(dict(vec![
            ("mode", Value::from("read")),
            ("path", Value::from(path_str.as_str())),
            ("encoding", Value::from("shift_jis")),
        ]))
        .await
        .unwrap_err();
        assert_eq!(err.py_class, PyExc::LookupError);
        assert!(
            err.message
                .contains("is a preview and can't run this orchestration")
        );
    }

    #[test]
    fn classify_encoding_tells_apart_unknown_from_not_yet_implemented() {
        assert!(matches!(
            classify_encoding("utf-8"),
            EncodingOutcome::Supported(Codec::Utf8)
        ));
        assert!(matches!(
            classify_encoding("utf-16"),
            EncodingOutcome::NotImplemented(_)
        ));
        assert!(matches!(
            classify_encoding("cp1252"),
            EncodingOutcome::NotImplemented(_)
        ));
        assert!(matches!(
            classify_encoding("bogus-codec-xyz"),
            EncodingOutcome::Unknown(_)
        ));
    }

    #[test]
    fn py_mtime_truncates_toward_zero_not_floor() {
        // A pre-1970 mtime 10.3s before the epoch: the kernel's own
        // `tv_sec`/`tv_nsec` pair represents it as `tv_sec=-11,
        // tv_nsec=700_000_000` (never a negative `tv_nsec`); CPython's
        // `int(st_mtime)` truncates the *true* float value (-10.3)
        // toward zero, i.e. -10, not -11 -- issue #460's own fix-pass
        // finding 9. Checked against the pure function directly (not
        // a real file's own mtime): a CI sandbox's filesystem isn't
        // guaranteed to keep the exact sub-second value a `set_times`
        // call requests.
        assert_eq!(truncate_toward_zero(-11, 700_000_000), -10);
        assert_eq!(truncate_toward_zero(-10, 0), -10);
        assert_eq!(truncate_toward_zero(5, 0), 5);
        assert_eq!(truncate_toward_zero(5, 500_000_000), 5);
    }
}
