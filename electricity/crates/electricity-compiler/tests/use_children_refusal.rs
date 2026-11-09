//! Proves the seam `use_children::iter_use_path_children` exposes (M1
//! lane I prerequisites, issue #448, item U4): walking a document's own
//! literal `use: {path:}` children, compiling each one, and checking it
//! against `electricity_bytecode::first_unsupported` and
//! `electricity_config::orchestration_denials` -- the same
//! two checks the parent document already got, now run against a child
//! no static walk over the *parent* alone could ever see. Wiring either
//! check into a real run (so a `use` effect whose own child needs this
//! is actually refused before the run reaches it) stays lane I's own
//! job; this only proves the walk feeds both correctly.
//!
//! M0 behaviour is unchanged either way: a document with a `use` effect
//! at all stays refused outright (`RefusalReason::Use`) while
//! `Supported::use_` is `false`, never reaching either check below.

use electricity_bytecode::{RefusalReason, Supported};
use electricity_compiler::{DocumentOrigin, compile::compile_document, use_children};
use electricity_config::{CircuitryConfig, orchestration_denials};
use std::fs;
use std::path::PathBuf;

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "electricity-use-children-refusal-test-{tag}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A test-only [`Supported`] with `use_` on and nothing else -- M1
/// lane I has not landed, so this is never `Supported::current`'s own
/// value; it only exists to let a test compile a document containing a
/// `use` effect past the walker's own `RefusalReason::Use` check, the
/// same way lane I's own tests (once it lands) will need to.
fn supported_with_use_only() -> Supported<'static> {
    Supported {
        use_: true,
        ..Supported::m0(&["json"])
    }
}

#[test]
fn use_is_still_refused_while_its_own_flag_is_off() {
    let dir = tmp_dir("flag-off");
    let entry = dir.join("root.yaml");
    fs::write(
        &entry,
        "effects:\n  - type: use\n    name: child\n    path: child.yaml\n",
    )
    .unwrap();
    fs::write(dir.join("child.yaml"), "effects: []\n").unwrap();

    let document =
        electricity_yaml::load_yaml_last_key_wins(&fs::read_to_string(&entry).unwrap()).unwrap();
    let origin = DocumentOrigin::File {
        document_dir: dir.clone(),
        confinement_root: dir.clone(),
    };
    let program = compile_document(&document, &origin).unwrap();

    let refusal =
        electricity_bytecode::first_unsupported(&program, false, &Supported::m0(&["json"]))
            .expect("a use effect must still be refused while use_ is false");
    assert_eq!(refusal.reason, RefusalReason::Use);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_loaded_childs_own_unsupported_tool_is_found_before_any_run_reaches_it() {
    let dir = tmp_dir("child-tool");
    let entry = dir.join("root.yaml");
    fs::write(
        &entry,
        "effects:\n  - type: use\n    name: child\n    path: child.yaml\n",
    )
    .unwrap();
    fs::write(
        dir.join("child.yaml"),
        "effects:\n\
         \x20 - type: tool\n\
         \x20   name: run_it\n\
         \x20   provider: shell\n\
         \x20   params: {command: 'echo hi'}\n",
    )
    .unwrap();

    let document =
        electricity_yaml::load_yaml_last_key_wins(&fs::read_to_string(&entry).unwrap()).unwrap();
    let origin = DocumentOrigin::File {
        document_dir: dir.clone(),
        confinement_root: dir.clone(),
    };
    let program = compile_document(&document, &origin).unwrap();

    // The parent alone, with `use_` on, is accepted -- its own `use`
    // effect names no unsupported content of its own.
    assert_eq!(
        electricity_bytecode::first_unsupported(&program, false, &supported_with_use_only()),
        None
    );

    // The walk over the parent's own literal path children finds
    // `child.yaml`, and compiling + checking *it* finds the `shell`
    // tool the parent's own compiled `Program` never contained at all.
    let children = use_children::iter_use_path_children(&document, Some(&entry));
    assert_eq!(children.len(), 1);
    let (label, child_document) = &children[0];
    assert!(label.ends_with("child.yaml"));

    let child_origin = DocumentOrigin::File {
        document_dir: dir.clone(),
        confinement_root: dir.clone(),
    };
    let child_program = compile_document(child_document, &child_origin).unwrap();
    let child_refusal =
        electricity_bytecode::first_unsupported(&child_program, false, &supported_with_use_only())
            .expect("the child's own shell tool must still be refused");
    assert_eq!(
        child_refusal.reason,
        RefusalReason::UnsupportedToolProvider("shell".to_string())
    );

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_loaded_childs_own_disallowed_tool_is_found_by_the_allowlist_walk() {
    let dir = tmp_dir("child-allowlist");
    let entry = dir.join("root.yaml");
    fs::write(
        &entry,
        "effects:\n  - type: use\n    name: child\n    path: child.yaml\n",
    )
    .unwrap();
    fs::write(
        dir.join("child.yaml"),
        "effects:\n\
         \x20 - type: tool\n\
         \x20   name: run_it\n\
         \x20   provider: shell\n\
         \x20   params: {command: 'echo hi'}\n",
    )
    .unwrap();

    let document =
        electricity_yaml::load_yaml_last_key_wins(&fs::read_to_string(&entry).unwrap()).unwrap();
    let config = CircuitryConfig {
        enabled_tools: Some(vec!["json".to_string()]),
        ..CircuitryConfig::default()
    };

    // The parent document's own text names no tool at all -- nothing
    // to deny yet.
    assert!(electricity_config::check_allowlist(&document, &config).is_empty());

    let children = use_children::iter_use_path_children(&document, Some(&entry));
    assert_eq!(children.len(), 1);
    let (_, child_document) = &children[0];

    // `cli/allowlist.py::check_allowlist`'s own per-child call shape:
    // `skip_templated: true, include_document_adapter: false`.
    let denials = orchestration_denials(
        child_document,
        config.enabled_adapters.as_deref(),
        config.enabled_tools.as_deref(),
        true,
        false,
    );
    assert_eq!(denials.len(), 1);
    assert!(denials[0].contains("tool 'shell'"));

    fs::remove_dir_all(&dir).unwrap();
}
