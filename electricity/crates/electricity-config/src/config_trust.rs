//! `cli/config_trust.py`, read-only: whether a project config file
//! discovered in the working directory (`cli/config.py::
//! discover_project_config`) may be applied, per the trust store
//! `cof trust` writes (`trusted.json`, beside the global config).
//!
//! electricity never writes this file -- there is no `electricity
//! trust` command, and none of this module's functions create or
//! modify `trusted.json`. A missing or unreadable store is simply
//! "nothing trusted" (`TrustState::Untrusted`), exactly as the
//! reference's own `check_trust` degrades on a broken store.

use electricity_value::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

/// `cli/config_trust.py::TRUST_STORE_FILENAME`.
pub const TRUST_STORE_FILENAME: &str = "trusted.json";

/// `cli/config_trust.py::TRUST_PROJECT_CONFIG_ENV`.
pub const TRUST_PROJECT_CONFIG_ENV: &str = "CIRCUITRY_TRUST_PROJECT_CONFIG";

/// `cli/config_trust.py::TrustState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustState {
    Trusted,
    Env,
    Untrusted,
    Changed,
}

impl TrustState {
    /// `ProjectConfigStatus.applied`.
    pub fn applied(self) -> bool {
        matches!(self, TrustState::Trusted | TrustState::Env)
    }

    /// `TRUST_STATE_LABELS`.
    pub fn label(self) -> &'static str {
        match self {
            TrustState::Trusted => "trusted",
            TrustState::Env => "trusted by CIRCUITRY_TRUST_PROJECT_CONFIG",
            TrustState::Untrusted => "not trusted — skipped",
            TrustState::Changed => "changed since trusted — skipped",
        }
    }
}

/// `cli/config_trust.py::config_digest`: lowercase hex SHA-256.
pub fn config_digest(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// `cli/config_trust.py::trust_key`: the store key for *path*, its
/// resolved absolute path. Falls back to the given path, absolutized
/// against the current directory, when the filesystem can't resolve
/// it (a race between the earlier read and here) -- a key that then
/// fails to match any store entry degrades to `Untrusted`, the same
/// outcome a resolve failure has in the reference (`Path.resolve()`
/// never raises; this is only reachable here at all, unlike there).
pub fn trust_key(path: &Path) -> String {
    match std::fs::canonicalize(path) {
        Ok(resolved) => resolved.display().to_string(),
        Err(_) => {
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir().unwrap_or_default().join(path)
            };
            absolute.display().to_string()
        }
    }
}

/// `cli/config_trust.py::env_trusts_project_config`.
pub fn env_trusts_project_config(env: &HashMap<String, String>) -> bool {
    env.get(TRUST_PROJECT_CONFIG_ENV).is_some_and(|raw| {
        matches!(
            raw.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// `cli/config_trust.py::_read_store`, narrowed to the one entry
/// [`check_trust`] needs: *digest*'s own `sha256`, when the store (at
/// *store_path*) has a `trusted` object and an entry for *key*. Any
/// read/parse problem, or a `trusted` entry of the wrong shape, reads
/// as "no entry" -- the reference's own `TrustStoreError` also
/// degrades to `Untrusted` at every one of this function's callers.
fn stored_digest(store_path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(store_path).ok()?;
    let raw = electricity_json::loads(&text).ok()?;
    let dict = raw.as_dict()?;
    let trusted = dict.get(&Value::Str("trusted".to_string()))?.as_dict()?;
    let entry = trusted.get(&Value::Str(key.to_string()))?.as_dict()?;
    entry
        .get(&Value::Str("sha256".to_string()))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// `cli/config_trust.py::check_trust`.
pub fn check_trust(
    path: &Path,
    data: &[u8],
    store_path: &Path,
    env: &HashMap<String, String>,
) -> TrustState {
    if env_trusts_project_config(env) {
        return TrustState::Env;
    }
    let key = trust_key(path);
    match stored_digest(store_path, &key) {
        None => TrustState::Untrusted,
        Some(digest) if digest == config_digest(data) => TrustState::Trusted,
        Some(_) => TrustState::Changed,
    }
}

/// `ProjectConfigStatus.skip_warning`: `None` when *state* applies.
pub fn skip_warning(path: &Path, state: TrustState) -> Option<String> {
    if state.applied() {
        return None;
    }
    let reason = if matches!(state, TrustState::Changed) {
        "it changed since you trusted it"
    } else {
        "it is not trusted"
    };
    Some(format!(
        "Skipped project config {}: {reason}, so none of its settings apply. Review it, then run \
         `cof trust {}` to apply it.",
        path.display(),
        shell_quote(&path.display().to_string()),
    ))
}

/// Python's `shlex.quote`, narrowed to what a filesystem path ever
/// needs: empty, or containing anything outside
/// `[A-Za-z0-9_@%+=:,./-]`, is single-quoted (with any embedded `'`
/// escaped `'"'"'`); otherwise returned verbatim. `pub(crate)`: also
/// used by [`crate::capabilities::refusal_message`], the same
/// `shlex.quote(label)` call `cli/document_consent.py::refusal_message`
/// makes.
pub(crate) fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\"'\"'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct ScratchDir(std::path::PathBuf);

    impl ScratchDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "electricity-config-trust-test-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            fs::create_dir_all(&dir).unwrap();
            ScratchDir(dir)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn no_store_at_all_is_untrusted() {
        let scratch = ScratchDir::new("no-store");
        let config_path = scratch.0.join("circuitry.config.json");
        fs::write(&config_path, "{}").unwrap();
        let store_path = scratch.0.join("trusted.json");
        let state = check_trust(&config_path, b"{}", &store_path, &HashMap::new());
        assert_eq!(state, TrustState::Untrusted);
    }

    #[test]
    fn a_matching_digest_in_the_store_is_trusted() {
        let scratch = ScratchDir::new("matching-digest");
        let config_path = scratch.0.join("circuitry.config.json");
        let data = b"{\"default_model\": \"gpt-4\"}";
        fs::write(&config_path, data).unwrap();
        let store_path = scratch.0.join("trusted.json");
        let key = trust_key(&config_path);
        let digest = config_digest(data);
        fs::write(
            &store_path,
            format!(
                "{{\"trusted\": {{{:?}: {{\"sha256\": {:?}, \"trusted_at\": \"2026-01-01T00:00:00+00:00\"}}}}}}",
                key, digest
            ),
        )
        .unwrap();
        let state = check_trust(&config_path, data, &store_path, &HashMap::new());
        assert_eq!(state, TrustState::Trusted);
    }

    #[test]
    fn an_edited_file_no_longer_matches_the_stored_digest() {
        let scratch = ScratchDir::new("changed-digest");
        let config_path = scratch.0.join("circuitry.config.json");
        fs::write(&config_path, b"{\"default_model\": \"gpt-4\"}").unwrap();
        let store_path = scratch.0.join("trusted.json");
        let key = trust_key(&config_path);
        let stale_digest = config_digest(b"{\"default_model\": \"old\"}");
        fs::write(
            &store_path,
            format!(
                "{{\"trusted\": {{{:?}: {{\"sha256\": {:?}, \"trusted_at\": \"2026-01-01T00:00:00+00:00\"}}}}}}",
                key, stale_digest
            ),
        )
        .unwrap();
        let state = check_trust(
            &config_path,
            b"{\"default_model\": \"gpt-4\"}",
            &store_path,
            &HashMap::new(),
        );
        assert_eq!(state, TrustState::Changed);
    }

    #[test]
    fn the_env_var_trusts_everything_without_a_store() {
        let scratch = ScratchDir::new("env-trust");
        let config_path = scratch.0.join("circuitry.config.json");
        fs::write(&config_path, "{}").unwrap();
        let store_path = scratch.0.join("trusted.json");
        let mut env = HashMap::new();
        env.insert(TRUST_PROJECT_CONFIG_ENV.to_string(), "1".to_string());
        let state = check_trust(&config_path, b"{}", &store_path, &env);
        assert_eq!(state, TrustState::Env);
    }

    #[test]
    fn skip_warning_is_none_once_applied() {
        assert_eq!(
            skip_warning(Path::new("/x/circuitry.config.json"), TrustState::Trusted),
            None
        );
        assert_eq!(
            skip_warning(Path::new("/x/circuitry.config.json"), TrustState::Env),
            None
        );
    }

    #[test]
    fn skip_warning_names_the_trust_command_for_an_untrusted_file() {
        let warning =
            skip_warning(Path::new("/x/circuitry.config.json"), TrustState::Untrusted).unwrap();
        assert!(warning.contains("Skipped project config /x/circuitry.config.json"));
        assert!(warning.contains("it is not trusted"));
        assert!(warning.contains("cof trust /x/circuitry.config.json"));
    }

    #[test]
    fn skip_warning_explains_a_changed_digest() {
        let warning =
            skip_warning(Path::new("/x/circuitry.config.json"), TrustState::Changed).unwrap();
        assert!(warning.contains("it changed since you trusted it"));
    }
}
