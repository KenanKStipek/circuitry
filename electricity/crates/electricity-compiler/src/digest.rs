//! Lane D: ports `core/prompt_compose.py::document_content_digest`
//! byte for byte -- the document bytes plus, for each referenced prompt
//! file, its relative-path label, its 8-byte length and its bytes
//! (#407's single algorithm).
//!
//! Called from `pipeline.rs` (lane B), which has the resolved path and
//! has already read the document's own bytes by the time it has a
//! compiled [`electricity_bytecode::Program`] to attach a
//! [`electricity_bytecode::DocumentInfo`] to -- `compile_document`
//! itself never sees raw bytes or the path as given (see
//! `electricity_bytecode::Program::document`'s doc comment).

use crate::CompileError;
use crate::compose::{CHILD_LISTS, dict_get, effects_or_steps, finally_list};
use crate::prompt_files::{file_shaped, resolve_non_strict, resolve_prompt_file_path};
use electricity_value::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

/// `document_content_digest(resolved_path, document, confinement_root)`
/// -- the document's bytes plus every `{file: ...}` prompt source it
/// references, hashed the way `core/prompt_compose.py` hashes them:
/// SHA-256 over *resolved_path*'s own bytes, then, for each referenced
/// prompt file, in ascending order of resolved absolute path (compared
/// component by component, matching Python's `Path` ordering), its
/// label, its byte length as 8 big-endian bytes, then its bytes.
///
/// Reading *resolved_path* itself is not best-effort -- a caller that
/// cannot even read the document it just compiled gets an `Err`
/// (`cli/runtime_shim.py::run`'s own treatment: "the digest is `null`
/// if the document became unreadable between the earlier load and this
/// hash", DESIGN.md §6.10). Everything after that first read *is*
/// best-effort, mirroring `core/prompt_compose.py`'s own bare `except
/// Exception: pass`: a failure resolving the document's directory or
/// reading any one referenced prompt file silently stops adding files,
/// leaving the digest as whatever was hashed before that point.
pub fn document_content_digest(
    resolved_path: &Path,
    document: &Value,
    confinement_root: &Path,
) -> Result<String, CompileError> {
    let document_bytes = std::fs::read(resolved_path).map_err(|err| {
        CompileError(format!("could not read {}: {err}", resolved_path.display()))
    })?;

    let mut hasher = Sha256::new();
    hasher.update(&document_bytes);
    let _ = add_prompt_files(&mut hasher, resolved_path, document, confinement_root);

    Ok(format!("{:x}", hasher.finalize()))
}

/// Best-effort: resolves the document's directory, finds every `{file:
/// ...}` reference [`referenced_file_values`] turns up (silently
/// dropping one that doesn't actually resolve -- `core/prompt_compose.
/// py::referenced_prompt_file_paths`'s own per-reference swallow), then
/// hashes each resolved file in sorted order, stopping -- via `?` -- at
/// the first `io::Error` resolving the document directory or reading a
/// file, leaving every `hasher.update` call already made in place.
fn add_prompt_files(
    hasher: &mut Sha256,
    resolved_path: &Path,
    document: &Value,
    confinement_root: &Path,
) -> io::Result<()> {
    let document_dir = resolve_non_strict(resolved_path)?
        .parent()
        .ok_or_else(|| io::Error::other("document path has no parent"))?
        .to_path_buf();

    let mut paths: BTreeSet<PathBuf> = BTreeSet::new();
    for value in referenced_file_values(document) {
        let Some(file_value) = file_shaped(value) else {
            continue;
        };
        if let Ok((_, resolved)) =
            resolve_prompt_file_path(file_value, "file", &document_dir, confinement_root)
        {
            paths.insert(resolved);
        }
    }

    for prompt_file in paths {
        let data = std::fs::read(&prompt_file)?;
        let label = relative_label(&document_dir, &prompt_file);
        hasher.update(label.as_bytes());
        hasher.update((data.len() as u64).to_be_bytes());
        hasher.update(&data);
    }
    Ok(())
}

/// Every value that could be a `{file: ...}` prompt source: the
/// top-level `prompts:` map's own values, and, walked the same way
/// [`crate::compose::all_effect_names`] walks the document, each
/// effect's `template` and `messages[].content` -- `core/prompt_
/// compose.py::referenced_prompt_file_paths`'s own two scans. Params/
/// `use` inputs are deliberately excluded: Circuitry's own digest
/// doesn't hash those either (only `template`/`messages[].content` --
/// and a declared prompt -- may be `{file: ...}`-shaped at all; #396
/// §3).
///
/// Walks with an explicit stack rather than recursion, so a document as
/// deep as [`electricity_value::MAX_DEPTH`] allows can't overflow the
/// stack here either.
fn referenced_file_values(document: &Value) -> Vec<&Value> {
    let mut found = Vec::new();
    if let Some(Value::Dict(prompts)) = dict_get(document, "prompts") {
        found.extend(prompts.values());
    }

    let mut pending: Vec<&Value> = Vec::new();
    pending.extend(effects_or_steps(document).iter());
    pending.extend(finally_list(document).iter());

    while let Some(effect) = pending.pop() {
        let Some(dict) = effect.as_dict() else {
            continue;
        };
        if let Some(value) = dict_get(effect, "template") {
            found.push(value);
        }
        if let Some(Value::List(messages)) = dict_get(effect, "messages") {
            for message in messages {
                if let Some(content) = dict_get(message, "content") {
                    found.push(content);
                }
            }
        }
        for field in CHILD_LISTS {
            if let Some(Value::List(items)) = dict.get(&Value::Str(field.to_string())) {
                pending.extend(items.iter());
            }
        }
    }
    found
}

/// *prompt_file*'s path, relative to *document_dir*, `/`-separated --
/// `Path(os.path.relpath(prompt_file, document_dir)).as_posix()`.
/// Location-independent: it never depends on where the project sits on
/// disk, a leading `../` included (both paths are already absolute and
/// resolved, so this is a plain common-prefix-then-remainder
/// computation, never touching the filesystem again).
fn relative_label(document_dir: &Path, prompt_file: &Path) -> String {
    let from: Vec<_> = document_dir.components().collect();
    let to: Vec<_> = prompt_file.components().collect();
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let mut parts: Vec<String> = Vec::new();
    parts.extend(std::iter::repeat_n("..".to_string(), from.len() - common));
    parts.extend(
        to[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );

    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "electricity-digest-test-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::canonicalize(&root).unwrap()
    }

    fn file_value_dict(path: &str) -> Value {
        let mut dict = Dict::new();
        dict.insert(Value::Str("file".to_string()), Value::Str(path.to_string()));
        Value::Dict(dict)
    }

    #[test]
    fn digest_of_a_document_with_no_prompt_files_is_just_the_document_bytes() {
        let dir = temp_dir("no-files");
        let doc_path = dir.join("doc.yml");
        fs::write(&doc_path, "effects: []\n").unwrap();
        let document = Value::Dict(Dict::new());

        let digest = document_content_digest(&doc_path, &document, &dir).unwrap();

        let mut expected = Sha256::new();
        expected.update(b"effects: []\n");
        assert_eq!(digest, format!("{:x}", expected.finalize()));
    }

    #[test]
    fn digest_changes_when_a_referenced_prompt_file_changes() {
        let dir = temp_dir("prompt-file-changes");
        let doc_path = dir.join("doc.yml");
        fs::write(&doc_path, "prompts:\n  voice: {file: voice.md}\n").unwrap();
        fs::write(dir.join("voice.md"), "be nice").unwrap();

        let mut prompts = Dict::new();
        prompts.insert(Value::Str("voice".to_string()), file_value_dict("voice.md"));
        let mut document = Dict::new();
        document.insert(Value::Str("prompts".to_string()), Value::Dict(prompts));
        let document = Value::Dict(document);

        let before = document_content_digest(&doc_path, &document, &dir).unwrap();
        fs::write(dir.join("voice.md"), "be mean").unwrap();
        let after = document_content_digest(&doc_path, &document, &dir).unwrap();

        assert_ne!(before, after);
    }

    #[test]
    fn digest_is_independent_of_where_the_project_sits_on_disk() {
        let dir_a = temp_dir("relocate-a");
        let dir_b = temp_dir("relocate-b");
        for dir in [&dir_a, &dir_b] {
            fs::write(dir.join("doc.yml"), "prompts:\n  voice: {file: voice.md}\n").unwrap();
            fs::write(dir.join("voice.md"), "be nice").unwrap();
        }

        let mut prompts = Dict::new();
        prompts.insert(Value::Str("voice".to_string()), file_value_dict("voice.md"));
        let mut document = Dict::new();
        document.insert(Value::Str("prompts".to_string()), Value::Dict(prompts));
        let document = Value::Dict(document);

        let digest_a = document_content_digest(&dir_a.join("doc.yml"), &document, &dir_a).unwrap();
        let digest_b = document_content_digest(&dir_b.join("doc.yml"), &document, &dir_b).unwrap();
        assert_eq!(digest_a, digest_b);
    }

    #[test]
    fn digest_includes_a_prompt_file_outside_the_documents_own_directory() {
        let project = temp_dir("dotdot-project");
        fs::create_dir(project.join("docs")).unwrap();
        let doc_path = project.join("docs/doc.yml");
        fs::write(&doc_path, "prompts:\n  voice: {file: ../shared/voice.md}\n").unwrap();
        fs::create_dir(project.join("shared")).unwrap();
        fs::write(project.join("shared/voice.md"), "shared voice").unwrap();

        let mut prompts = Dict::new();
        prompts.insert(
            Value::Str("voice".to_string()),
            file_value_dict("../shared/voice.md"),
        );
        let mut document = Dict::new();
        document.insert(Value::Str("prompts".to_string()), Value::Dict(prompts));
        let document = Value::Dict(document);

        let with_file = document_content_digest(&doc_path, &document, &project).unwrap();
        let without_file =
            document_content_digest(&doc_path, &Value::Dict(Dict::new()), &project).unwrap();
        assert_ne!(with_file, without_file);
    }

    #[test]
    fn unreadable_document_is_an_error() {
        let dir = temp_dir("unreadable-doc");
        let doc_path = dir.join("missing.yml");
        let document = Value::Dict(Dict::new());
        assert!(document_content_digest(&doc_path, &document, &dir).is_err());
    }

    #[test]
    fn relative_label_handles_a_parent_directory_reference() {
        let document_dir = Path::new("/a/b/docs");
        let prompt_file = Path::new("/a/b/shared/voice.md");
        assert_eq!(
            relative_label(document_dir, prompt_file),
            "../shared/voice.md"
        );
    }

    #[test]
    fn relative_label_handles_a_sibling_file() {
        let document_dir = Path::new("/a/b");
        let prompt_file = Path::new("/a/b/voice.md");
        assert_eq!(relative_label(document_dir, prompt_file), "voice.md");
    }

    /// Cross-checked by hand against `core.prompt_compose.
    /// document_content_digest` on the same tree (a document under
    /// `docs/`, a `{file: ../shared/voice.md}` declared prompt): both
    /// produced `59877614ff1e02817ecb5507898af22dbaca7f5ab5ceb5f1f3658f824adc01d5`.
    /// `tests/golden_compose.rs`'s own `every_case_digest_matches_
    /// circuitry` test now covers this same shape (and every other
    /// corpus case) directly against this crate's public
    /// [`document_content_digest`], independent of whether
    /// `compile_document` itself can reach it yet; this test keeps the
    /// exact bytes pinned at the unit level too.
    #[test]
    fn matches_circuitrys_own_digest_for_a_parent_directory_prompt_file() {
        let dir = temp_dir("matches-circuitry");
        fs::create_dir(dir.join("docs")).unwrap();
        fs::create_dir(dir.join("shared")).unwrap();
        let doc_path = dir.join("docs/doc.yml");
        fs::write(&doc_path, "prompts:\n  voice: {file: ../shared/voice.md}\n").unwrap();
        fs::write(dir.join("shared/voice.md"), "shared voice").unwrap();

        let mut prompts = Dict::new();
        prompts.insert(
            Value::Str("voice".to_string()),
            file_value_dict("../shared/voice.md"),
        );
        let mut document = Dict::new();
        document.insert(Value::Str("prompts".to_string()), Value::Dict(prompts));
        let document = Value::Dict(document);

        let digest = document_content_digest(&doc_path, &document, &dir).unwrap();
        assert_eq!(
            digest,
            "59877614ff1e02817ecb5507898af22dbaca7f5ab5ceb5f1f3658f824adc01d5"
        );
    }
}
