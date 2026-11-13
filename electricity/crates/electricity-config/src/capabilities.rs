//! `plugins/capabilities.py`/`cli/document_consent.py`, read-only -- the
//! capability-tag table a tool provider name maps to
//! ([`capabilities_of`]), and the pieces [`crate`]'s one caller (the
//! `electricity` lib crate's own run wiring, which alone can walk a
//! document's `use` children -- see `allowlist.rs`'s own module doc on
//! why that walk can't live in this crate) needs to reproduce issue
//! #337's cache-path gate for issue #728: a document named by its own
//! path is never gated on capability consent (`DESIGN.md` §1/§14), with
//! one exception the reference carves out -- a path that resolves
//! inside the library cache directory (`github_source.py::
//! default_cache_root`) is naming the exact same fetched content a
//! library-name run would, so it is gated exactly as if it were one.
//!
//! electricity has no library-source registry of its own (no
//! `runtime.library` sources, `electricity-config/src/effective_
//! settings.rs`'s own doc comment), so [`is_cache_path`] checks only
//! the one cache root every `LibraryRegistry.is_cache_path` call always
//! includes regardless of config -- never a configured source's own
//! cache directory, since electricity has none to configure.
//!
//! electricity never writes `trusted.json` -- [`consented_capabilities`]
//! is a read-only lookup of its `document_capabilities` section
//! (`cli/document_consent.py`'s own store key), and there is no
//! electricity-side path that calls `record_consent`/prompts a user.

use crate::config_trust;
use electricity_value::Value;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

pub const SHELL: &str = "shell";
pub const PYTHON_EVAL: &str = "python_eval";
pub const FS_WRITE: &str = "fs-write";
pub const NETWORK: &str = "network";

/// `plugins/capabilities.py::PLUGIN_CAPABILITIES`, name for name --
/// unlike `electricity_tools::registry`, this includes every tool
/// provider name the reference recognizes, not only the ones this
/// build of electricity can actually run: the gate below judges a
/// document's own `provider:` text the same way `cof run` would,
/// whether or not electricity happens to support dispatching it.
const PLUGIN_CAPABILITIES: &[(&str, &[&str])] = &[
    ("shell", &[SHELL]),
    ("python_eval", &[PYTHON_EVAL]),
    ("agent", &[SHELL, FS_WRITE, NETWORK]),
    ("docker", &[SHELL, NETWORK]),
    ("kubectl", &[SHELL, NETWORK]),
    ("gh", &[SHELL, NETWORK]),
    ("git", &[SHELL, NETWORK]),
    ("yt_dlp", &[SHELL, NETWORK]),
    ("ripgrep", &[SHELL]),
    ("pytest", &[SHELL]),
    ("awk", &[SHELL]),
    ("sed", &[SHELL]),
    ("pandoc", &[SHELL]),
    ("mediainfo", &[SHELL]),
    ("imagemagick", &[SHELL]),
    ("exiftool", &[SHELL]),
    ("7z", &[SHELL]),
    ("ping", &[SHELL, NETWORK]),
    ("traceroute", &[SHELL, NETWORK]),
    ("linter", &[SHELL]),
    ("ocr", &[SHELL]),
    ("gpg", &[SHELL]),
    ("diff_patch", &[SHELL]),
    ("pdf_render", &[SHELL]),
    ("ffmpeg", &[SHELL]),
    ("service", &[SHELL, NETWORK]),
    ("fs", &[FS_WRITE]),
    ("tar", &[FS_WRITE]),
    ("zip", &[FS_WRITE]),
    ("gzip", &[FS_WRITE]),
    ("vector_search", &[FS_WRITE]),
    ("comfyui", &[NETWORK]),
    ("http", &[NETWORK]),
    ("email_smtp", &[NETWORK]),
    ("port_check", &[NETWORK]),
    ("dns", &[NETWORK]),
    ("whois", &[NETWORK]),
    ("rss", &[NETWORK]),
    ("wikipedia", &[NETWORK]),
    ("webhook", &[NETWORK]),
    ("web_fetch", &[NETWORK]),
    ("web_search", &[NETWORK]),
    ("weather", &[NETWORK]),
    ("s3", &[NETWORK]),
    ("surrealdb", &[NETWORK]),
    ("systemone", &[NETWORK]),
    ("mcp", &[NETWORK]),
    ("linear", &[NETWORK]),
    ("slack", &[NETWORK]),
    ("discord", &[NETWORK]),
    ("github", &[NETWORK]),
    ("jira", &[NETWORK]),
    ("notion", &[NETWORK]),
    ("gcalendar", &[NETWORK]),
    ("gdrive", &[NETWORK, FS_WRITE]),
    ("playwright", &[NETWORK]),
    ("screenshot", &[NETWORK, FS_WRITE]),
];

/// `plugins/capabilities.py::capabilities_of`: an unrecognized or
/// untagged name needs none -- the empty set, never an error.
pub fn capabilities_of(plugin_name: &str) -> BTreeSet<String> {
    let normalized = plugin_name.trim().to_lowercase();
    PLUGIN_CAPABILITIES
        .iter()
        .find(|(name, _)| *name == normalized)
        .map(|(_, caps)| caps.iter().map(|c| c.to_string()).collect())
        .unwrap_or_default()
}

fn expand_user(raw: &str, home: Option<&Path>) -> PathBuf {
    match raw.strip_prefix('~') {
        Some("") => home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(raw)),
        Some(rest) if rest.starts_with('/') => home
            .map(|h| h.join(rest.trim_start_matches('/')))
            .unwrap_or_else(|| PathBuf::from(raw)),
        _ => PathBuf::from(raw),
    }
}

/// `github_source.py::default_cache_root`: `~/.cache/circuitry/library`,
/// honouring `XDG_CACHE_HOME`; `CIRCUITRY_CACHE_DIR` overrides both.
/// `None` only when no override is set and *home* is unresolvable (no
/// `HOME`/no home directory at all) -- the one case the reference
/// itself cannot hit, since `Path.home()` raises there rather than
/// returning `None` (a process in that state has already failed
/// earlier, resolving the global config path).
pub fn cache_root(env: &HashMap<String, String>, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = env.get("CIRCUITRY_CACHE_DIR").filter(|v| !v.is_empty()) {
        return Some(expand_user(dir, home));
    }
    let base = match env.get("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => expand_user(dir, home),
        None => home?.join(".cache"),
    };
    Some(base.join("circuitry").join("library"))
}

#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    a.file_type() == b.file_type() && a.len() == b.len()
}

/// `library_sources.py::LibraryRegistry.is_cache_path`, narrowed to the
/// one cache root [`cache_root`] names (module doc) -- whether *path*,
/// resolved (symlinks followed), lies inside it. Compares directory
/// *identity* (`dev`/`ino`), not path strings, for the same reason the
/// reference does: a case-altered or firmlinked path to the exact same
/// cache directory must not skip the gate on a case-insensitive
/// filesystem.
pub fn is_cache_path(path: &Path, env: &HashMap<String, String>, home: Option<&Path>) -> bool {
    let Ok(resolved) = path.canonicalize() else {
        return false;
    };
    let mut ancestor_stats = Vec::new();
    let mut current = Some(resolved.as_path());
    while let Some(p) = current {
        if let Ok(meta) = std::fs::metadata(p) {
            ancestor_stats.push(meta);
        }
        current = p.parent();
    }
    if ancestor_stats.is_empty() {
        return false;
    }
    let Some(cache_dir) = cache_root(env, home) else {
        return false;
    };
    let Ok(cache_resolved) = cache_dir.canonicalize() else {
        return false;
    };
    let Ok(cache_meta) = std::fs::metadata(&cache_resolved) else {
        return false;
    };
    ancestor_stats.iter().any(|st| same_file(st, &cache_meta))
}

/// `cli/document_consent.py::_read_entries`, narrowed to one digest's
/// own entry -- `trusted.json`'s `document_capabilities` section, read
/// only, never written.
pub fn consented_capabilities(digest: &str, store_path: &Path) -> Option<BTreeSet<String>> {
    let text = std::fs::read_to_string(store_path).ok()?;
    let raw = electricity_json::loads(&text).ok()?;
    let dict = raw.as_dict()?;
    let section = dict
        .get(&Value::Str("document_capabilities".to_string()))?
        .as_dict()?;
    let entry = section.get(&Value::Str(digest.to_string()))?.as_dict()?;
    let caps = entry
        .get(&Value::Str("capabilities".to_string()))?
        .as_list()?;
    Some(
        caps.iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
    )
}

/// `cli/document_consent.py::refusal_message`.
pub fn refusal_message(label: &str, missing: &BTreeSet<String>) -> String {
    let names = missing.iter().cloned().collect::<Vec<_>>().join(", ");
    format!(
        "'{label}' needs capabilities nobody has consented to: {names}. Run `cof trust {}` to \
         review and approve it, or pass `--allow-capabilities {names}` for a scripted/CI run.",
        config_trust::shell_quote(label)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_of_is_empty_for_an_untagged_plugin() {
        assert!(capabilities_of("json").is_empty());
        assert!(capabilities_of("unknown-plugin").is_empty());
    }

    #[test]
    fn capabilities_of_normalizes_case_and_whitespace() {
        assert_eq!(
            capabilities_of(" Shell "),
            BTreeSet::from([SHELL.to_string()])
        );
    }

    #[test]
    fn capabilities_of_can_carry_more_than_one_tag() {
        assert_eq!(
            capabilities_of("agent"),
            BTreeSet::from([SHELL.to_string(), FS_WRITE.to_string(), NETWORK.to_string()])
        );
    }

    #[test]
    fn cache_root_honors_the_override_env_var() {
        let mut env = HashMap::new();
        env.insert(
            "CIRCUITRY_CACHE_DIR".to_string(),
            "/custom/cache".to_string(),
        );
        assert_eq!(
            cache_root(&env, Some(Path::new("/home/x"))),
            Some(PathBuf::from("/custom/cache"))
        );
    }

    #[test]
    fn cache_root_falls_back_to_home_dot_cache() {
        let env = HashMap::new();
        assert_eq!(
            cache_root(&env, Some(Path::new("/home/x"))),
            Some(PathBuf::from("/home/x/.cache/circuitry/library"))
        );
    }

    #[test]
    fn is_cache_path_is_false_with_no_cache_root_resolvable() {
        let env = HashMap::new();
        assert!(!is_cache_path(Path::new("/tmp"), &env, None));
    }

    #[test]
    fn refusal_message_matches_the_references_own_wording() {
        let missing = BTreeSet::from([SHELL.to_string()]);
        let message = refusal_message("/x/doc.yml", &missing);
        assert_eq!(
            message,
            "'/x/doc.yml' needs capabilities nobody has consented to: shell. Run \
             `cof trust /x/doc.yml` to review and approve it, or pass `--allow-capabilities \
             shell` for a scripted/CI run."
        );
    }
}
