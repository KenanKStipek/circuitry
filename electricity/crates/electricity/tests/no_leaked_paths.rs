//! Every committed file under `tests/golden/` carries no absolute or
//! temporary local path (issue #431's gate lane, extending
//! `electricity-compiler`'s own `no_leaked_paths.rs` test of the same
//! name to this crate's own golden run corpus).
//!
//! `electricity/scripts/_run_corpus.py` already refuses to *write* such a
//! file; this test is the second, independent check that nothing slipped
//! past that at commit time -- a stale file checked in by hand, or a
//! future generator that doesn't use `_run_corpus.py` at all.

use std::fs;
use std::path::Path;

fn golden_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden"))
}

type PathMatcher = Box<dyn Fn(&str) -> bool>;

fn leaked_path_patterns() -> Vec<(&'static str, PathMatcher)> {
    vec![
        (
            "a home directory (/Users/ or /home/)",
            Box::new(|t: &str| t.contains("/Users/") || t.contains("/home/")),
        ),
        (
            "a temp directory (/tmp/ or /var/folders/)",
            Box::new(|t: &str| t.contains("/tmp/") || t.contains("/var/folders/")),
        ),
        ("/private/", Box::new(|t: &str| t.contains("/private/"))),
        (
            "a Windows drive letter (e.g. C:\\ or C:/)",
            Box::new(|t: &str| {
                // A real path serialized into this JSON would have its
                // single `\` separator JSON-escaped to two literal
                // backslash characters in the file -- requiring that
                // (rather than one) avoids matching an ordinary JSON
                // string escape like "effects:\n" (one literal
                // backslash followed by `n`), which a naive single-
                // backslash check flags as a false positive. The letter
                // must also not be preceded by another letter or digit
                // (so it is a standalone drive letter, not the tail of
                // a word), and the `/` variant must not be followed by
                // a second `/` -- otherwise an ordinary `https://` URL
                // (schema `$schema` fields, http tool params) would
                // match on its `s:/`.
                let bytes = t.as_bytes();
                let boundary_ok = |i: usize| {
                    i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_')
                };
                bytes.windows(4).enumerate().any(|(i, w)| {
                    boundary_ok(i)
                        && w[0].is_ascii_alphabetic()
                        && w[1] == b':'
                        && w[2] == b'\\'
                        && w[3] == b'\\'
                }) || bytes.windows(3).enumerate().any(|(i, w)| {
                    boundary_ok(i)
                        && w[0].is_ascii_alphabetic()
                        && w[1] == b':'
                        && w[2] == b'/'
                        && bytes.get(i + 3) != Some(&b'/')
                })
            }),
        ),
    ]
}

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).expect("read golden dir") {
        let entry = entry.expect("read dir entry");
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[test]
fn golden_files_contain_no_local_paths() {
    let mut files = Vec::new();
    walk(golden_dir(), &mut files);
    assert!(
        !files.is_empty(),
        "expected at least one file under tests/golden/"
    );

    let patterns = leaked_path_patterns();
    let mut failures = Vec::new();
    for path in &files {
        let text = fs::read_to_string(path).unwrap_or_default();
        for (label, matches) in &patterns {
            if matches(&text) {
                failures.push(format!("{}: contains {label}", path.display()));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "golden files leak a local path -- fix the generator that wrote them:\n{}",
        failures.join("\n")
    );
}
