//! `report` effect execution through an isolated child run -- the same
//! path a decomposition/reflector plan's own effects run through
//! (`decompose.rs::run_isolated`'s own caller).

mod support;

use electricity_bytecode::{
    EffectPath, Escape, LeafKind, NodeKind, OnError, Op, ReportLevel, ReportOp, TemplateText,
};
use electricity_value::Value;
use electricity_vm::exec::use_::run_isolated;
use electricity_vm::{CancellationToken, Frame, Limiter};
use support::{Event, RecordingObserver, json_registry, run_ctx};

fn root_path() -> EffectPath {
    EffectPath::root()
}

fn report_leaf(path: EffectPath, name: Option<&str>, message: &str) -> Op {
    Op {
        path,
        name: name.map(str::to_string),
        kind: NodeKind::Leaf(Box::new(LeafKind::Report(ReportOp {
            message: TemplateText::new(message.to_string(), true, Escape::None),
            data: None,
            level: ReportLevel::Info,
            description: None,
        }))),
        on_error: OnError::Fail,
        labels: None,
        enabled: true,
    }
}

/// `decompose.rs::child_frame`'s own namespacing (`frame.nested(&op.path)`,
/// *op.path* being the decomposing node's own path in the parent
/// document) -- a plan's `report` must reach the parent's own
/// `RunObserver::report`, with its path prefixed by the decomposing
/// node's, the same way `core/decompose.py::_namespaced_report_cb`
/// namespaces an unnamed report under the decomposing node and a named
/// one under `<node>.<name>`.
#[tokio::test]
async fn a_decomposition_plans_report_reaches_the_parent_observer_namespaced() {
    let plan_root = support::chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![
            report_leaf(root_path().push_name("status"), Some("status"), "named"),
            // An unnamed leaf's own compiled path is its parent's,
            // unchanged (`electricity_compiler::compile::leaves::
            // compile_report`'s own `own_path` for `validated_name:
            // None`) -- the root dynamic's path here, not a `"peek"`
            // segment of its own.
            report_leaf(root_path(), None, "unnamed"),
        ],
    );
    let plan_program = support::program(plan_root);

    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new();

    // The decomposing node's own path in the *parent* document -- the
    // same value `decompose.rs` passes as `&op.path` to `child_frame`.
    let decomposing_node_path = root_path().push_name("decomposed");
    let plan_frame = Frame::root().nested(&decomposing_node_path);

    run_isolated(
        &plan_program,
        Value::None,
        &plan_frame,
        &ctx,
        &observer,
        &token,
    )
    .await
    .expect("an isolated plan with two reports must succeed");

    let reports: Vec<(String, String, String)> = observer
        .events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Report(path, level, message, _data) => Some((path, level, message)),
            _ => None,
        })
        .collect();

    // A named report lands at `<decomposing node>.<name>`; an unnamed
    // one falls back to the container it ran in -- here, the plan's
    // own root dynamic, which is itself the decomposing node's path
    // (an unnamed root dynamic contributes no path segment of its own).
    assert_eq!(
        reports,
        vec![
            (
                "prime.decomposed.status".to_string(),
                "info".to_string(),
                "named".to_string()
            ),
            (
                "prime.decomposed".to_string(),
                "info".to_string(),
                "unnamed".to_string()
            ),
        ]
    );
}
