//! Rebuilds one golden case's `files` map (`reference::Case`) into a
//! fresh temporary directory -- the Rust-side counterpart of
//! `_compiler_corpus.py`'s `_materialize`, so a golden test runs
//! against the exact same tree the Python side ran `validate`/`run`
//! against.

use super::reference::FileContent;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// A temporary directory holding one case's materialized files, removed
/// when dropped.
pub struct TempCase {
    pub root: PathBuf,
}

impl TempCase {
    pub fn entry_path(&self, entry: &str) -> PathBuf {
        self.root.join(entry)
    }
}

impl Drop for TempCase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Materializes *files* into a fresh temporary directory unique to this
/// process and *tag* (so two tests running concurrently never collide).
///
/// The returned root is canonicalized (`fs::canonicalize`, resolving
/// any symlink in the temp directory's own path, e.g. macOS's `/tmp` ->
/// `/private/tmp`) -- the same resolved form `_compiler_corpus.py`
/// records (`Path(tmp).resolve()`), so [`super::reference::normalize`]
/// strips the same string on both sides.
pub fn materialize(tag: &str, files: &BTreeMap<String, FileContent>) -> TempCase {
    let root = std::env::temp_dir().join(format!(
        "electricity-compiler-golden-{tag}-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    fs::create_dir_all(&root).expect("create temp case root");
    let root = fs::canonicalize(&root).expect("canonicalize temp case root");

    // Symlinks after every plain/binary file, same ordering rule as the
    // Python side: a symlink's target may be a file this same case
    // writes.
    let mut symlinks: Vec<(PathBuf, String)> = Vec::new();
    for (relpath, content) in files {
        let dest = root.join(relpath);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        match content {
            FileContent::Text(text) => {
                fs::write(&dest, text).expect("write text file");
            }
            FileContent::BytesHex { bytes_hex } => {
                let bytes = decode_hex(bytes_hex);
                fs::write(&dest, bytes).expect("write binary file");
            }
            FileContent::Symlink { symlink } => {
                symlinks.push((dest, symlink.clone()));
            }
        }
    }
    for (dest, target) in symlinks {
        make_symlink(&target, &dest);
    }

    TempCase { root }
}

#[cfg(unix)]
fn make_symlink(target: &str, dest: &Path) {
    std::os::unix::fs::symlink(target, dest).expect("create symlink");
}

#[cfg(windows)]
fn make_symlink(target: &str, dest: &Path) {
    // Best-effort: a case that depends on symlink behavior is skipped on
    // Windows by the test itself, not here.
    let _ = std::os::windows::fs::symlink_file(target, dest);
}

fn decode_hex(hex: &str) -> Vec<u8> {
    assert!(hex.len() % 2 == 0, "odd-length hex string: {hex:?}");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("valid hex byte"))
        .collect()
}

fn unique_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}
