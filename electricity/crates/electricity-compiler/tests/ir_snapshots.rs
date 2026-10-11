//! IR snapshot tests (issue #408's Test strategy section): one per
//! effect type and per container combination, snapshotted as the
//! library serializer's own JSON (the same `serde_json::to_string_
//! pretty(&program)` shape `electricity::dump_ir` wraps in its
//! `{"ir_version": ..., "program": ...}` envelope). These pin the IR's
//! own shape, which a hand-written synthetic document round-tripped
//! through Circuitry's reference compiler can't express on its own:
//! path placeholders (a named loop's [`electricity_bytecode::path::
//! PathSegment::Pass`]), the `overlay` flag per container, and the
//! escape mode ([`electricity_bytecode::Escape`]) per templated field.
//!
//! Each snapshot is hand-written synthetic YAML, not generated from
//! Circuitry's own code -- there is no Python reference for this
//! shape (`electricity-compiler`'s `Program` has no Circuitry
//! counterpart; `golden_compile.rs`'s own projection already pins the
//! *dataclass*-shaped half of the same tree). Regenerate with
//! `WRITE_IR_SNAPSHOTS=1 cargo test -p electricity-compiler --test
//! ir_snapshots` after a deliberate IR change, then review the diff.

use electricity_compiler::{DocumentOrigin, compile_document};
use std::path::{Path, PathBuf};

fn origin() -> DocumentOrigin {
    DocumentOrigin::File {
        document_dir: PathBuf::from("/doc"),
        confinement_root: PathBuf::from("/doc"),
    }
}

fn snapshot_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/ir_snapshots/{name}.json"))
}

fn assert_snapshot(name: &str, document_yaml: &str) {
    let document =
        electricity_yaml::load_yaml(document_yaml).expect("snapshot fixture parses as YAML");
    let program = compile_document(&document, &origin()).unwrap_or_else(|err| {
        panic!(
            "snapshot {name:?}: expected a successful compile, got: {}",
            err.0
        )
    });
    let actual: serde_json::Value =
        serde_json::from_str(&serde_json::to_string_pretty(&program).unwrap()).unwrap();

    let path = snapshot_path(name);
    if std::env::var("WRITE_IR_SNAPSHOTS").is_ok() {
        std::fs::write(&path, serde_json::to_string_pretty(&actual).unwrap() + "\n")
            .unwrap_or_else(|err| panic!("writing snapshot {path:?}: {err}"));
        return;
    }

    let expected_text = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "snapshot {name:?}: could not read {path:?} ({err}) -- run with \
             WRITE_IR_SNAPSHOTS=1 to create it"
        )
    });
    let expected: serde_json::Value =
        serde_json::from_str(&expected_text).expect("snapshot file is valid JSON");
    assert_eq!(
        actual, expected,
        "snapshot {name:?} changed -- if deliberate, rerun with WRITE_IR_SNAPSHOTS=1 \
         and review the diff"
    );
}

#[test]
fn prompt_with_template() {
    assert_snapshot(
        "prompt_with_template",
        "effects:\n  - type: prompt\n    name: greet\n    template: 'hi {{input.name}}'\n",
    );
}

#[test]
fn prompt_with_messages() {
    assert_snapshot(
        "prompt_with_messages",
        "effects:\n  - type: prompt\n    name: chat\n    messages:\n      - role: system\n        content: sys\n      - role: user\n        content: 'hi {{input.name}}'\n",
    );
}

#[test]
fn prompt_inputs_and_params_are_raw_literals() {
    // Issue #449's gate lane hazard: Python's `_compile_prompt` never
    // template-checks or Mustache-renders `inputs:`/`params:` -- both
    // merge into the run-time context/adapter call as the raw dict,
    // `{{...}}`-looking text included. Pin that the compiler emits
    // `ParamNode::Literal` for both fields here, not `ParamNode::
    // Template`, so a later lane can't silently turn them into
    // template fields without this snapshot's own diff catching it.
    assert_snapshot(
        "prompt_inputs_and_params_are_raw_literals",
        "effects:\n  - type: prompt\n    name: ask\n    template: hi\n    inputs:\n      example: '{{literal, not rendered}}'\n    params:\n      temperature: 0.5\n      note: '{{also literal}}'\n",
    );
}

#[test]
fn tool_leaf() {
    assert_snapshot(
        "tool_leaf",
        "effects:\n  - type: tool\n    name: fetch\n    provider: shell\n    params:\n      cmd: 'echo {{input.x}}'\n      n: {from: 'input.n'}\n",
    );
}

#[test]
fn use_path_leaf() {
    assert_snapshot(
        "use_path_leaf",
        "effects:\n  - type: use\n    name: sub\n    path: child.yml\n    inputs:\n      topic: '{{input.topic}}'\n",
    );
}

#[test]
fn use_inline_leaf() {
    assert_snapshot(
        "use_inline_leaf",
        "effects:\n  - type: use\n    name: sub\n    inline: 'effects: []'\n",
    );
}

#[test]
fn yield_leaf() {
    assert_snapshot(
        "yield_leaf",
        "effects:\n  - type: yield\n    name: out\n    template: 'value {{input.n}}'\n",
    );
}

#[test]
fn reflector_leaf() {
    assert_snapshot(
        "reflector_leaf",
        "effects:\n  - type: reflector\n    name: think\n    effects:\n      - type: prompt\n        name: propose_steps\n        template: p\n",
    );
}

#[test]
fn dynamic_chain_with_finally() {
    assert_snapshot(
        "dynamic_chain_with_finally",
        "effects:\n  - type: dynamic\n    name: group\n    effects:\n      - type: prompt\n        name: a\n        template: a\n    finally:\n      - type: prompt\n        name: cleanup\n        template: done\n",
    );
}

#[test]
fn dynamic_tree() {
    assert_snapshot(
        "dynamic_tree",
        "effects:\n  - type: dynamic\n    name: group\n    flow: tree\n    max_concurrency: 2\n    stop_on_error: true\n    effects:\n      - type: prompt\n        name: a\n        template: a\n      - type: prompt\n        name: b\n        template: b\n",
    );
}

#[test]
fn conditional_cel_with_else() {
    assert_snapshot(
        "conditional_cel_with_else",
        "effects:\n  - type: if\n    name: branch\n    if:\n      mode: cel\n      expr: 'state.input.ok == true'\n    then:\n      - type: prompt\n        name: yes_branch\n        template: y\n    else:\n      - type: prompt\n        name: no_branch\n        template: n\n",
    );
}

#[test]
fn conditional_model_without_else() {
    assert_snapshot(
        "conditional_model_without_else",
        "effects:\n  - type: if\n    if:\n      template: 'well?'\n    then:\n      - type: prompt\n        name: yes_branch\n        template: y\n",
    );
}

#[test]
fn loop_each_chain_with_collect() {
    assert_snapshot(
        "loop_each_chain_with_collect",
        "effects:\n  - type: loop\n    name: iterate\n    each:\n      in: input.items\n      as: item\n    collect: handle\n    body:\n      - type: prompt\n        name: handle\n        template: 'item {{item}}'\n",
    );
}

#[test]
fn loop_while() {
    assert_snapshot(
        "loop_while",
        "effects:\n  - type: loop\n    name: poll\n    while:\n      mode: cel\n      expr: 'state.iter.index < 3'\n    body:\n      - type: prompt\n        name: tick\n        template: t\n",
    );
}

#[test]
fn loop_each_tree() {
    assert_snapshot(
        "loop_each_tree",
        "effects:\n  - type: loop\n    name: parallel_iterate\n    flow: tree\n    each:\n      in: input.items\n    body:\n      - type: prompt\n        name: handle\n        template: 'item {{item}}'\n",
    );
}

#[test]
fn unnamed_loop_is_transparent_in_the_path() {
    assert_snapshot(
        "unnamed_loop_is_transparent",
        "effects:\n  - type: loop\n    each:\n      in: input.items\n    body:\n      - type: prompt\n        name: handle\n        template: 'item {{item}}'\n",
    );
}

#[test]
fn nested_named_loop_inside_a_dynamic() {
    assert_snapshot(
        "nested_named_loop_inside_a_dynamic",
        "effects:\n  - type: dynamic\n    name: group\n    effects:\n      - type: loop\n        name: inner_loop\n        each:\n          in: input.items\n        body:\n          - type: prompt\n            name: handle\n            template: 'item {{item}}'\n",
    );
}
