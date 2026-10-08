//! On a 2 MiB thread stack, loading and checking the deepest nesting
//! `electricity-yaml` accepts must never overflow (issue #408's lane B
//! acceptance criterion) -- the same standard `electricity-yaml`'s own
//! `tests/nesting_limit.rs` holds itself to, applied here to
//! [`load_document`]/[`structural_errors`]/[`check_report`]'s own
//! recursive walks (the near-miss unknown-key walk, the `group:`-
//! placement walk, and the `Value` -> JSON-Schema-instance conversion).

use electricity_compiler::{CheckOptions, check_report, load_document, structural_errors};

const SMALL_STACK: usize = 2 * 1024 * 1024;

/// `n` levels of a single-child `dynamic` effect nested under the
/// previous one's own `effects:` list -- each level costs two
/// `Value::depth()` units (the effect's own `Dict`, then its
/// `effects:` key's `List`), so `n` levels of this shape stay within
/// `electricity_yaml::MAX_DEPTH` (512) for `n` up to 255.
fn nested_dynamic_effects(n: usize) -> String {
    let mut text = String::from("effects:\n");
    for i in 0..n {
        let indent = "  ".repeat(i + 1);
        text.push_str(&format!("{indent}- type: dynamic\n"));
        text.push_str(&format!("{indent}  name: d{i}\n"));
        text.push_str(&format!("{indent}  effects:\n"));
    }
    text
}

#[test]
fn deepest_accepted_nesting_does_not_overflow_a_2mib_thread() {
    let path = std::env::temp_dir().join(format!(
        "electricity-compiler-depth-safety-{}.yml",
        std::process::id()
    ));
    std::fs::write(&path, nested_dynamic_effects(255)).expect("write temp document");

    let result = std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(move || {
            let document = load_document(&path).expect("deeply nested document must load");
            let errors = structural_errors(&document);
            let report = check_report(&path, &CheckOptions::default());
            (errors.len(), report.ok, path)
        })
        .expect("spawning the probe thread")
        .join()
        .expect("the probe thread must not panic (never a stack overflow)");

    let (_error_count, _ok, path) = result;
    let _ = std::fs::remove_file(&path);
}
