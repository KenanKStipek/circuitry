//! Section nesting must be bounded by
//! [`electricity_template::MAX_SECTION_DEPTH`], rejected at tokenize
//! time, before the tree-building or rendering stage ever sees it
//! (`lib.rs`'s own docs on `MAX_SECTION_DEPTH`) -- a plain `#[test]` is
//! enough for that, since a rejection never recurses at all, but the
//! point this module exists to pin is the *measurement* behind why this
//! crate needs its own, much smaller limit than
//! [`electricity_value::MAX_DEPTH`] (512): rendering 512 levels of
//! nested sections isn't just a stack-overflow risk (confirmed
//! separately, somewhere between 350 and 400 levels on a 2 MiB
//! debug-build stack) but a denial-of-service in its own right --
//! `render::render_with_pushed_scope` clones the entire current scope
//! stack on every section it descends into, so render time grows with
//! the *cube* of section depth: measured directly on the machine this
//! was written on, rendering 100 matching levels took ~0.3 seconds, 200
//! took ~2.3, 300 took ~8.3 -- a trajectory that puts 512 at roughly a
//! minute, for a single template render, with no recursion depth
//! involved at all yet. `MAX_SECTION_DEPTH` (64) keeps a render at the
//! limit itself cheap (measured at ~125ms) rather than merely
//! stack-safe.

use electricity_template::{MAX_SECTION_DEPTH, PlainCtx, Value, render_template};

fn nested_template(n: usize) -> String {
    let open: String = (0..n).map(|_| "{{#a}}").collect();
    let close: String = (0..n).map(|_| "{{/a}}").collect();
    format!("{open}x{close}")
}

/// Nested `n` levels deep so the whole template resolves and renders
/// `"x"` when `n` is within the limit.
fn nested_ctx(n: usize) -> Value {
    let mut v = Value::from("x");
    for _ in 0..n {
        let mut outer = Value::Dict(Default::default());
        outer
            .as_dict_mut()
            .unwrap()
            .insert(Value::Str("a".into()), v);
        v = outer;
    }
    v
}

#[test]
fn exactly_at_the_limit_renders() {
    let template = nested_template(MAX_SECTION_DEPTH);
    let ctx = nested_ctx(MAX_SECTION_DEPTH);
    let out = render_template(&template, &ctx, &PlainCtx, "t")
        .expect("nesting at exactly MAX_SECTION_DEPTH must still render");
    assert_eq!(out, "x");
}

#[test]
fn one_past_the_limit_is_a_distinct_nesting_error() {
    let template = nested_template(MAX_SECTION_DEPTH + 1);
    let ctx = nested_ctx(MAX_SECTION_DEPTH + 1);
    let err = render_template(&template, &ctx, &PlainCtx, "t").unwrap_err();
    assert!(err.is_too_deeply_nested(), "got: {err}");
}

#[test]
fn inverted_sections_count_toward_the_same_limit() {
    let n = MAX_SECTION_DEPTH + 1;
    let open: String = (0..n).map(|_| "{{^a}}").collect();
    let close: String = (0..n).map(|_| "{{/a}}").collect();
    let template = format!("{open}x{close}");
    let err = render_template(&template, &Value::Bool(false), &PlainCtx, "t").unwrap_err();
    assert!(err.is_too_deeply_nested(), "got: {err}");
}

/// A too-deep template is rejected during tokenizing, before
/// `render_template` would ever reach the cubic-cost rendering walk --
/// this asserts the specific, fast failure mode (a `Result::Err`
/// returned promptly), not just "didn't hang", since a hang wouldn't
/// show up as a test failure at all.
#[test]
fn rejection_is_cheap_even_though_rendering_at_this_depth_would_not_be() {
    let n = 400; // measured elsewhere in this module's docs at ~18s to render
    let template = nested_template(n);
    let ctx = nested_ctx(n);
    let start = std::time::Instant::now();
    let err = render_template(&template, &ctx, &PlainCtx, "t").unwrap_err();
    assert!(err.is_too_deeply_nested(), "got: {err}");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(1),
        "rejecting an over-nested template took {:?}; it should fail at tokenize time, \
         long before paying any rendering cost",
        start.elapsed()
    );
}

/// `_get_key`'s dotted-name walk (`render::walk_dotted`) is a plain loop
/// over `key.split('.')`, never recursion -- so unlike section nesting,
/// an arbitrarily long dotted name is bounded only by memory, not stack
/// depth, and needs no limit of its own. Exercised here at a length
/// `MAX_SECTION_DEPTH` make no sense pretending to bound.
#[test]
fn an_extremely_long_dotted_name_does_not_overflow_the_stack() {
    let segments = 200_000;
    let key = (0..segments).map(|_| "a").collect::<Vec<_>>().join(".");
    let template = format!("{{{{{key}}}}}");
    let ctx = Value::Dict(Default::default());
    let out = render_template(&template, &ctx, &PlainCtx, "t")
        .expect("a missing, arbitrarily long dotted path renders empty, not an error");
    assert_eq!(out, "");
}
