//! Proves `electricity_bytecode::first_unsupported`'s own tool-provider
//! refusal (issue #431's gate lane, orchestrator ruling on PR #432's
//! review: "`first_unsupported` MUST also refuse any `tool` effect
//! whose provider ... is not `json`") works end to end against a real
//! `check_for_run` compile -- not just the unit-level `Program`
//! fixtures `electricity-bytecode`'s own tests build by hand. Lane A
//! ships the walker and this proof; wiring it into the CLI's own
//! output is lane D's (`electricity/docs/spec/vm-lanes.md`).

use electricity_bytecode::RefusalReason;
use electricity_compiler::{CheckOptions, check_for_run};
use std::fs;
use std::path::PathBuf;

fn compile_doc(tag: &str, yaml: &str) -> electricity_bytecode::Program {
    let dir = std::env::temp_dir().join(format!(
        "electricity-refusal-walker-test-{tag}-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    let doc_path: PathBuf = dir.join("doc.yml");
    fs::write(&doc_path, yaml).unwrap();

    let program = check_for_run(&doc_path, &CheckOptions::default())
        .unwrap_or_else(|err| panic!("case {tag:?}: expected check_for_run to succeed: {err}"));

    fs::remove_dir_all(&dir).unwrap();
    program
}

#[test]
fn a_shell_tool_at_the_root_is_refused_with_its_exact_effect_path() {
    let program = compile_doc(
        "shell-root",
        "effects:\n\
         \x20 - type: tool\n\
         \x20   name: run_it\n\
         \x20   provider: shell\n\
         \x20   params: {command: 'echo hi'}\n",
    );
    let refusal = electricity_bytecode::first_unsupported(
        &program,
        &electricity_bytecode::Supported::m0(&["json"]),
    )
    .unwrap();
    assert_eq!(
        refusal.reason,
        RefusalReason::UnsupportedToolProvider("shell".to_string())
    );
    assert_eq!(refusal.path.to_string(), "prime.run_it");
}

#[test]
fn a_shell_tool_nested_inside_a_tree_branch_is_refused_with_its_concrete_path() {
    let program = compile_doc(
        "shell-tree",
        "effects:\n\
         \x20 - type: dynamic\n\
         \x20   name: fan_out\n\
         \x20   flow: tree\n\
         \x20   effects:\n\
         \x20     - type: tool\n\
         \x20       name: left\n\
         \x20       provider: shell\n\
         \x20       params: {command: 'echo hi'}\n",
    );
    let refusal = electricity_bytecode::first_unsupported(
        &program,
        &electricity_bytecode::Supported::m0(&["json"]),
    )
    .unwrap();
    assert_eq!(
        refusal.reason,
        RefusalReason::UnsupportedToolProvider("shell".to_string())
    );
    assert_eq!(refusal.path.to_string(), "prime.fan_out.left");
}

#[test]
fn a_shell_tool_inside_root_finally_is_refused_with_its_concrete_path() {
    let program = compile_doc(
        "shell-finally",
        "effects: []\n\
         finally:\n\
         \x20 - type: tool\n\
         \x20   name: cleanup\n\
         \x20   provider: shell\n\
         \x20   params: {command: 'echo hi'}\n",
    );
    let refusal = electricity_bytecode::first_unsupported(
        &program,
        &electricity_bytecode::Supported::m0(&["json"]),
    )
    .unwrap();
    assert_eq!(
        refusal.reason,
        RefusalReason::UnsupportedToolProvider("shell".to_string())
    );
    // `finally:` shares the body's own scope and path prefix (#431's
    // Scope section), not a `.finally.` segment of its own.
    assert_eq!(refusal.path.to_string(), "prime.cleanup");
}

#[test]
fn a_provider_written_with_whitespace_and_mixed_case_is_still_accepted() {
    let program = compile_doc(
        "json-whitespace",
        "effects:\n\
         \x20 - type: tool\n\
         \x20   name: fetch\n\
         \x20   provider: ' JSON '\n\
         \x20   params: {mode: parse, input: '{}'}\n",
    );
    assert_eq!(
        electricity_bytecode::first_unsupported(
            &program,
            &electricity_bytecode::Supported::m0(&["json"])
        ),
        None
    );
}

#[test]
fn a_test_tools_provider_is_refused_unless_the_caller_lists_it() {
    let program = compile_doc(
        "sleep",
        "effects:\n\
         \x20 - type: tool\n\
         \x20   name: wait\n\
         \x20   provider: sleep\n\
         \x20   params: {ms: 10}\n",
    );
    let refusal = electricity_bytecode::first_unsupported(
        &program,
        &electricity_bytecode::Supported::m0(&["json"]),
    )
    .unwrap();
    assert_eq!(
        refusal.reason,
        RefusalReason::UnsupportedToolProvider("sleep".to_string())
    );
    assert_eq!(
        electricity_bytecode::first_unsupported(
            &program,
            &electricity_bytecode::Supported::m0(&["json", "sleep"])
        ),
        None
    );
}
