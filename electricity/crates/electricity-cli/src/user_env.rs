//! Port of `cli/config.py::load_user_env` (issue #468): loads the
//! private `.env` file `cof setup` writes next to the global config
//! (`$HOME/.config/circuitry/.env`) into the process environment,
//! before config resolution. Only `main.rs` calls this (before either
//! `electricity::config_error` or `electricity::dump_ir`) -- the
//! `electricity` library crate never does, matching `circuitry.api`:
//! an embedding program owns its own environment.
//!
//! Rules, ported from the reference exactly:
//! - a variable already set in the real environment always wins (the
//!   file never overrides it);
//! - a file not owned by the current user, or writable by group or
//!   others, is refused outright (one warning naming the `chmod 600`
//!   fix);
//! - a file merely *readable* by group or others is still loaded,
//!   with a softer warning;
//! - an unreadable or undecodable file is skipped, with a warning;
//! - the ownership/mode checks (and so both warnings above) are
//!   skipped entirely on a non-Unix target, where POSIX mode bits
//!   don't mean anything.
//!
//! Parsing itself -- quoting, escapes, `export `, comments, multi-line
//! values, `${VAR}` interpolation -- is [`electricity_config::dotenv`],
//! a faithful port of `python-dotenv`'s own `dotenv_values` (which
//! `load_user_env` calls directly).
//!
//! Every warning here logs through `log::warn!`, reaching stderr via
//! this binary's own `StderrWarnLogger` (installed in `main` before
//! [`load_user_env`] is ever called) -- `cof`'s own line format
//! (`WARNING: <text>`) and position (before config resolution).
//!
//! python-dotenv's own per-line parse warning ("could not parse
//! statement starting at line N") never reaches `cof`'s stderr: it
//! logs through a `dotenv.main`-named logger, which `cli/
//! logging_setup.py::configure_cli_logging` never attaches a handler
//! to (only the `"circuitry"` logger). `electricity_config::dotenv`
//! reproduces that silence by simply dropping a malformed line, same
//! as the reference, with no warning of its own either.

use electricity_config::dotenv::dotenv_values;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// What [`load_user_env`] did with the user's private `.env` --
/// mirrors `cli/config.py::UserEnvResult`. `supplied`/`skipped` carry
/// variable *names* only, never values.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UserEnvResult {
    pub path: PathBuf,
    pub loaded: bool,
    pub supplied: Vec<String>,
    pub skipped: Vec<String>,
    pub warning: Option<String>,
}

/// `$HOME/.config/circuitry/.env` -- `cli/config.py::GLOBAL_CONFIG_DIR`
/// (`Path.home() / ".config" / "circuitry"`) joined with
/// `_user_env_path`'s own `.env` filename. `None` when neither `HOME`
/// nor a `/etc/passwd` entry for the current uid gives one.
/// `std::env::home_dir()` is `Path.home()`'s own fallback order on
/// Unix: `HOME` if set and non-empty, else a `getpwuid_r` lookup --
/// matching `os.path.expanduser("~")` (what `Path.home()` calls), not
/// just this one environment variable (#471 review finding 7: under
/// `env -i`, with `HOME` unset entirely, cof's own fallback still
/// finds a home directory; electricity's used to return `None` there).
pub fn user_env_path() -> Option<PathBuf> {
    let home = std::env::home_dir()?;
    Some(home.join(".config").join("circuitry").join(".env"))
}

/// Python's `oct(mode)` -- `0o`, then the permission bits in octal, no
/// leading zero-padding.
fn mode_octal(mode: u32) -> String {
    format!("0o{mode:o}")
}

/// Python `repr(s)` of a plain string -- the invalid-entry warning's
/// own `{key!r}` f-string interpolation (#473), quoted with Python's
/// own rules rather than Rust's `{key:?}` Debug formatting.
fn py_repr_str(s: &str) -> String {
    electricity_value::Value::Str(s.to_string()).py_repr()
}

/// `cli/config.py::load_user_env`'s own f-string, verbatim: `{path}:
/// skipping {key!r}: not a valid environment variable name or
/// value.` -- factored out from [`load_user_env_at`]'s loop so a unit
/// test can check the exact text without needing a real `.env` entry
/// `dotenv_values` would actually produce (an empty key, for one,
/// never reaches here from a real file either side's parser can
/// produce -- this check stays defensive, matching the reference).
fn invalid_entry_warning(path: &Path, key: &str) -> String {
    format!(
        "{}: skipping {}: not a valid environment variable name or value.",
        path.display(),
        py_repr_str(key),
    )
}

/// POSIX ownership/mode bits are meaningless on a non-Unix target
/// (there is no `getuid`, and a file's reported mode there doesn't
/// reflect a real ACL) -- `cli/config.py::load_user_env`'s own
/// `hasattr(os, "getuid")` branch. electricity has no Windows support
/// at all today (DESIGN.md's M1 decisions), but this split is kept the
/// same shape as the reference's own for any other non-Unix target a
/// future cross-compile might add.
/// The pure part of [`permission_refusal`]'s check -- *mode* already
/// masked to the low 12 bits, *file_uid*/*current_uid* already read
/// (by [`permission_refusal`]'s own `stat`/`getuid`, or a test's
/// literal values) -- so a unit test can exercise an owner mismatch
/// without ever needing a file this process doesn't own (#471 review
/// finding 3: `getuid()` called *inside* the old single function left
/// no seam for that).
#[cfg(unix)]
fn unsafe_reason(mode: u32, file_uid: u32, current_uid: u32, path: &Path) -> Option<String> {
    let owned_by_user = file_uid == current_uid;
    let group_or_other_writable = mode & 0o022 != 0;
    if !owned_by_user || group_or_other_writable {
        Some(format!(
            "{} is not safe to load (owner mismatch or group/world-writable, mode {}); \
             refusing to load it. Run 'chmod 600 {}' and check its ownership.",
            path.display(),
            mode_octal(mode),
            path.display(),
        ))
    } else {
        None
    }
}

#[cfg(unix)]
fn permission_refusal(metadata: &std::fs::Metadata, path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let mode = metadata.mode() & 0o7777;
    // SAFETY: `getuid` has no preconditions and never fails.
    let current_uid = unsafe { libc::getuid() };
    unsafe_reason(mode, metadata.uid(), current_uid, path)
}

#[cfg(unix)]
fn readable_by_others_warning(metadata: &std::fs::Metadata, path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let mode = metadata.mode() & 0o7777;
    if mode & 0o077 != 0 {
        Some(format!(
            "{} is mode {} (group/world-readable); run 'chmod 600 {}'.",
            path.display(),
            mode_octal(mode),
            path.display(),
        ))
    } else {
        None
    }
}

#[cfg(not(unix))]
fn permission_refusal(_metadata: &std::fs::Metadata, _path: &Path) -> Option<String> {
    None
}

#[cfg(not(unix))]
fn readable_by_others_warning(_metadata: &std::fs::Metadata, _path: &Path) -> Option<String> {
    None
}

/// Python's `exc.__class__.__name__` for the two realistic ways
/// reading an already-stat'd file can fail here: permission denied
/// (a file the owner themself can't read -- mode `0o000`, say) and
/// everything else, which keeps the reference's own base-class name
/// (`OSError`) since no other subclass is reachable from a file that
/// already passed [`permission_refusal`]'s own stat-based check.
fn os_error_class_name(err: &std::io::Error) -> &'static str {
    match err.kind() {
        std::io::ErrorKind::PermissionDenied => "PermissionError",
        _ => "OSError",
    }
}

/// Loads the file at [`user_env_path`] into the real process
/// environment -- this module's own doc comment has the full rule
/// set. A no-op (`loaded: false`, no warning) when `HOME` isn't set or
/// the file doesn't exist; this is also where every `log::warn!` this
/// module ever calls fires, each at the exact point `cli/config.py::
/// load_user_env`'s own `logger.warning` would.
pub fn load_user_env() -> UserEnvResult {
    match user_env_path() {
        Some(path) => load_user_env_at(&path),
        None => UserEnvResult::default(),
    }
}

fn load_user_env_at(path: &Path) -> UserEnvResult {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            return UserEnvResult {
                path: path.to_path_buf(),
                ..Default::default()
            };
        }
    };

    if let Some(warning) = permission_refusal(&metadata, path) {
        log::warn!("{warning}");
        return UserEnvResult {
            path: path.to_path_buf(),
            loaded: false,
            warning: Some(warning),
            ..Default::default()
        };
    }

    let readable_warning = readable_by_others_warning(&metadata, path);
    if let Some(warning) = &readable_warning {
        log::warn!("{warning}");
    }

    // `DotEnv._get_stream`'s own `_is_file_or_fifo` check: neither a
    // directory nor (on this platform) a FIFO at *path* is ever opened
    // for reading at all -- `_get_stream` falls through to
    // `io.StringIO("")` instead, so a directory there parses as an
    // empty file (`loaded: true`, nothing supplied) rather than the
    // `OSError` a real `fs::read` on a directory raises (#471 review
    // finding 6: that would print its own spurious "could not be
    // read" warning cof never does).
    let text = if metadata.is_dir() {
        String::new()
    } else {
        match std::fs::read(path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(_) => {
                    let warning = format!(
                        "{} could not be read (UnicodeDecodeError); skipping it.",
                        path.display()
                    );
                    log::warn!("{warning}");
                    return UserEnvResult {
                        path: path.to_path_buf(),
                        loaded: false,
                        warning: Some(warning),
                        ..Default::default()
                    };
                }
            },
            Err(err) => {
                let warning = format!(
                    "{} could not be read ({}); skipping it.",
                    path.display(),
                    os_error_class_name(&err),
                );
                log::warn!("{warning}");
                return UserEnvResult {
                    path: path.to_path_buf(),
                    loaded: false,
                    warning: Some(warning),
                    ..Default::default()
                };
            }
        }
    };

    // The real environment, as it stands *before* this file can add
    // anything to it -- `dotenv_values`'s own `${VAR}` interpolation
    // reads this snapshot, exactly as `cli/config.py::load_user_env`'s
    // own `dotenv_values(path)` reads the live `os.environ` at the
    // point it runs, before this function has set anything itself.
    //
    // `std::env::vars()`, not `vars_os()`, would panic on a variable
    // whose name or value isn't valid Unicode (issue #468's startup-
    // crash comment, the same reasoning as `electricity::
    // current_env_vars`) -- this crate's own startup path reaches
    // here *before* that one ever runs, so a fix there alone would
    // never even be reached. A name or value that doesn't decode is
    // simply unavailable as a `${VAR}` interpolation fallback (never a
    // crash) -- nothing a real `.env` file would reference by name
    // anyway, since its own keys are always valid Unicode text parsed
    // from the file itself.
    let real_env: HashMap<String, String> = std::env::vars_os()
        .filter_map(|(key, value)| {
            let key = key.into_string().ok()?;
            let value = value.into_string().ok()?;
            Some((key, value))
        })
        .collect();
    let values = dotenv_values(&text, &real_env);

    let mut supplied = Vec::new();
    let mut skipped = Vec::new();
    for (key, value) in values {
        // `std::env::var_os` -- never `real_env.contains_key`, which
        // can only ever see a name/value that decoded as UTF-8 above --
        // a key already present in the real environment must win
        // regardless of whether its *value* happens to be valid
        // Unicode, exactly as Python's own `if key in os.environ` sees
        // every real key, `surrogateescape`-decoded or not.
        if std::env::var_os(&key).is_some() {
            skipped.push(key);
            continue;
        }
        let Some(value) = value else { continue };
        // `std::env::set_var` panics on a key that is empty or
        // contains `=`/NUL, or a value containing NUL -- `cof`'s own
        // `os.environ[key] = value` would instead raise a `ValueError`
        // ("illegal environment variable name" / "embedded null
        // byte"); `cli/config.py::load_user_env` catches neither case
        // by the type, it rejects the entry itself before ever
        // assigning it, with the exact warning built below (#473).
        // Neither side's warning text ever names the *value* -- but
        // `set_var`'s panic message does (`failed to set environment
        // variable {key:?} to {value:?}`), and with no custom panic
        // hook that would reach stderr verbatim (#471 review finding
        // 2: a secret leak). A single-quoted key can contain `=`
        // (`'A=B'=secret`), and a literal NUL byte decodes as valid
        // UTF-8 inside a quoted or unquoted value -- both reachable
        // from a real file, not just a hostile one. Skipped, not
        // propagated as a hard failure: naming only the key keeps
        // every other line in the same file loading, the same
        // robustness this function already gives an unreadable or
        // undecodable file.
        if key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0') {
            let warning = invalid_entry_warning(path, &key);
            log::warn!("{warning}");
            skipped.push(key);
            continue;
        }
        // SAFETY: called once, at the very start of `main`, before the
        // signal-handling thread or the Tokio runtime exist -- the one
        // point in this binary's lifetime where mutating the process
        // environment from a single thread is always sound.
        unsafe { std::env::set_var(&key, &value) };
        supplied.push(key);
    }

    UserEnvResult {
        path: path.to_path_buf(),
        loaded: true,
        supplied,
        skipped,
        warning: readable_warning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `std::env::set_var`/`remove_var` are documented as unsound to
    /// call concurrently with any other access to the process
    /// environment from another thread -- every test below that
    /// mutates it (directly, or indirectly through
    /// [`load_user_env_at`]) takes this lock first, serializing them
    /// against each other within this one test binary (every other
    /// test in this crate, in `main.rs`/`tests/*.rs`, either never
    /// touches the environment or runs the `electricity` binary as a
    /// separate subprocess, so there is nothing else in-process for
    /// this lock to need to cover).
    static ENV_MUTATION_LOCK: Mutex<()> = Mutex::new(());

    /// A fresh scratch directory under the OS temp dir, matching every
    /// other Rust test in this workspace (no `tempfile` dependency).
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "electricity-user-env-test-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            ));
            std::fs::create_dir_all(&dir).unwrap();
            ScratchDir(dir)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_file_is_a_silent_no_op() {
        let dir = ScratchDir::new("missing");
        let path = dir.0.join(".env");
        let result = load_user_env_at(&path);
        assert!(!result.loaded);
        assert_eq!(result.warning, None);
    }

    #[test]
    fn a_file_the_real_environment_already_covers_is_skipped_not_overridden() {
        let _guard = ENV_MUTATION_LOCK.lock().unwrap();
        let dir = ScratchDir::new("precedence");
        let path = dir.0.join(".env");
        std::fs::write(&path, "PATH=should-never-win\nFRESH_VAR_468=supplied\n").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let real_path_before = std::env::var("PATH").unwrap();
        let result = load_user_env_at(&path);
        assert!(result.loaded);
        assert_eq!(std::env::var("PATH").unwrap(), real_path_before);
        assert_eq!(result.skipped, vec!["PATH".to_string()]);
        assert_eq!(std::env::var("FRESH_VAR_468").unwrap(), "supplied");
        assert_eq!(result.supplied, vec!["FRESH_VAR_468".to_string()]);
        unsafe { std::env::remove_var("FRESH_VAR_468") };
    }

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    #[test]
    fn group_or_world_writable_is_refused() {
        let _guard = ENV_MUTATION_LOCK.lock().unwrap();
        let dir = ScratchDir::new("unsafe-mode");
        let path = dir.0.join(".env");
        std::fs::write(&path, "FOO=bar\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o622)).unwrap();

        let result = load_user_env_at(&path);
        assert!(!result.loaded);
        let warning = result.warning.expect("refusal must carry a warning");
        assert!(warning.contains("not safe to load"));
        assert!(warning.contains("chmod 600"));
        assert!(std::env::var("FOO").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn group_only_writable_is_refused() {
        let _guard = ENV_MUTATION_LOCK.lock().unwrap();
        let dir = ScratchDir::new("unsafe-mode-group-only");
        let path = dir.0.join(".env");
        std::fs::write(&path, "FOO=bar\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o620)).unwrap();

        let result = load_user_env_at(&path);
        assert!(!result.loaded);
        assert!(std::env::var("FOO").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn world_only_writable_is_refused() {
        let _guard = ENV_MUTATION_LOCK.lock().unwrap();
        let dir = ScratchDir::new("unsafe-mode-world-only");
        let path = dir.0.join(".env");
        std::fs::write(&path, "FOO=bar\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o602)).unwrap();

        let result = load_user_env_at(&path);
        assert!(!result.loaded);
        assert!(std::env::var("FOO").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_reason_refuses_an_owner_mismatch_even_at_a_safe_mode() {
        // #471 review finding 3's own probe: #468 requires this case,
        // and `permission_refusal`'s own `getuid()` call left no way to
        // supply a different uid without owning two real uids --
        // `unsafe_reason` takes both as plain arguments instead.
        let path = Path::new("/home/someone/.config/circuitry/.env");
        let reason = unsafe_reason(0o600, 1000, 2000, path);
        assert!(reason.unwrap().contains("not safe to load"));
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_reason_refuses_group_only_writable() {
        let path = Path::new("/home/someone/.config/circuitry/.env");
        let reason = unsafe_reason(0o620, 1000, 1000, path);
        assert!(reason.unwrap().contains("not safe to load"));
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_reason_refuses_world_only_writable() {
        let path = Path::new("/home/someone/.config/circuitry/.env");
        let reason = unsafe_reason(0o602, 1000, 1000, path);
        assert!(reason.unwrap().contains("not safe to load"));
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_reason_allows_an_owned_unwritable_file() {
        let path = Path::new("/home/someone/.config/circuitry/.env");
        assert_eq!(unsafe_reason(0o600, 1000, 1000, path), None);
    }

    #[cfg(unix)]
    #[test]
    fn group_or_world_readable_loads_with_a_softer_warning() {
        let _guard = ENV_MUTATION_LOCK.lock().unwrap();
        let dir = ScratchDir::new("readable-warning");
        let path = dir.0.join(".env");
        std::fs::write(&path, "READABLE_WARN_468=loaded\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let result = load_user_env_at(&path);
        assert!(result.loaded);
        let warning = result.warning.expect("a readable file still warns");
        assert!(warning.contains("group/world-readable"));
        assert!(warning.contains("chmod 600"));
        assert_eq!(std::env::var("READABLE_WARN_468").unwrap(), "loaded");
        unsafe { std::env::remove_var("READABLE_WARN_468") };
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_skipped_with_a_warning() {
        let _guard = ENV_MUTATION_LOCK.lock().unwrap();
        let dir = ScratchDir::new("unreadable");
        let path = dir.0.join(".env");
        std::fs::write(&path, "FOO=bar\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

        let result = load_user_env_at(&path);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!result.loaded);
        let warning = result.warning.expect("an unreadable file still warns");
        assert!(warning.contains("could not be read"));
        assert!(std::env::var("FOO").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_at_the_path_parses_as_empty_not_an_os_error() {
        let _guard = ENV_MUTATION_LOCK.lock().unwrap();
        let dir = ScratchDir::new("directory-env");
        let path = dir.0.join(".env");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let result = load_user_env_at(&path);
        assert!(result.loaded);
        assert!(result.supplied.is_empty());
        let warning = result.warning.expect("a readable directory still warns");
        assert!(warning.contains("group/world-readable"));
        assert!(!warning.contains("could not be read"));
    }

    #[test]
    fn a_single_quoted_key_containing_equals_is_skipped_not_set() {
        // #471 review finding 2's own probe: `std::env::set_var` panics
        // on a key containing `=`, and the panic message would include
        // the value -- this must never reach `set_var` at all.
        let _guard = ENV_MUTATION_LOCK.lock().unwrap();
        let dir = ScratchDir::new("secret-leak-key-equals");
        let path = dir.0.join(".env");
        std::fs::write(&path, "'A=B'=s3cr3t-probe\n").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let result = load_user_env_at(&path);
        assert!(result.loaded);
        assert!(result.supplied.is_empty());
        assert_eq!(result.skipped, vec!["A=B".to_string()]);
        assert!(std::env::var("A=B").is_err());
    }

    #[test]
    fn a_nul_byte_in_a_value_is_skipped_not_set() {
        // #471 review finding 2's other probe: a raw NUL decodes as
        // valid UTF-8, but `std::env::set_var` panics on one in a
        // value, again printing the value in its own panic message.
        let _guard = ENV_MUTATION_LOCK.lock().unwrap();
        let dir = ScratchDir::new("secret-leak-nul-value");
        let path = dir.0.join(".env");
        std::fs::write(&path, b"K_468_NUL=ab\x00cd\n").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let result = load_user_env_at(&path);
        assert!(result.loaded);
        assert!(result.supplied.is_empty());
        assert_eq!(result.skipped, vec!["K_468_NUL".to_string()]);
        assert!(std::env::var("K_468_NUL").is_err());
    }

    #[test]
    fn invalid_entry_warning_quotes_the_key_with_python_repr_not_rust_debug() {
        // #473: the reference's own `{path}: skipping {key!r}: not a
        // valid environment variable name or value.`, byte for byte --
        // Rust's `{key:?}` Debug formatting would double-quote instead.
        let path = Path::new("/home/someone/.config/circuitry/.env");
        assert_eq!(
            invalid_entry_warning(path, "A=B"),
            "/home/someone/.config/circuitry/.env: skipping 'A=B': \
             not a valid environment variable name or value."
        );
    }

    #[test]
    fn invalid_entry_warning_never_contains_a_value() {
        let path = Path::new("/home/someone/.config/circuitry/.env");
        let warning = invalid_entry_warning(path, "K_468_NUL");
        assert!(!warning.contains('\0'));
        assert_eq!(
            warning,
            "/home/someone/.config/circuitry/.env: skipping 'K_468_NUL': \
             not a valid environment variable name or value."
        );
    }

    #[test]
    fn undecodable_file_is_skipped_with_a_warning() {
        let _guard = ENV_MUTATION_LOCK.lock().unwrap();
        let dir = ScratchDir::new("undecodable");
        let path = dir.0.join(".env");
        std::fs::write(&path, [0xffu8, 0xfe, 0x00]).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let result = load_user_env_at(&path);
        assert!(!result.loaded);
        let warning = result.warning.expect("undecodable file still warns");
        assert!(warning.contains("UnicodeDecodeError"));
    }

    #[test]
    fn interpolation_reads_the_real_environment() {
        let _guard = ENV_MUTATION_LOCK.lock().unwrap();
        let dir = ScratchDir::new("interpolation");
        let path = dir.0.join(".env");
        std::fs::write(&path, "GREETING_468=${USER_468_NAME:-stranger}\n").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        unsafe { std::env::set_var("USER_468_NAME", "Ada") };
        let result = load_user_env_at(&path);
        assert!(result.loaded);
        assert_eq!(std::env::var("GREETING_468").unwrap(), "Ada");
        unsafe {
            std::env::remove_var("USER_468_NAME");
            std::env::remove_var("GREETING_468");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_real_non_utf8_value_still_wins_over_the_file() {
        // #471 review finding 8: the real environment wins on
        // `var_os`, never on whether the real value happens to decode
        // as UTF-8 -- the same reasoning `real_env`'s own doc comment
        // gives for using `var_os` here rather than a `HashMap` lookup.
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let _guard = ENV_MUTATION_LOCK.lock().unwrap();
        let dir = ScratchDir::new("non-utf8-precedence");
        let path = dir.0.join(".env");
        std::fs::write(&path, "NON_UTF8_468=from_file\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let non_utf8 = OsStr::from_bytes(&[0x66, 0x6f, 0xff, 0x6f]);
        unsafe { std::env::set_var("NON_UTF8_468", non_utf8) };

        let result = load_user_env_at(&path);
        assert!(result.loaded);
        assert_eq!(result.skipped, vec!["NON_UTF8_468".to_string()]);
        assert!(result.supplied.is_empty());
        assert_eq!(std::env::var_os("NON_UTF8_468").as_deref(), Some(non_utf8));
        unsafe { std::env::remove_var("NON_UTF8_468") };
    }
}
