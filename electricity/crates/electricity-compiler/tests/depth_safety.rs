//! On a 2 MiB thread stack, loading and checking the deepest nesting
//! `electricity-yaml` accepts must never overflow (issue #408's lane B
//! acceptance criterion) -- the same standard `electricity-yaml`'s own
//! `tests/nesting_limit.rs` holds itself to, applied here to
//! [`load_document`]/[`structural_errors`]/[`check_report`]'s own
//! recursive walks (the near-miss unknown-key walk, the `group:`-
//! placement walk, and the `Value` -> JSON-Schema-instance conversion),
//! across every shape that walk recurses through: nested *containers*
//! (`dynamic.effects`, `if.then`, `loop.body`) and a deep *list* under a
//! leaf's own scalar-bearing field (`tool.params`, and `interface.
//! inputs.<k>.default`, which `_unquote_hint`/[`crate::structural`]'s
//! own default-type check runs `py_repr()` over). `opt-level = 2` for
//! the dev profile (this workspace's `Cargo.toml`) is the lever that
//! keeps every one of these within a 2 MiB stack; this test is only
//! meaningful in a profile that applies it -- release already builds at
//! `opt-level = 3`, strictly more aggressive, so it is not exercised
//! separately here.

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

/// `n` levels of a single-child `if` effect nested under the previous
/// one's own `then:` list -- the same two-units-per-level cost as
/// [`nested_dynamic_effects`], through a different `CHILD_KEYS` branch
/// (`then` rather than `effects`).
fn nested_if_then(n: usize) -> String {
    let mut text = String::from("effects:\n");
    for i in 0..n {
        let indent = "  ".repeat(i + 1);
        text.push_str(&format!("{indent}- type: if\n"));
        text.push_str(&format!("{indent}  name: c{i}\n"));
        text.push_str(&format!("{indent}  if: {{mode: cel, expr: \"true\"}}\n"));
        text.push_str(&format!("{indent}  then:\n"));
    }
    text
}

/// `n` levels of a single-child `loop` effect nested under the previous
/// one's own `body:` list -- through `CHILD_KEYS`' `body` branch.
fn nested_loop_body(n: usize) -> String {
    let mut text = String::from("effects:\n");
    for i in 0..n {
        let indent = "  ".repeat(i + 1);
        text.push_str(&format!("{indent}- type: loop\n"));
        text.push_str(&format!("{indent}  name: l{i}\n"));
        text.push_str(&format!("{indent}  while: {{mode: cel, expr: \"true\"}}\n"));
        text.push_str(&format!("{indent}  body:\n"));
    }
    text
}

/// A single tool effect whose `params:` is a list nested `n` deep --
/// walked by [`crate::schema_instance::to_schema_instance`]'s own
/// recursive `Value` -> JSON-Schema-instance conversion, not the
/// container walk the other three shapes exercise.
fn deeply_nested_list_under_params(n: usize) -> String {
    let mut text =
        String::from("effects:\n  - type: tool\n    name: t\n    provider: json\n    params:\n");
    for i in 0..n {
        let indent = "  ".repeat(i + 3);
        text.push_str(&format!("{indent}- \n"));
    }
    text
}

/// A declared `interface.inputs.x.default` that is a list nested `n`
/// deep -- reaches `structural.rs`'s `interface_default_type_errors`,
/// whose mismatch message runs `Value::py_repr()` (itself recursive)
/// over the default value.
fn deeply_nested_list_under_interface_default(n: usize) -> String {
    let mut text =
        String::from("interface:\n  inputs:\n    x:\n      type: integer\n      default:\n");
    for i in 0..n {
        let indent = "  ".repeat(i + 3);
        text.push_str(&format!("{indent}- \n"));
    }
    text.push_str("effects: []\n");
    text
}

/// Writes *text* to a fresh temp file, loads and structurally checks it
/// on a 2 MiB-stack thread, and returns `structural_errors`'s own count
/// -- panicking (via the probe thread's `join()`) on a stack overflow,
/// exactly like [`nested_dynamic_effects`]'s original single-shape
/// test, but now shared across every shape below.
fn check_on_a_small_stack(name: &str, text: String) -> usize {
    let path = std::env::temp_dir().join(format!(
        "electricity-compiler-depth-safety-{name}-{}.yml",
        std::process::id()
    ));
    std::fs::write(&path, text).expect("write temp document");

    let result = std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(move || {
            let document = load_document(&path).expect("deeply nested document must load");
            let errors = structural_errors(&document);
            let report = check_report(&path, &CheckOptions::default());
            (errors.len(), report.warnings.len(), path)
        })
        .expect("spawning the probe thread")
        .join()
        .expect("the probe thread must not panic (never a stack overflow)");

    let (error_count, _warning_count, path) = result;
    let _ = std::fs::remove_file(&path);
    error_count
}

// Each of the three container shapes below ends in an empty, unfilled
// `effects:`/`then:`/`body:` key at its innermost level (the loop that
// builds the text writes the key, then the *next* iteration would have
// filled it with a list, but there is no next iteration at the bottom) --
// YAML reads that as `null`, which the schema's own `"type": "array"`
// constraint on that key rejects, so exactly one ordinary schema error is
// the correct, expected outcome, not an artifact of the depth itself.

#[test]
fn deepest_accepted_nesting_does_not_overflow_a_2mib_thread() {
    assert_eq!(
        check_on_a_small_stack("dynamic-effects", nested_dynamic_effects(255)),
        1,
        "255 levels of nested dynamic.effects, with the innermost effects: left null"
    );
}

#[test]
fn deepest_accepted_if_then_nesting_does_not_overflow_a_2mib_thread() {
    assert_eq!(
        check_on_a_small_stack("if-then", nested_if_then(255)),
        1,
        "255 levels of nested if.then, with the innermost then: left null"
    );
}

#[test]
fn deepest_accepted_loop_body_nesting_does_not_overflow_a_2mib_thread() {
    assert_eq!(
        check_on_a_small_stack("loop-body", nested_loop_body(255)),
        1,
        "255 levels of nested loop.body, with the innermost body: left null"
    );
}

#[test]
fn deepest_accepted_list_under_params_does_not_overflow_a_2mib_thread() {
    // `null` at the bottom of a 511-deep list is itself schema-valid
    // against `params`'s own `"type": "object"` constraint being
    // irrelevant here -- a list fails that regardless of depth, so this
    // is checking the walk survives, not that it reports zero errors;
    // the one schema error at `effects[0].params` is exactly what's
    // expected, not an unrelated depth artifact.
    assert_eq!(
        check_on_a_small_stack("params-list", deeply_nested_list_under_params(505)),
        1,
        "a deeply nested list under params is a single, ordinary schema-shape error -- not a crash"
    );
}

#[test]
fn deepest_accepted_list_under_interface_default_does_not_overflow_a_2mib_thread() {
    assert_eq!(
        check_on_a_small_stack(
            "interface-default-list",
            deeply_nested_list_under_interface_default(505)
        ),
        1,
        "a deeply nested list default for a declared `integer` input is a single type-mismatch \
         error (py_repr's own recursion survives) -- not a crash"
    );
}
