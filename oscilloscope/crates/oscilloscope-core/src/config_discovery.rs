//! Mirrors cof's own config discovery (`cli/config.py`'s
//! `resolve_config`/`discover_project_config`, `cli/config_trust.py`'s
//! `check_trust`), narrowed to the one case `osi`'s plan compile
//! cares about: no `--config` and no config positional, the case a
//! bare `cof run <doc>` resolves on its own by layering the global
//! config (`~/.config/circuitry/config.json`) and a *trusted* project
//! config discovered in the working directory (issue #433). Without
//! this, a `group:`/`runtime:` block defined only in one of those two
//! layers made the plan compile fall back to no plan at all, even
//! though the real run would see it.
//!
//! Deliberately narrower than `resolve_config`: no `CIRCUITRY_CONFIG`/
//! `--config` branch (the caller already has its own config argument
//! for that, handled before this module is ever consulted), no
//! `CIRCUITRY_*` environment overlay (`electricity::check_options`,
//! which the caller feeds this module's own output into, already
//! applies those once it has a file to layer them onto), and no
//! allowlist narrowing (`_narrow_allowlists` only affects adapter/tool
//! enforcement, never plan structure, and `check_for_run` skips
//! preflight entirely either way). `home`, `cwd` and the trust env
//! var's own value are all explicit parameters, never read from the
//! process environment inside this module itself, so a test is
//! structurally unable to touch the real `$HOME` or a real
//! `CIRCUITRY_TRUST_PROJECT_CONFIG` by forgetting to override it.

use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

/// `cli/config.py::DEFAULT_CONFIG_FILENAMES` -- checked in this order;
/// the first one present in `cwd` is the discovered project config,
/// whether or not it turns out to be trusted.
const DEFAULT_CONFIG_FILENAMES: [&str; 2] = ["circuitry.config.json", "config.json"];

const TRUST_STORE_FILENAME: &str = "trusted.json";

/// `cli/config_trust.py::TRUST_PROJECT_CONFIG_ENV`.
pub const TRUST_PROJECT_CONFIG_ENV: &str = "CIRCUITRY_TRUST_PROJECT_CONFIG";

fn global_config_path(home: &Path) -> PathBuf {
    home.join(".config").join("circuitry").join("config.json")
}

fn trust_store_path(home: &Path) -> PathBuf {
    home.join(".config")
        .join("circuitry")
        .join(TRUST_STORE_FILENAME)
}

fn discover_project_config(cwd: &Path) -> Option<PathBuf> {
    DEFAULT_CONFIG_FILENAMES
        .iter()
        .map(|name| cwd.join(name))
        .find(|p| p.is_file())
}

fn config_digest(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// `cli/config_trust.py::env_trusts_project_config`.
fn env_trusts(trust_env: Option<&str>) -> bool {
    matches!(
        trust_env.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// `cli/config_trust.py::check_trust`, narrowed to whether `path`
/// (whose contents are `data`) may be applied at all -- `trusted`/
/// `env` both do, `untrusted`/`changed` (an unreadable store counts as
/// `untrusted`) neither does; osi's plan compile has no `cof trust`/
/// `cof doctor` surface of its own to explain *why* a skip happened.
/// `trust_key` in the real module resolves symlinks too
/// (`path.expanduser().resolve()`); `path` here is already expected to
/// be canonicalized by the caller for the same reason.
fn project_config_is_trusted(
    canonical_path: &Path,
    data: &[u8],
    trust_store_path: &Path,
    trust_env: Option<&str>,
) -> bool {
    if env_trusts(trust_env) {
        return true;
    }
    let Ok(text) = std::fs::read_to_string(trust_store_path) else {
        return false;
    };
    let Ok(store) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    let Some(trusted) = store.get("trusted").and_then(Value::as_object) else {
        return false;
    };
    let key = canonical_path.to_string_lossy();
    let Some(entry) = trusted.get(key.as_ref()) else {
        return false;
    };
    entry.get("sha256").and_then(Value::as_str) == Some(config_digest(data).as_str())
}

/// `cli/config.py::_deep_merge`: `overlay`'s keys win; nested objects
/// recurse, everything else (arrays included -- they replace, never
/// concatenate) is a plain overwrite.
pub fn deep_merge(base: &Value, overlay: &Value) -> Value {
    match (base, overlay) {
        (Value::Object(base_map), Value::Object(overlay_map)) => {
            let mut merged = base_map.clone();
            for (key, value) in overlay_map {
                let new_value = match merged.get(key) {
                    Some(existing) => deep_merge(existing, value),
                    None => value.clone(),
                };
                merged.insert(key.clone(), new_value);
            }
            Value::Object(merged)
        }
        _ => overlay.clone(),
    }
}

/// The config a bare `cof run <doc>` (no `--config`) would layer on
/// its own from `home`/`cwd`: the global config, then a project config
/// discovered in `cwd`, deep-merged on top of it only once it passes
/// [`project_config_is_trusted`]. `None` when neither layer applies
/// (no global config file, and no project config file, or one that's
/// discovered but not trusted) -- the caller's own existing no-config
/// fallback is exactly as correct for that case as it always was.
pub fn discover_merged_config(home: &Path, cwd: &Path, trust_env: Option<&str>) -> Option<Value> {
    let mut merged: Option<Value> = None;

    if let Ok(text) = std::fs::read_to_string(global_config_path(home)) {
        if let Ok(global_value) = serde_json::from_str::<Value>(&text) {
            if global_value.is_object() {
                merged = Some(global_value);
            }
        }
    }

    if let Some(project_path) = discover_project_config(cwd) {
        if let Ok(data) = std::fs::read(&project_path) {
            if let Ok(project_value) = serde_json::from_slice::<Value>(&data) {
                if project_value.is_object() {
                    let canonical =
                        std::fs::canonicalize(&project_path).unwrap_or(project_path.clone());
                    if project_config_is_trusted(
                        &canonical,
                        &data,
                        &trust_store_path(home),
                        trust_env,
                    ) {
                        merged = Some(match merged {
                            Some(base) => deep_merge(&base, &project_value),
                            None => project_value,
                        });
                    }
                }
            }
        }
    }

    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_json(path: &Path, value: &Value) {
        std::fs::write(path, value.to_string()).unwrap();
    }

    #[test]
    fn no_home_or_project_config_discovers_nothing() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        assert_eq!(discover_merged_config(home.path(), cwd.path(), None), None);
    }

    #[test]
    fn a_global_config_alone_is_discovered() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".config/circuitry")).unwrap();
        write_json(
            &home.path().join(".config/circuitry/config.json"),
            &serde_json::json!({"default_adapter": "scripted"}),
        );
        let merged = discover_merged_config(home.path(), cwd.path(), None).unwrap();
        assert_eq!(merged["default_adapter"], "scripted");
    }

    #[test]
    fn an_untrusted_project_config_is_skipped() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        write_json(
            &cwd.path().join("circuitry.config.json"),
            &serde_json::json!({"default_adapter": "scripted"}),
        );
        // No trusted.json entry, and no env override: discovered, but
        // not applied (cli/config_trust.py's own "direnv allow" model).
        assert_eq!(discover_merged_config(home.path(), cwd.path(), None), None);
    }

    #[test]
    fn a_project_config_trusted_by_env_is_merged() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        write_json(
            &cwd.path().join("circuitry.config.json"),
            &serde_json::json!({"default_adapter": "scripted"}),
        );
        let merged = discover_merged_config(home.path(), cwd.path(), Some("1")).unwrap();
        assert_eq!(merged["default_adapter"], "scripted");
    }

    #[test]
    fn a_project_config_trusted_by_the_store_is_merged_and_narrows_the_global() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".config/circuitry")).unwrap();
        write_json(
            &home.path().join(".config/circuitry/config.json"),
            &serde_json::json!({"default_adapter": "global-adapter", "default_model": "global-model"}),
        );
        let project_path = cwd.path().join("circuitry.config.json");
        write_json(
            &project_path,
            &serde_json::json!({"default_adapter": "project-adapter"}),
        );
        let data = std::fs::read(&project_path).unwrap();
        let canonical = std::fs::canonicalize(&project_path).unwrap();
        let digest = config_digest(&data);
        let mut store =
            std::fs::File::create(home.path().join(".config/circuitry/trusted.json")).unwrap();
        write!(
            store,
            "{}",
            serde_json::json!({"version": 1, "trusted": {canonical.to_string_lossy(): {"sha256": digest, "trusted_at": "2026-01-01T00:00:00Z"}}})
        )
        .unwrap();

        let merged = discover_merged_config(home.path(), cwd.path(), None).unwrap();
        assert_eq!(merged["default_adapter"], "project-adapter");
        assert_eq!(merged["default_model"], "global-model");
    }

    #[test]
    fn a_project_config_whose_content_changed_since_being_trusted_is_skipped() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".config/circuitry")).unwrap();
        let project_path = cwd.path().join("circuitry.config.json");
        write_json(
            &project_path,
            &serde_json::json!({"default_adapter": "trusted-version"}),
        );
        let canonical = std::fs::canonicalize(&project_path).unwrap();
        let stale_digest = config_digest(b"{\"default_adapter\": \"an-earlier-version\"}");
        let mut store =
            std::fs::File::create(home.path().join(".config/circuitry/trusted.json")).unwrap();
        write!(
            store,
            "{}",
            serde_json::json!({"version": 1, "trusted": {canonical.to_string_lossy(): {"sha256": stale_digest, "trusted_at": "2026-01-01T00:00:00Z"}}})
        )
        .unwrap();

        // Edit after trusting: the file on disk no longer matches the
        // digest the store recorded.
        write_json(
            &project_path,
            &serde_json::json!({"default_adapter": "edited-after-trust"}),
        );

        assert_eq!(discover_merged_config(home.path(), cwd.path(), None), None);
    }

    #[test]
    fn deep_merge_recurses_into_nested_objects_but_replaces_arrays() {
        let base = serde_json::json!({"runtime": {"a": 1, "b": 2}, "list": [1, 2]});
        let overlay = serde_json::json!({"runtime": {"b": 3}, "list": [9]});
        let merged = deep_merge(&base, &overlay);
        assert_eq!(
            merged,
            serde_json::json!({"runtime": {"a": 1, "b": 3}, "list": [9]})
        );
    }
}
