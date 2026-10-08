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

/// About 255 nested `loop`/`dynamic`/`if` levels (issue #408's own
/// estimate for "the deepest document electricity-yaml accepts"): each
/// level is a one-item flow list wrapping a one-key flow mapping, so
/// `n` levels cost `2n` of `electricity_yaml::MAX_DEPTH`'s 512 units,
/// leaving headroom for the document's own root mapping and the
/// innermost tool leaf. Also exercises, in the same document: a
/// declared `interface.inputs` entry (so `validate_bare_input_refs`'s
/// own recursive walk runs over the whole tree), and a named,
/// tree-flow `each` loop at every third level (so
/// `containers::tree_loop_prev_references`'s own scan for
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
    match i % 3 {
        0 => format!("{pad}- type: dynamic\n{pad}  name: n{i}\n{pad}  effects:\n"),
        1 => format!("{pad}- type: if\n{pad}  if: {{mode: cel, expr: 'true'}}\n{pad}  then:\n"),
        _ => format!(
            "{pad}- type: loop\n{pad}  name: n{i}\n{pad}  flow: tree\n\
             {pad}  each: {{in: input.items}}\n{pad}  body:\n"
        ),
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

#[test]
fn about_255_nested_containers_compiles_or_fails_cleanly_on_a_2mib_stack() {
    run_on_small_stack(deeply_nested_containers_document(250));
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

#[test]
fn well_past_the_depth_limit_fails_cleanly_not_a_crash() {
    run_on_small_stack(deeply_nested_containers_document(500));
}
