//! Lane J1 (issue #488): reads and schema-checks a profile file --
//! `cli/profiles.py::parse_profile_document`/`load_profile`'s own file
//! I/O, YAML, and schema half. The pure checks ([`ProfileSettings`], the
//! effect-path walk, the routing-pin check) live in
//! `electricity_config::profile`, which this module calls directly:
//! `electricity-compiler` already depends on `electricity-config`, and
//! only this crate reaches `schema_instance`'s `Value` ->
//! schema-instance conversion the schema check needs.
//!
//! `--profile <path>` has no CLI surface yet (lane R); nothing in
//! electricity's run wiring calls [`load_profile`] today, so this lane
//! changes no run behaviour.
//!
//! # Loading order (`parse_profile_document`, then `load_profile`)
//!
//! 1. **Locating the file** (electricity's own text, DESIGN.md §6.11):
//!    not an existing regular file -> [`ProfileError`].
//! 2. **Reading**, `path.read_text(encoding="utf-8")`: strict UTF-8 (an
//!    invalid byte gives CPython's own `UnicodeDecodeError` text,
//!    unwrapped -- [`describe_utf8_error`], ported locally rather than
//!    reusing `prompt_files.rs`'s private copy, a different lane's
//!    file); universal newlines (`\r\n`/a lone `\r` -> `\n`); any other
//!    I/O failure gets electricity's own wrapped text.
//! 3. **YAML**, `electricity_yaml::load_yaml_last_key_wins` (plain
//!    `yaml.safe_load` semantics already -- no further loader needed):
//!    a YAML error is third-party text, unwrapped.
//! 4. **`... or {}`**: any falsy result becomes `{}`; a truthy
//!    non-mapping root fails with electricity's own text.
//! 5. **Schema**, Draft 7, via `schema_instance::to_schema_instance` +
//!    `electricity_schema::profile_errors`.
//!
//! [`load_profile`] continues: 6. effect-path validation
//! (`electricity_config::profile::validate_effect_paths`, over the raw
//! orchestration *document* this crate's caller already loaded); 7.
//! [`electricity_config::profile::settings_from_raw`].

use crate::schema_instance::to_schema_instance;
use electricity_config::{ProfileError, ProfileSettings, profile_name_from_path};
use electricity_value::{Dict, Value};
use std::path::Path;

/// `{name!r}`.
fn name_repr(name: &str) -> String {
    Value::Str(name.to_string()).py_repr()
}

/// Python's `str(PurePosixPath(given))`: collapses repeated `/`, drops
/// `.` segments, strips a trailing `/`, keeps `..` unresolved. Ported
/// locally (not reused from `load.rs`'s own private copy, a different
/// lane's file) for every `{path}` in this module's own messages.
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

/// `\r\n` and a lone `\r` both become `\n` -- `read_text`'s own
/// universal-newline translation, applied before the YAML parse.
/// Ported locally for the same reason [`pathlib_str`] is.
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

/// Python truthiness (`load_yaml_last_key_wins(text) or {}`).
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

/// `UnicodeDecodeError.__str__` -- a local port of
/// `prompt_files.rs`'s own `describe_utf8_error`/`find_utf8_error`/
/// `utf8_lead_shape` (a different lane's file, so duplicated here
/// rather than exposed from there): `'utf-8' codec can't decode byte
/// 0xXX in position N: <reason>` for a single bad byte, or `can't
/// decode bytes in position N-M: <reason>` when one or more otherwise-
/// valid continuation bytes were already read before the failure.
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

/// `cli/profiles.py::parse_profile_document`: reads and schema-checks
/// *path* without resolving effect paths -- *name* is the already-
/// derived profile name (callers typically have it from
/// [`profile_name_from_path`]), used only for this function's own
/// messages.
pub fn parse_profile_document(path: &Path, name: &str) -> Result<Value, ProfileError> {
    if !path.is_file() {
        return Err(ProfileError(format!(
            "Profile {} not found: {} does not exist or is not a file.",
            name_repr(name),
            pathlib_str(path)
        )));
    }

    let bytes = std::fs::read(path).map_err(|err| {
        ProfileError(format!(
            "Profile {} at {} could not be read: {err}",
            name_repr(name),
            pathlib_str(path)
        ))
    })?;
    let text =
        std::str::from_utf8(&bytes).map_err(|_| ProfileError(describe_utf8_error(&bytes)))?;
    let text = translate_universal_newlines(text);

    let value = electricity_yaml::load_yaml_last_key_wins(&text)
        .map_err(|err| ProfileError(err.to_string()))?;
    let value = if is_truthy(&value) {
        value
    } else {
        Value::Dict(Dict::new())
    };
    let Value::Dict(_) = &value else {
        return Err(ProfileError(format!(
            "Profile {} at {} must be a mapping/object at the root.",
            name_repr(name),
            pathlib_str(path)
        )));
    };

    let instance = to_schema_instance(&value);
    let mut errors = electricity_schema::profile_errors(&instance);
    if !errors.is_empty() {
        // Python sorts by `str(err)` (third-party `jsonschema` text) --
        // not reproducible here (DESIGN.md §1, the M1-J1 ruling); sort
        // by `(location, message)` instead, the closest Rust analogue,
        // and treat a multi-error failure as a set of lines plus the
        // exact header when comparing (never by this order itself).
        errors.sort_by(|a, b| (&a.location, &a.message).cmp(&(&b.location, &b.message)));
        let lines: Vec<String> = errors
            .iter()
            .map(|e| format!("  - {}", e.describe()))
            .collect();
        return Err(ProfileError(format!(
            "Profile {} at {} failed schema validation:\n{}",
            name_repr(name),
            pathlib_str(path),
            lines.join("\n")
        )));
    }

    Ok(value)
}

/// `cli/profiles.py::load_profile`, narrowed to electricity's own
/// explicit-path discovery (DESIGN.md §1): discovery itself
/// (`discover_profile_path`'s name-based search) is out of scope for
/// this lane and this milestone -- *path* is always given directly.
/// *document* is the raw orchestration document the run is checking
/// the profile's `effects` keys against.
pub fn load_profile(path: &Path, document: &Value) -> Result<ProfileSettings, ProfileError> {
    let name = profile_name_from_path(path);
    let raw = parse_profile_document(path, &name)?;

    let effects_raw = raw
        .as_dict()
        .and_then(|d| d.get(&Value::Str("effects".to_string())))
        .cloned()
        .unwrap_or(Value::Dict(Dict::new()));
    electricity_config::validate_effect_paths(&effects_raw, document, &name)?;

    Ok(electricity_config::settings_from_raw(raw, &name, path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::ops::Deref;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "electricity-profile-test-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

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

    #[test]
    fn missing_file_reports_not_found() {
        let dir = TempDir::new("missing");
        let path = dir.join("nope.yml");
        let err = parse_profile_document(&path, "nope").unwrap_err();
        assert!(err.0.starts_with("Profile 'nope' not found: "));
        assert!(err.0.ends_with("does not exist or is not a file."));
    }

    #[test]
    fn a_directory_counts_as_not_found() {
        let dir = TempDir::new("dir-as-profile");
        let err = parse_profile_document(&dir, "d").unwrap_err();
        assert!(err.0.contains("not found"));
    }

    #[test]
    fn invalid_utf8_reports_cpythons_exact_text_unwrapped() {
        let dir = TempDir::new("badutf8");
        let path = dir.join("bad.yml");
        fs::write(&path, [0xFFu8]).unwrap();
        let err = parse_profile_document(&path, "bad").unwrap_err();
        assert_eq!(
            err.0,
            "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte"
        );
    }

    #[test]
    fn an_empty_file_loads_as_an_empty_mapping() {
        let dir = TempDir::new("empty");
        let path = dir.join("empty.yml");
        fs::write(&path, "").unwrap();
        let value = parse_profile_document(&path, "empty").unwrap();
        assert_eq!(value, Value::Dict(Dict::new()));
    }

    #[test]
    fn a_non_mapping_root_is_rejected() {
        let dir = TempDir::new("list-root");
        let path = dir.join("list.yml");
        fs::write(&path, "- 1\n- 2\n").unwrap();
        let err = parse_profile_document(&path, "list").unwrap_err();
        assert_eq!(
            err.0,
            format!(
                "Profile 'list' at {} must be a mapping/object at the root.",
                pathlib_str(&path)
            )
        );
    }

    #[test]
    fn a_schema_violation_reports_the_header_and_a_located_line() {
        let dir = TempDir::new("schema-bad");
        let path = dir.join("bad.yml");
        fs::write(&path, "adapter: 5\n").unwrap();
        let err = parse_profile_document(&path, "bad").unwrap_err();
        assert!(
            err.0.starts_with(&format!(
                "Profile 'bad' at {} failed schema validation:\n  - adapter: ",
                pathlib_str(&path)
            )),
            "{}",
            err.0
        );
    }

    #[test]
    fn duplicate_keys_keep_the_last_value() {
        let dir = TempDir::new("duplicate");
        let path = dir.join("dup.yml");
        fs::write(&path, "model: a\nmodel: b\n").unwrap();
        let value = parse_profile_document(&path, "dup").unwrap();
        assert_eq!(
            value.as_dict().unwrap().get(&Value::Str("model".into())),
            Some(&Value::Str("b".into()))
        );
    }

    #[test]
    fn load_profile_rejects_an_unknown_effect_path() {
        let dir = TempDir::new("unknown-path");
        let path = dir.join("p.yml");
        fs::write(&path, "effects:\n  bogus:\n    enabled: false\n").unwrap();
        let document = Value::Dict(Dict::new());
        let err = load_profile(&path, &document).unwrap_err();
        assert!(err.0.contains("references unknown effect path(s): bogus"));
    }

    #[test]
    fn load_profile_succeeds_end_to_end() {
        let dir = TempDir::new("ok");
        let path = dir.join("p.yml");
        fs::write(&path, "adapter: openai\nmodel: gpt\ninputs:\n  x: 1\n").unwrap();
        let document = Value::Dict(Dict::new());
        let settings = load_profile(&path, &document).unwrap();
        assert_eq!(settings.name, "p");
        assert_eq!(settings.adapter, Some("openai".to_string()));
        assert_eq!(settings.model, Some("gpt".to_string()));
    }
}
