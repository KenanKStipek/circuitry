//! Finding 1 (review of PR #415): the depth acceptance criterion
//! (issue #408's own list) was untested -- "on a 2 MiB thread stack,
//! the deepest nesting `electricity-yaml` accepts either compiles or
//! is rejected with a distinct depth error, never a stack overflow."
//!
//! Every probe here runs [`compile_document`], then
//! [`groups::unknown_group_errors`], [`cycles::detect_cycles`], a
//! `serde_json` dump of the resulting [`electricity_bytecode::Program`]
//! (the same serialization `--dump-ir`/the IR snapshot tests use), and
//! finally the `Program`'s own drop -- all on an explicit 2 MiB-stack
//! thread, the way `electricity-yaml`'s own `nesting_limit.rs` probes
//! its composer. The input is parsed by `electricity_yaml::load_yaml`
//! first (on the same small-stack thread), so a document past
//! `electricity_yaml::MAX_DEPTH` (512) is rejected there, before
//! `compile_document` ever sees it -- this module's own documents stay
//! at or under that limit, "the deepest nesting electricity-yaml
//! accepts".

use electricity_compiler::{DocumentOrigin, compile_document, cycles, groups};
use std::collections::BTreeSet;
use std::path::PathBuf;

const SMALL_STACK: usize = 2 * 1024 * 1024;

fn origin() -> DocumentOrigin {
    DocumentOrigin::File {
        document_dir: PathBuf::from("/doc"),
        confinement_root: PathBuf::from("/doc"),
    }
}

/// Parses *text*, compiles it, runs the two post-compile checks
/// (`pipeline.rs`'s own order), dumps the result through `serde_json`,
/// and drops everything -- all on the probe thread. Panics (a thread
/// `join` failure, never a silent pass) only on a stack overflow; a
/// clean compile error is an accepted outcome, logged for visibility.
/// Only for probes that are not themselves pinning which side of
/// `MAX_COMPILE_DEPTH` the input falls on -- see the dedicated
/// container-depth probes below for those.
fn run_on_small_stack(text: String) {
    std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(move || {
            let document = match electricity_yaml::load_yaml(&text) {
                Ok(document) => document,
                // Past electricity-yaml's own MAX_DEPTH (512): still a
                // clean, distinct error, never a crash -- already
                // covered by electricity-yaml's own nesting_limit.rs,
                // but tolerated here too rather than failing a probe
                // that pushes past both limits at once.
                Err(_) => return,
            };
            match compile_document(&document, &origin()) {
                Ok(program) => {
                    let _ = groups::unknown_group_errors(&program, &BTreeSet::new());
                    let _ = cycles::detect_cycles(&document, Some(&PathBuf::from("/doc/root.yml")));
                    let dumped = serde_json::to_string(&program).expect("serde dump");
                    assert!(!dumped.is_empty());
                    drop(program);
                }
                Err(err) => {
                    // A distinct, well-formed compile error is an
                    // accepted outcome too -- the one thing that must
                    // never happen is a stack overflow (the `join`
                    // below catches that).
                    assert!(!err.0.is_empty());
                }
            }
        })
        .expect("spawning the probe thread")
        .join()
        .expect("the probe thread must not panic (never a stack overflow)");
}

/// `n` levels of nested `dynamic`/`if`/`loop`/`reflector` containers,
/// rotating through all four so no single probe exercises only one
/// container kind: each level is a one-item flow list wrapping a
/// one-key flow mapping, so `n` levels cost `2n` of
/// `electricity_yaml::MAX_DEPTH`'s 512 units, leaving headroom for the
/// document's own root mapping and the innermost tool leaf. Also
/// exercises, in the same document: a declared `interface.inputs`
/// entry (so `validate_bare_input_refs`'s own recursive walk runs over
/// the whole tree), and a named, tree-flow `each` loop at every fourth
/// level (so `containers::tree_loop_prev_references`'s own scan for
/// `prime.<loop>.prev` runs against a body nested hundreds of levels
/// deep, finding nothing, every time).
// Block-style YAML (indentation), not flow-style (`{...}`/`[...]`):
// saphyr-parser 0.1.0's scanner tracks flow-collection nesting in its
// own `u8` counter (`flow_level`), overflowing well under 255 effect
// levels -- a lower, pre-existing scanner limit `electricity-yaml`'s
// own `nesting_limit.rs` documents, distinct from
// `electricity_yaml::MAX_DEPTH` (512) and not what this probe is
// about. Block style has no such counter, so it reaches the actual
// depth limit this test means to probe.
fn nested_container_level(pad: &str, i: usize) -> String {
    match i % 4 {
        0 => format!("{pad}- type: dynamic\n{pad}  name: n{i}\n{pad}  effects:\n"),
        1 => format!("{pad}- type: if\n{pad}  if: {{mode: cel, expr: 'true'}}\n{pad}  then:\n"),
        2 => format!(
            "{pad}- type: loop\n{pad}  name: n{i}\n{pad}  flow: tree\n\
             {pad}  each: {{in: input.items}}\n{pad}  body:\n"
        ),
        _ => format!("{pad}- type: reflector\n{pad}  name: n{i}\n{pad}  effects:\n"),
    }
}

fn deeply_nested_containers_document(levels: usize) -> String {
    let mut out = String::new();
    out.push_str("interface:\n  inputs:\n    topic: {type: string}\n");
    out.push_str("effects:\n");
    let mut indent = 2usize;
    for i in 0..levels {
        let pad = " ".repeat(indent);
        out.push_str(&nested_container_level(&pad, i));
        indent += 4;
    }
    let leaf_pad = " ".repeat(indent);
    out.push_str(&format!(
        "{leaf_pad}- type: tool\n{leaf_pad}  name: leaf\n{leaf_pad}  provider: shell\n\
         {leaf_pad}  prompt: 'about {{{{input.topic}}}}'\n"
    ));
    out
}

/// Exactly `MAX_COMPILE_DEPTH - 1` (127) mixed containers -- precisely
/// the depth `compile::containers::depth_tests::
/// exactly_at_the_depth_limit_compiles` pins as the last depth that
/// must still compile. Unlike [`run_on_small_stack`], this probe does
/// not accept a compile error as a pass: it asserts the SUCCESS path
/// runs in full (`compile_document`, `groups::unknown_group_errors`,
/// `cycles::detect_cycles`, the serde dump, and the `Program`'s own
/// drop) on the small-stack thread, so a regression that starts
/// failing early -- or one that only looks fine because this suite
/// always accepted either outcome -- would actually be caught.
#[test]
fn exactly_127_mixed_containers_compiles_and_runs_the_full_pipeline_on_a_2mib_stack() {
    let text = deeply_nested_containers_document(127);
    std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(move || {
            let document = electricity_yaml::load_yaml(&text)
                .expect("127 mixed containers stays within electricity_yaml::MAX_DEPTH");
            let program = compile_document(&document, &origin()).unwrap_or_else(|err| {
                panic!(
                    "expected the success path at exactly 127 mixed containers, got: {}",
                    err.0
                )
            });
            let _ = groups::unknown_group_errors(&program, &BTreeSet::new());
            let _ = cycles::detect_cycles(&document, Some(&PathBuf::from("/doc/root.yml")));
            let dumped = serde_json::to_string(&program).expect("serde dump");
            assert!(!dumped.is_empty());
            drop(program);
        })
        .expect("spawning the probe thread")
        .join()
        .expect("the probe thread must not panic (never a stack overflow)");
}

/// About 250 mixed containers -- well past `MAX_COMPILE_DEPTH` (128),
/// but still at or under `electricity_yaml::MAX_DEPTH` (512), so
/// `load_yaml` must succeed and `compile_document` must fail with the
/// compiler's own distinct depth error -- not a generic "any error is
/// fine" check, and not a stack overflow.
#[test]
fn about_250_nested_containers_is_rejected_by_the_compilers_own_depth_error_on_a_2mib_stack() {
    let text = deeply_nested_containers_document(250);
    std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(move || {
            let document = electricity_yaml::load_yaml(&text)
                .expect("250 mixed containers stays within electricity_yaml::MAX_DEPTH");
            let err = compile_document(&document, &origin()).unwrap_err();
            assert!(
                err.0.contains("effect nesting is too deep"),
                "unexpected error: {}",
                err.0
            );
        })
        .expect("spawning the probe thread")
        .join()
        .expect("the probe thread must not panic (never a stack overflow)");
}

/// A tool `params` value about 510 levels deep -- the other half of
/// issue #408's own estimate, exercised independently of container
/// nesting so each probe stays within `electricity_yaml::MAX_DEPTH`
/// (512) on its own: `compile::params::build_param_node`'s own
/// recursion (and the `ParamNode` tree it builds) is the thing under
/// test here, not `containers::compile_effect`'s.
fn deep_params_value(depth: usize, base_indent: usize) -> String {
    let mut out = String::new();
    for d in 0..depth {
        let pad = " ".repeat(base_indent + d * 2);
        out.push_str(&format!("{pad}a:\n"));
    }
    let pad = " ".repeat(base_indent + depth * 2);
    out.push_str(&format!("{pad}1\n"));
    out
}

fn deep_tool_params_document(depth: usize) -> String {
    let mut out = String::new();
    out.push_str("effects:\n  - type: tool\n    name: leaf\n    provider: shell\n    params:\n");
    out.push_str(&deep_params_value(depth, 6));
    out
}

#[test]
fn a_tool_params_value_about_510_levels_deep_compiles_or_fails_cleanly_on_a_2mib_stack() {
    run_on_small_stack(deep_tool_params_document(505));
}

/// 500 mixed containers is about 1000 of `electricity_yaml::MAX_DEPTH`'s
/// 512 units -- past electricity-yaml's own structural depth limit, so
/// `load_yaml` rejects it before `compile_document` ever runs. This
/// probe is about that outer YAML limit (already covered independently
/// by `electricity-yaml`'s own `nesting_limit.rs`), not the compiler's
/// `MAX_COMPILE_DEPTH` -- see the 127/250-level probes above for that.
#[test]
fn far_past_electricity_yamls_own_depth_limit_is_rejected_there_before_compiling() {
    let text = deeply_nested_containers_document(500);
    std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(move || {
            let err = electricity_yaml::load_yaml(&text).unwrap_err();
            assert!(!err.to_string().is_empty());
        })
        .expect("spawning the probe thread")
        .join()
        .expect("the probe thread must not panic (never a stack overflow)");
}
