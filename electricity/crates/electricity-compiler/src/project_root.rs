//! Lane B: the confinement root `check_for_run` resolves for a document
//! with a real file of its own, porting `core/prompt_files.py`'s
//! `default_project_root` -- the nearest `circuitry.config.json`/
//! `config.json` at or above the document's directory, else that
//! directory itself.

use std::path::{Path, PathBuf};

/// Mirrors `circuitry.cli.config.DEFAULT_CONFIG_FILENAMES` (duplicated
/// in Circuitry's own `core/prompt_files.py` for the same reason this
/// crate duplicates it here: `core/` never imports `cli/`, and this
/// crate has no config-file loader of its own to share the name from).
const CONFIG_FILENAMES: [&str; 2] = ["circuitry.config.json", "config.json"];

/// `core/prompt_files.py::default_project_root(document_dir)`: walks
/// upward from *document_dir* for the nearest directory holding a
/// `circuitry.config.json`/`config.json`; falls back to *document_dir*
/// itself when none is found anywhere above it.
///
/// A *non-strict* resolve: every step is a plain `Path`/string
/// operation (`.parent()`, `.join()`) and an existence check
/// (`.is_file()`) -- never `std::fs::canonicalize`, which would error on
/// a component that doesn't exist. *document_dir* itself is never
/// required to exist.
pub fn default_project_root(document_dir: &Path) -> PathBuf {
    let mut current = document_dir;
    loop {
        for name in CONFIG_FILENAMES {
            if current.join(name).is_file() {
                return current.to_path_buf();
            }
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent,
            _ => return document_dir.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::default_project_root;
    use std::fs;

    #[test]
    fn falls_back_to_document_dir_when_no_config_file_exists_above_it() {
        let tmp = std::env::temp_dir().join(format!(
            "electricity-project-root-test-none-{}",
            std::process::id()
        ));
        let nested = tmp.join("a/b/c");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(default_project_root(&nested), nested);
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn finds_the_nearest_config_file_above_the_document() {
        let tmp = std::env::temp_dir().join(format!(
            "electricity-project-root-test-nearest-{}",
            std::process::id()
        ));
        let project = tmp.join("project");
        let nested = project.join("sub/dir");
        fs::create_dir_all(&nested).unwrap();
        fs::write(project.join("config.json"), "{}").unwrap();
        assert_eq!(default_project_root(&nested), project);
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn prefers_circuitry_config_json_name_but_accepts_either() {
        let tmp = std::env::temp_dir().join(format!(
            "electricity-project-root-test-either-{}",
            std::process::id()
        ));
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("circuitry.config.json"), "{}").unwrap();
        assert_eq!(default_project_root(&tmp), tmp);
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn stops_at_the_nearest_rather_than_an_outer_config_file() {
        let tmp = std::env::temp_dir().join(format!(
            "electricity-project-root-test-nearest-wins-{}",
            std::process::id()
        ));
        let outer = tmp.join("outer");
        let inner = outer.join("inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(outer.join("config.json"), "{}").unwrap();
        fs::write(inner.join("config.json"), "{}").unwrap();
        assert_eq!(default_project_root(&inner), inner);
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn document_dir_need_not_exist_on_disk() {
        let missing = std::env::temp_dir().join(format!(
            "electricity-project-root-test-missing-{}/nested",
            std::process::id()
        ));
        assert_eq!(default_project_root(&missing), missing);
    }
}
