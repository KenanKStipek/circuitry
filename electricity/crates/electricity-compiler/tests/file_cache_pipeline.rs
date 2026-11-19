//! A document's load, consent/resume digest and compile step read each
//! file at most once while a cache is active (#405): a prompt file
//! rewritten after the digest has already read it must not change what
//! compilation then uses.

use electricity_compiler::{DocumentOrigin, compile_document, document_content_digest, file_cache};
use std::path::PathBuf;

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(tag: &str) -> TempDir {
    let root = std::env::temp_dir().join(format!(
        "electricity-file-cache-pipeline-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
    ));
    std::fs::create_dir_all(&root).unwrap();
    TempDir(std::fs::canonicalize(&root).unwrap())
}

/// A document declaring one prompt sourced from `voice.md`, written with
/// *first content* -- returns the document's own parsed `Value` and
/// *voice.md*'s path, so a test can rewrite it and recompile.
fn write_document(dir: &std::path::Path) -> (electricity_value::Value, PathBuf) {
    let prompt_path = dir.join("voice.md");
    std::fs::write(&prompt_path, "first content").unwrap();
    let doc_path = dir.join("doc.yml");
    std::fs::write(
        &doc_path,
        "prompts:\n  greeting: {file: voice.md}\neffects: []\n",
    )
    .unwrap();
    let document = electricity_compiler::load_document(&doc_path).unwrap();
    (document, prompt_path)
}

#[test]
fn digest_and_compile_share_one_read_under_a_cache() {
    let dir = temp_dir("shared");
    let (document, prompt_path) = write_document(&dir.0);
    let origin = DocumentOrigin::File {
        document_dir: dir.0.clone(),
        confinement_root: dir.0.clone(),
    };

    let _guard = file_cache::enter();
    // Fills the cache with voice.md's first-content bytes.
    document_content_digest(&dir.0.join("doc.yml"), &document, &dir.0).unwrap();
    // Rewritten after the digest already read it -- the consent decision
    // (and anything else computed from that digest) was made from the
    // first content; compilation must see the exact same bytes, not this
    // one.
    std::fs::write(&prompt_path, "second content, never consented to").unwrap();

    let program = compile_document(&document, &origin).unwrap();
    assert_eq!(
        program.prompts.get("greeting").map(String::as_str),
        Some("first content"),
        "compilation must reuse the digest's own read, not re-read the file"
    );
}

#[test]
fn without_a_cache_a_rewrite_between_digest_and_compile_is_used() {
    // The regression the test above guards against, shown directly: with
    // no cache active (the old code's own shape for this whole span), a
    // digest and a later compile are two independent reads, and a rewrite
    // in between is exactly the race #405 closes.
    let dir = temp_dir("unshared");
    let (document, prompt_path) = write_document(&dir.0);
    let origin = DocumentOrigin::File {
        document_dir: dir.0.clone(),
        confinement_root: dir.0.clone(),
    };

    document_content_digest(&dir.0.join("doc.yml"), &document, &dir.0).unwrap();
    std::fs::write(&prompt_path, "second content, never consented to").unwrap();

    let program = compile_document(&document, &origin).unwrap();
    assert_eq!(
        program.prompts.get("greeting").map(String::as_str),
        Some("second content, never consented to"),
    );
}
