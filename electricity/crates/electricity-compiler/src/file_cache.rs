//! A per-thread cache of path -> bytes already read from disk, shared by
//! every reader below in one load/consent/compile cycle (#405): the
//! document loader, the consent/resume digest and the actual compile step
//! must all see the exact same bytes for a given file, so each one is read
//! from disk at most once.
//!
//! [`enter`] activates the cache for the current thread's call stack —
//! [`crate::pipeline::check_for_run`] and `electricity::run_orchestration`'s
//! own document-load-through-compile span are its two callers — and
//! [`read_cached`] is every actual reader's entry point
//! ([`crate::prompt_files::resolve_prompt_file`], [`crate::digest::
//! document_content_digest`]/its own prompt-file walk,
//! [`crate::load::read_universal_newlines`]). Outside any [`Guard`]'s
//! scope (a lone unit test, say), [`read_cached`] is a plain, uncached
//! `std::fs::read` — exactly like before this module existed.
//!
//! A `thread_local!`, not a value threaded through every function's own
//! signature, since every caller above compiles and digests one document
//! synchronously, start to finish, on whichever thread picked it up —
//! `check_for_run`/`post_state_checks` are never `async`, and
//! `run_orchestration`'s own span has no `.await` between the document
//! load and the compile step it wraps. A caller that *does* cross an
//! `.await` between a digest and a compile of the same document (a `use`
//! child loaded on one task and compiled after a suspension point) must
//! not rely on this cache — Tokio's work-stealing can resume that task on
//! a different OS thread, where this cache would simply be empty, so
//! nothing here is unsound for it, only silently less effective; see
//! DESIGN.md §13.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

thread_local! {
    static CACHE: RefCell<Option<HashMap<PathBuf, Vec<u8>>>> = const { RefCell::new(None) };
    static READ_COUNTS: RefCell<HashMap<PathBuf, u32>> = RefCell::new(HashMap::new());
}

/// Clears this thread's real-disk-read counts (see [`read_count`]) --
/// call before the cycle under test, so an earlier test's own reads on
/// this same thread don't leak into this one's count.
pub fn reset_read_counts() {
    READ_COUNTS.with(|cell| cell.borrow_mut().clear());
}

/// How many times [`read_cached`] has actually gone to disk for *path*
/// on this thread since the last [`reset_read_counts`] -- a cache hit
/// never increments this, only a real `std::fs::read` does. Lets a
/// cycle owner's own test (`check_for_run`'s, `electricity::run`'s, a
/// `use: path:` dispatch's) assert a path was read from disk exactly
/// once across its whole call, without needing the file's content to
/// change mid-call to notice a second, redundant read.
pub fn read_count(path: &Path) -> u32 {
    READ_COUNTS.with(|cell| *cell.borrow().get(path).unwrap_or(&0))
}

fn record_read(path: &Path) {
    READ_COUNTS.with(|cell| {
        *cell.borrow_mut().entry(path.to_path_buf()).or_insert(0) += 1;
    });
}

/// Activates this thread's file-read cache for the scope of the returned
/// guard. Reentrant: a nested `enter()` (a function that itself enters the
/// cache, called by a caller that already has) just shares the outer
/// scope's cache and does nothing on drop — only the outermost guard
/// clears it.
pub fn enter() -> Guard {
    let created = CACHE.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_some() {
            false
        } else {
            *slot = Some(HashMap::new());
            true
        }
    });
    Guard(created)
}

/// RAII handle for [`enter`]'s scope — see its own doc comment.
pub struct Guard(bool);

impl Drop for Guard {
    fn drop(&mut self) {
        if self.0 {
            CACHE.with(|cell| *cell.borrow_mut() = None);
        }
    }
}

/// *path*'s bytes: from this thread's active cache (see [`enter`]) if
/// already read there, else a fresh read, recorded for next time when a
/// cache is active.
pub fn read_cached(path: &Path) -> io::Result<Vec<u8>> {
    CACHE.with(|cell| {
        let mut slot = cell.borrow_mut();
        match slot.as_mut() {
            Some(cache) => {
                if let Some(data) = cache.get(path) {
                    return Ok(data.clone());
                }
                record_read(path);
                let data = std::fs::read(path)?;
                cache.insert(path.to_path_buf(), data.clone());
                Ok(data)
            }
            None => {
                record_read(path);
                std::fs::read(path)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::Deref;

    /// A temporary directory removed when dropped -- same pattern as
    /// `crate::digest`'s own test helper.
    struct TempDir(PathBuf);

    impl Deref for TempDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(tag: &str) -> TempDir {
        let root = std::env::temp_dir().join(format!(
            "electricity-file-cache-test-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::create_dir_all(&root).unwrap();
        TempDir(std::fs::canonicalize(&root).unwrap())
    }

    #[test]
    fn reads_once_per_path_while_active() {
        let dir = temp_dir("reads-once");
        let path = dir.join("a.txt");
        std::fs::write(&path, b"first").unwrap();

        let _guard = enter();
        assert_eq!(read_cached(&path).unwrap(), b"first");
        // Changed on disk after the first read -- the cache, not the
        // filesystem, answers the second call.
        std::fs::write(&path, b"second").unwrap();
        assert_eq!(read_cached(&path).unwrap(), b"first");
    }

    #[test]
    fn uncached_outside_any_guard_rereads_every_time() {
        let dir = temp_dir("uncached");
        let path = dir.join("b.txt");
        std::fs::write(&path, b"first").unwrap();
        assert_eq!(read_cached(&path).unwrap(), b"first");
        std::fs::write(&path, b"second").unwrap();
        assert_eq!(read_cached(&path).unwrap(), b"second");
    }

    #[test]
    fn nested_enter_shares_the_outer_scope() {
        let dir = temp_dir("nested");
        let path = dir.join("c.txt");
        std::fs::write(&path, b"first").unwrap();

        let outer = enter();
        assert_eq!(read_cached(&path).unwrap(), b"first");
        {
            let inner = enter();
            std::fs::write(&path, b"second").unwrap();
            // Still a cache hit from the outer scope's own read.
            assert_eq!(read_cached(&path).unwrap(), b"first");
            drop(inner);
        }
        // The outer scope's cache survived the inner guard's drop.
        assert_eq!(read_cached(&path).unwrap(), b"first");
        drop(outer);
        assert_eq!(read_cached(&path).unwrap(), b"second");
    }
}
