//! `electricity run`'s own `--state`/`--resume`/`--force` (issue #727)
//! -- exercised directly against `electricity::run_orchestration`,
//! each case building an "interrupted" state file by running a
//! document once, then editing specific nodes of its own saved state
//! the same way an interrupted process would have left them (an
//! unset `meta.completed_at`, a loop's own `completed_passes` short of
//! every pass), rather than actually racing a signal against a real
//! subprocess.
//!
//! Every document here uses the `uuid` tool (`provider: uuid`, no
//! subprocess, no capability gate) so a resumed effect's own `value`
//! is trivially distinguishable from a skipped one: a fresh run always
//! writes a new id, so an unchanged `value` after `--resume` proves
//! the effect was actually skipped, not merely idempotent.

use electricity::run::RunRequest;
use electricity_value::{Dict, Value};
use electricity_vm::CancellationToken;
use indexmap::IndexMap;
use std::fs;
use std::path::{Path, PathBuf};

/// A fresh, empty directory under the OS temp dir, removed again when
/// this guard drops -- on every exit path, including a failing
/// `assert!` (unlike a plain cleanup call at a test function's own
/// end, which a panic partway through would skip).
struct TempDir(PathBuf);

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(name: &str) -> TempDir {
    let dir = std::env::temp_dir().join(format!(
        "electricity-resume-test-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).unwrap();
    path
}

fn request(config_path: PathBuf, orchestration_path: PathBuf) -> RunRequest {
    RunRequest {
        config_path: Some(config_path),
        orchestration_path,
        inputs: IndexMap::new(),
        out_path: None,
        pretty: false,
        live_state_path: None,
        events_path: None,
        profile_path: None,
        allow_capabilities: Vec::new(),
        config_warnings: Vec::new(),
        adapter_override: None,
        label_override: None,
        armed: true,
        state_path: None,
        resume: false,
        resume_default_out: None,
        force: false,
    }
}

async fn run(req: &RunRequest) -> electricity::RunResult {
    let token = CancellationToken::new();
    electricity::run_orchestration(req, &token).await
}

fn key(s: &str) -> Value {
    Value::Str(s.to_string())
}

/// Walks *path* (dotted, every segment a dict key) into *state*,
/// mutably -- panics with the segment name if any hop isn't a dict.
fn at_mut<'a>(state: &'a mut Value, path: &str) -> &'a mut Value {
    let mut current = state;
    for segment in path.split('.') {
        let dict = current
            .as_dict_mut()
            .unwrap_or_else(|| panic!("{segment}: not a dict"));
        current = dict
            .get_mut(&key(segment))
            .unwrap_or_else(|| panic!("{segment}: missing"));
    }
    current
}

fn set(state: &mut Value, path: &str, value: Value) {
    *at_mut(state, path) = value;
}

fn remove(state: &mut Value, parent_path: &str, child_key: &str) {
    let dict = at_mut(state, parent_path)
        .as_dict_mut()
        .expect("parent is a dict");
    dict.shift_remove(&key(child_key));
}

/// Inserts (or overwrites) *child_key* under *parent_path*, unlike
/// `set` above which requires the full dotted path to already exist --
/// for a key a saved state may never have written at all (an effect
/// whose own node has no `meta.error` key yet because it never failed).
fn insert(state: &mut Value, parent_path: &str, child_key: &str, value: Value) {
    let dict = at_mut(state, parent_path)
        .as_dict_mut()
        .expect("parent is a dict");
    dict.insert(key(child_key), value);
}

fn write_state(dir: &Path, name: &str, state: &Value) -> PathBuf {
    let path = dir.join(name);
    electricity::out::write_out(&path, state, true).unwrap();
    path
}

fn child(v: &Value, k: &str) -> Value {
    v.as_dict()
        .and_then(|d| d.get(&key(k)))
        .cloned()
        .unwrap_or(Value::None)
}

#[tokio::test]
async fn a_named_loop_resumes_at_its_first_unfinished_pass() {
    let dir = temp_dir("loop");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "interface:\n  inputs:\n    items: {default: [0, 1, 2]}\n\
         effects:\n  - name: items\n    type: loop\n    each: {in: input.items, as: x}\n    \
         body:\n      - name: step\n        type: tool\n        provider: uuid\n",
    );

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();

    let saved_pass0 = child(at_mut(&mut state, "prime.items.iter_0"), "step");

    // Simulate an interrupt right after pass 0 finished: the loop's own
    // node never got to record its own completion, but its
    // authoritative `completed_passes` record (written on every pass,
    // not just at the end) still shows the one pass that did.
    set(&mut state, "prime.items.meta.completed_at", Value::None);
    set(
        &mut state,
        "prime.items.meta.completed_passes",
        Value::List(vec![Value::from(0)]),
    );
    remove(&mut state, "prime.items", "iter_1");
    remove(&mut state, "prime.items", "iter_2");

    let state_path = write_state(&dir, "state.json", &state);
    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;

    let second = run(&req).await;
    assert!(second.ok, "{:?}", second.error);
    let mut resumed = second.state.unwrap();

    assert_eq!(
        child(at_mut(&mut resumed, "prime.items.iter_0"), "step"),
        saved_pass0,
        "pass 0 was skipped and reused as-is, never rerun"
    );
    for idx in [1, 2] {
        let pass = at_mut(&mut resumed, &format!("prime.items.iter_{idx}"));
        let step = child(pass, "step");
        assert_ne!(
            step, saved_pass0,
            "pass {idx} ran fresh (absent from the saved state entirely)"
        );
        let meta = child(&step, "meta");
        assert!(child(&meta, "completed_at") != Value::None);
    }
    let completed_passes = child(
        &child(at_mut(&mut resumed, "prime.items"), "meta"),
        "completed_passes",
    );
    assert_eq!(
        completed_passes,
        Value::List(vec![Value::from(0), Value::from(1), Value::from(2)])
    );
}

#[tokio::test]
async fn a_while_loop_resumes_its_unfinished_pass_without_re_evaluating_the_condition() {
    let dir = temp_dir("while");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "interface:\n  inputs:\n    go: {default: true}\n\
         effects:\n  - name: w\n    type: loop\n    max_iterations: 1\n    \
         while: {mode: cel, expr: state.input.go}\n    body:\n      - name: step\n        \
         type: tool\n        provider: uuid\n",
    );

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();

    // Interrupted mid-pass-0: the body itself never finished (unlike
    // `a_named_loop_resumes_at_its_first_unfinished_pass`'s own "pass
    // already done, bookkeeping lagging" shape) -- `completed_passes`
    // is empty, and the pass's own in-flight step is removed outright.
    set(&mut state, "prime.w.meta.completed_at", Value::None);
    set(
        &mut state,
        "prime.w.meta.completed_passes",
        Value::List(vec![]),
    );
    remove(&mut state, "prime.w.iter_0", "step");

    let state_path = write_state(&dir, "state.json", &state);
    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;
    // The condition would now select "stop" if it were ever
    // re-evaluated for the pass this resumes -- it must not be: that
    // pass already held before the interrupt. `max_iterations: 1`
    // means the *next* pass's own condition check (which does
    // re-evaluate) is what actually stops the loop here, not this
    // override on its own.
    req.inputs.insert("go".to_string(), "false".to_string());

    let second = run(&req).await;
    assert!(second.ok, "{:?}", second.error);
    let mut resumed = second.state.unwrap();
    let step = child(at_mut(&mut resumed, "prime.w.iter_0"), "step");
    assert!(
        child(&child(&step, "meta"), "completed_at") != Value::None,
        "pass 0 must still run to completion: {step:?}"
    );
    let completed_passes = child(
        &child(at_mut(&mut resumed, "prime.w"), "meta"),
        "completed_passes",
    );
    assert_eq!(
        completed_passes,
        Value::List(vec![Value::from(0)]),
        "pass 0 completed; no further pass started"
    );
}

#[tokio::test]
async fn a_chain_resumes_at_the_first_effect_that_did_not_finish_whole() {
    let dir = temp_dir("chain-positional");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "effects:\n  - name: a\n    type: tool\n    provider: uuid\n  - name: b\n    \
         type: tool\n    provider: uuid\n  - name: c\n    type: tool\n    provider: uuid\n",
    );

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();
    let saved_a = child(&state, "prime");
    let saved_a = child(&saved_a, "a");

    // b never finished; c (positioned after it in the same chain) was
    // never even reached -- a plain crash mid-chain leaves exactly
    // this shape (a's own node intact, b's own node absent or
    // unfinished, c's own node never created at all).
    remove(&mut state, "prime", "b");
    remove(&mut state, "prime", "c");

    let state_path = write_state(&dir, "state.json", &state);
    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;

    let second = run(&req).await;
    assert!(second.ok, "{:?}", second.error);
    let mut resumed = second.state.unwrap();
    assert_eq!(
        child(at_mut(&mut resumed, "prime"), "a"),
        saved_a,
        "a already finished and comes before the first unfinished effect -- skipped and reused"
    );
    for name in ["b", "c"] {
        let node = child(at_mut(&mut resumed, "prime"), name);
        assert!(
            child(&child(&node, "meta"), "completed_at") != Value::None,
            "{name} must actually run on resume: {node:?}"
        );
    }
}

#[tokio::test]
async fn a_use_without_record_children_reruns_its_child_whole() {
    let dir = temp_dir("use-no-record-children");
    let config = write(&dir, "config.json", "{}");
    let child_doc = write(
        &dir,
        "child.yml",
        "effects:\n  - name: inner\n    type: tool\n    provider: uuid\n",
    );
    let doc = write(
        &dir,
        "doc.yml",
        &format!(
            "effects:\n  - name: step\n    type: use\n    path: {}\n",
            child_doc.display()
        ),
    );

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();
    let saved_inner = child(at_mut(&mut state, "prime.step.inner"), "value");

    // Interrupted mid-child: the `use` node's own meta never completed.
    set(&mut state, "prime.step.meta.completed_at", Value::None);

    let state_path = write_state(&dir, "state.json", &state);
    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;

    let second = run(&req).await;
    assert!(second.ok, "{:?}", second.error);
    let mut resumed = second.state.unwrap();
    let resumed_inner = child(at_mut(&mut resumed, "prime.step.inner"), "value");
    assert_ne!(
        resumed_inner, saved_inner,
        "a use with no record_children always reruns its child whole"
    );
}

#[tokio::test]
async fn a_use_with_record_children_continues_inside_its_matching_child() {
    let dir = temp_dir("use-record-children-continue");
    let config = write(
        &dir,
        "config.json",
        r#"{"runtime": {"state": {"record_children": true}}}"#,
    );
    let child_doc = write(
        &dir,
        "child.yml",
        "effects:\n  - name: inner1\n    type: tool\n    provider: uuid\n  - name: inner2\n    \
         type: tool\n    provider: uuid\n",
    );
    let doc = write(
        &dir,
        "doc.yml",
        &format!(
            "effects:\n  - name: step\n    type: use\n    path: {}\n",
            child_doc.display()
        ),
    );

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();
    let saved_inner1 = child(at_mut(&mut state, "prime.step.inner1"), "value");
    let saved_inner2 = child(at_mut(&mut state, "prime.step.inner2"), "value");

    // Interrupted after the child itself finished (both children
    // recorded under the `use` node, via `record_children`), but
    // before the `use` node's own meta was marked complete.
    set(&mut state, "prime.step.meta.completed_at", Value::None);

    let state_path = write_state(&dir, "state.json", &state);
    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;

    let second = run(&req).await;
    assert!(second.ok, "{:?}", second.error);
    let mut resumed = second.state.unwrap();
    assert_eq!(
        child(at_mut(&mut resumed, "prime.step.inner1"), "value"),
        saved_inner1,
        "record_children's own matching record continues inside the child -- inner1 is reused, \
         not rerun"
    );
    assert_eq!(
        child(at_mut(&mut resumed, "prime.step.inner2"), "value"),
        saved_inner2,
        "inner2 likewise"
    );
}

#[tokio::test]
async fn a_use_with_resume_restart_always_reruns_its_child_even_when_the_record_matches() {
    let dir = temp_dir("use-resume-restart");
    let config = write(
        &dir,
        "config.json",
        r#"{"runtime": {"state": {"record_children": true}}}"#,
    );
    let child_doc = write(
        &dir,
        "child.yml",
        "effects:\n  - name: inner\n    type: tool\n    provider: uuid\n",
    );
    let doc = write(
        &dir,
        "doc.yml",
        &format!(
            "effects:\n  - name: step\n    type: use\n    resume: restart\n    path: {}\n",
            child_doc.display()
        ),
    );

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();
    let saved_inner = child(at_mut(&mut state, "prime.step.inner"), "value");
    set(&mut state, "prime.step.meta.completed_at", Value::None);

    let state_path = write_state(&dir, "state.json", &state);
    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;

    let second = run(&req).await;
    assert!(second.ok, "{:?}", second.error);
    let mut resumed = second.state.unwrap();
    assert_ne!(
        child(at_mut(&mut resumed, "prime.step.inner"), "value"),
        saved_inner,
        "resume: restart drops a use's saved record and reruns its child fresh, even though \
         record_children's own record would otherwise match"
    );
}

#[tokio::test]
async fn a_finally_effect_always_reruns_even_if_its_saved_node_finished() {
    let dir = temp_dir("finally");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "effects:\n  - name: outer\n    type: dynamic\n    effects:\n      - name: body\n        \
         type: tool\n        provider: uuid\n    finally:\n      - name: cleanup\n        \
         type: tool\n        provider: uuid\n",
    );

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();
    let saved_body = child(at_mut(&mut state, "prime.outer"), "body");
    let saved_cleanup = child(at_mut(&mut state, "prime.outer"), "cleanup");

    // The outer dynamic itself never finished (the interrupt landed
    // between the body completing and `finally:` ever starting), even
    // though `cleanup`'s own saved node -- from *this* run, which did
    // reach `finally:` -- looks completed.
    set(&mut state, "prime.outer.meta.completed_at", Value::None);

    let state_path = write_state(&dir, "state.json", &state);
    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;

    let second = run(&req).await;
    assert!(second.ok, "{:?}", second.error);
    let mut resumed = second.state.unwrap();
    let resumed_cleanup = child(at_mut(&mut resumed, "prime.outer"), "cleanup");
    assert_ne!(
        resumed_cleanup, saved_cleanup,
        "finally: never skips via --resume, even when its own saved node finished"
    );
    let resumed_body = child(at_mut(&mut resumed, "prime.outer"), "body");
    assert_eq!(
        resumed_body, saved_body,
        "body already looked finished and comes before finally: in the same chain -- it must be \
         skipped and reused as-is, not rerun alongside cleanup"
    );
}

#[tokio::test]
async fn a_dynamic_with_resume_restart_reruns_its_whole_scope_even_when_both_steps_finished() {
    let dir = temp_dir("dynamic-resume-restart");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "effects:\n  - name: outer\n    type: dynamic\n    resume: restart\n    effects:\n      \
         - name: step1\n        type: tool\n        provider: uuid\n      - name: step2\n        \
         type: tool\n        provider: uuid\n",
    );

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();
    let saved_step1 = child(at_mut(&mut state, "prime.outer"), "step1");
    let saved_step2 = child(at_mut(&mut state, "prime.outer"), "step2");

    // Both steps finished, but the outer dynamic's own meta never did
    // (the interrupt landed between step2 completing and the dynamic
    // marking itself done) -- the same shape `a_use_without_record_
    // children_reruns_its_child_whole` uses for a `use`.
    set(&mut state, "prime.outer.meta.completed_at", Value::None);

    let state_path = write_state(&dir, "state.json", &state);
    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;

    let second = run(&req).await;
    assert!(second.ok, "{:?}", second.error);
    let mut resumed = second.state.unwrap();
    assert_ne!(
        child(at_mut(&mut resumed, "prime.outer"), "step1"),
        saved_step1,
        "resume: restart drops the dynamic's whole saved scope -- step1 reruns fresh even \
         though it already looked finished (the positional continue rule alone would have \
         reused it)"
    );
    assert_ne!(
        child(at_mut(&mut resumed, "prime.outer"), "step2"),
        saved_step2,
        "step2 likewise"
    );
}

fn named_if_doc() -> &'static str {
    "interface:\n  inputs:\n    flag: {default: true}\n\
     effects:\n  - name: gate\n    type: if\n    if: {mode: cel, expr: state.input.flag}\n    \
     then:\n      - name: a\n        type: tool\n        provider: uuid\n      - name: b\n        \
     type: tool\n        provider: uuid\n    else:\n      - name: c\n        type: tool\n        \
     provider: uuid\n"
}

#[tokio::test]
async fn a_named_if_continues_inside_its_saved_branch_when_the_condition_selects_it_again() {
    let dir = temp_dir("if-continue");
    let config = write(&dir, "config.json", "{}");
    let doc = write(&dir, "doc.yml", named_if_doc());

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();
    let saved_a = child(at_mut(&mut state, "prime.gate"), "a");

    // Interrupted after `a` finished but before `b` did -- the gate's
    // own meta never completed, and `b` is simply absent, the same
    // shape a real interrupt mid-branch leaves.
    set(&mut state, "prime.gate.meta.completed_at", Value::None);
    remove(&mut state, "prime.gate", "b");

    let state_path = write_state(&dir, "state.json", &state);
    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;

    let second = run(&req).await;
    assert!(second.ok, "{:?}", second.error);
    let mut resumed = second.state.unwrap();
    assert_eq!(
        child(at_mut(&mut resumed, "prime.gate"), "a"),
        saved_a,
        "the condition selected the same ('then') branch again -- a, positioned before the \
         unfinished effect, is skipped and reused"
    );
    let b = child(at_mut(&mut resumed, "prime.gate"), "b");
    assert!(
        child(&b, "meta")
            .as_dict()
            .and_then(|m| m.get(&key("completed_at")))
            .is_some_and(|v| *v != Value::None),
        "b never finished -- it must actually run on resume: {b:?}"
    );
}

#[tokio::test]
async fn a_named_if_reruns_from_its_start_when_the_condition_now_selects_the_other_branch() {
    let dir = temp_dir("if-flip");
    let config = write(&dir, "config.json", "{}");
    let doc = write(&dir, "doc.yml", named_if_doc());

    let mut first_req = request(config.clone(), doc.clone());
    first_req
        .inputs
        .insert("flag".to_string(), "true".to_string());
    let first = run(&first_req).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();

    // Interrupted mid-'then' branch, exactly as the continue case above.
    set(&mut state, "prime.gate.meta.completed_at", Value::None);
    remove(&mut state, "prime.gate", "b");

    let state_path = write_state(&dir, "state.json", &state);
    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;
    // The condition now selects 'else' instead -- the saved 'then'
    // branch's own progress (however far it got) is irrelevant; 'else'
    // runs from its own first effect, same as a first run would.
    req.inputs.insert("flag".to_string(), "false".to_string());

    let second = run(&req).await;
    assert!(second.ok, "{:?}", second.error);
    let mut resumed = second.state.unwrap();
    assert_eq!(
        child(at_mut(&mut resumed, "prime.gate"), "meta")
            .as_dict()
            .and_then(|m| m.get(&key("branch")))
            .cloned(),
        Some(Value::Str("else".to_string())),
        "the condition flipped to 'else'"
    );
    let c = child(at_mut(&mut resumed, "prime.gate"), "c");
    assert!(
        child(&c, "meta")
            .as_dict()
            .and_then(|m| m.get(&key("completed_at")))
            .is_some_and(|v| *v != Value::None),
        "the newly selected branch's own effect must actually run: {c:?}"
    );
}

#[tokio::test]
async fn a_changed_document_is_refused_without_force() {
    let dir = temp_dir("hash-mismatch");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "effects:\n  - name: a\n    type: tool\n    provider: uuid\n",
    );

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let state = first.state.unwrap();
    let state_path = write_state(&dir, "state.json", &state);

    // The document changes after the saved run.
    fs::write(
        &doc,
        "effects:\n  - name: a\n    type: tool\n    provider: uuid\n  - name: b\n    type: tool\n    provider: uuid\n",
    )
    .unwrap();

    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;

    let result = run(&req).await;
    assert!(!result.ok);
    assert!(
        result.state.is_none(),
        "a refused resume must write nothing back to --state's own file (the --out default), \
         same as cof's own pre-run refusal"
    );
    assert!(
        result
            .error
            .as_deref()
            .unwrap()
            .contains("changed since the run being resumed"),
        "{:?}",
        result.error
    );
}

#[tokio::test]
async fn force_resumes_a_changed_document_anyway() {
    let dir = temp_dir("hash-mismatch-forced");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "effects:\n  - name: a\n    type: tool\n    provider: uuid\n",
    );

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let state = first.state.unwrap();
    let state_path = write_state(&dir, "state.json", &state);

    fs::write(
        &doc,
        "effects:\n  - name: a\n    type: tool\n    provider: uuid\n  - name: b\n    type: tool\n    provider: uuid\n",
    )
    .unwrap();

    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;
    req.force = true;

    let result = run(&req).await;
    assert!(result.ok, "{:?}", result.error);
}

#[tokio::test]
async fn a_state_file_with_no_document_hash_is_refused_without_force() {
    let dir = temp_dir("no-hash");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "effects:\n  - name: a\n    type: tool\n    provider: uuid\n",
    );

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();
    set(&mut state, "runtime.last_run.document_hash", Value::None);
    let state_path = write_state(&dir, "state.json", &state);

    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;

    let result = run(&req).await;
    assert!(!result.ok);
    assert!(
        result.state.is_none(),
        "same as the hash-mismatch refusal above"
    );
    assert!(
        result
            .error
            .as_deref()
            .unwrap()
            .contains("no way to tell whether this document changed"),
        "{:?}",
        result.error
    );
}

#[tokio::test]
async fn an_empty_document_hash_is_refused_as_missing_not_changed() {
    let dir = temp_dir("empty-hash");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "effects:\n  - name: a\n    type: tool\n    provider: uuid\n",
    );

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();
    // `cli/app.py::_resolve_resume_state`'s own `if not recorded_hash:`
    // (Python truthiness) treats an empty string the same as missing --
    // never the "changed" refusal an empty string would otherwise
    // mismatch into.
    set(
        &mut state,
        "runtime.last_run.document_hash",
        Value::Str(String::new()),
    );
    let state_path = write_state(&dir, "state.json", &state);

    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;

    let result = run(&req).await;
    assert!(!result.ok);
    assert!(result.state.is_none());
    assert!(
        result
            .error
            .as_deref()
            .unwrap()
            .contains("no way to tell whether this document changed"),
        "{:?}",
        result.error
    );
}

#[tokio::test]
async fn a_state_file_with_no_runtime_key_never_falls_back_to_a_root_level_last_run() {
    let dir = temp_dir("no-runtime-key");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "effects:\n  - name: a\n    type: tool\n    provider: uuid\n",
    );

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();

    // A root-level `last_run.document_hash` that would mismatch if the
    // check ever fell back to reading it -- `runtime` itself is removed
    // entirely, so `cli/app.py`'s own `last_run_record = loaded.get(
    // "runtime", {}).get("last_run") if isinstance(..., dict) else None`
    // must see `None`, never this decoy.
    let mut decoy = Dict::new();
    decoy.insert(
        key("document_hash"),
        Value::Str("not-a-real-hash".to_string()),
    );
    let root = state.as_dict_mut().unwrap();
    root.insert(key("last_run"), Value::Dict(decoy));
    root.shift_remove(&key("runtime"));
    let state_path = write_state(&dir, "state.json", &state);

    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;

    let result = run(&req).await;
    assert!(!result.ok);
    assert!(result.state.is_none());
    assert!(
        result
            .error
            .as_deref()
            .unwrap()
            .contains("no way to tell whether this document changed"),
        "a missing runtime key must be refused as \"no document_hash\", never by reading a \
         root-level last_run decoy: {:?}",
        result.error
    );
}

#[tokio::test]
async fn a_pre_407_legacy_document_hash_resumes_without_force() {
    let dir = temp_dir("legacy-hash");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "effects:\n  - name: a\n    type: tool\n    provider: uuid\n",
    );

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();

    let loaded = electricity_compiler::prepare_document(
        &doc,
        &electricity_compiler::CheckOptions::default(),
    )
    .unwrap();
    let legacy_hash = electricity_compiler::legacy_document_sha256(
        &loaded.path,
        &loaded.document,
        &loaded.confinement_root,
    )
    .unwrap();
    set(
        &mut state,
        "runtime.last_run.document_hash",
        Value::Str(legacy_hash),
    );
    let state_path = write_state(&dir, "state.json", &state);

    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;

    let result = run(&req).await;
    assert!(result.ok, "{:?}", result.error);
}

#[tokio::test]
async fn a_legacy_root_key_state_file_is_lifted_under_input_on_resume() {
    let dir = temp_dir("migrate-legacy-state");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "interface:\n  inputs:\n    name: {type: string}\n\
         effects:\n  - name: a\n    type: tool\n    provider: uuid\n",
    );

    let mut first_req = request(config.clone(), doc.clone());
    first_req
        .inputs
        .insert("name".to_string(), "Ada".to_string());
    let first = run(&first_req).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();

    // A pre-namespace snapshot: `name` lifted back out to the root,
    // `input` removed entirely -- `core/state_ns.py::migrate_legacy_
    // state`'s own "no input key at all" trigger.
    let name_value = child(at_mut(&mut state, "input"), "name");
    let root = state.as_dict_mut().unwrap();
    root.shift_remove(&key("input"));
    root.insert(key("name"), name_value);
    let state_path = write_state(&dir, "state.json", &state);

    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = false; // plain --state, same as cof's own `_load_state`

    let result = run(&req).await;
    assert!(result.ok, "{:?}", result.error);
    let mut resumed = result.state.unwrap();
    assert_eq!(
        child(at_mut(&mut resumed, "input"), "name"),
        Value::Str("Ada".to_string()),
        "a legacy root key is lifted back under 'input' on load, not left at the root"
    );
}

#[tokio::test]
async fn a_missing_state_file_fails_with_no_state_at_all() {
    let dir = temp_dir("missing-state-file");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "effects:\n  - name: a\n    type: tool\n    provider: uuid\n",
    );

    let mut req = request(config, doc);
    req.state_path = Some(dir.join("does-not-exist.json"));
    req.resume = true;

    let result = run(&req).await;
    assert!(!result.ok);
    assert!(result.state.is_none());
    assert!(
        result
            .error
            .as_deref()
            .unwrap()
            .contains("state file not found"),
        "{:?}",
        result.error
    );
}

#[tokio::test]
async fn plain_state_with_no_resume_seeds_the_whole_tree_but_never_skips() {
    let dir = temp_dir("plain-state-no-resume");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "effects:\n  - name: a\n    type: tool\n    provider: uuid\n",
    );

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let state = first.state.unwrap();
    let saved_a = child(&child(&state, "prime"), "a");
    let state_path = write_state(&dir, "state.json", &state);

    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = false; // plain --state, no --resume

    let result = run(&req).await;
    assert!(result.ok, "{:?}", result.error);
    let resumed = result.state.unwrap();
    let resumed_a = child(&child(&resumed, "prime"), "a");
    assert_ne!(
        resumed_a, saved_a,
        "--state with no --resume still reruns everything -- only --resume skips"
    );
}

#[tokio::test]
async fn an_e_override_merges_onto_the_resumed_input_namespace() {
    let dir = temp_dir("e-override-merge");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "interface:\n  inputs:\n    name: {type: string}\n    other: {type: string}\n\
         effects:\n  - name: a\n    type: tool\n    provider: uuid\n",
    );

    let mut first_req = request(config.clone(), doc.clone());
    first_req
        .inputs
        .insert("name".to_string(), "Ada".to_string());
    first_req
        .inputs
        .insert("other".to_string(), "kept".to_string());
    let first = run(&first_req).await;
    assert!(first.ok, "{:?}", first.error);
    let state = first.state.unwrap();
    let state_path = write_state(&dir, "state.json", &state);

    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;
    req.inputs.insert("name".to_string(), "Grace".to_string());

    let result = run(&req).await;
    assert!(result.ok, "{:?}", result.error);
    let mut resumed = result.state.unwrap();
    let input = at_mut(&mut resumed, "input");
    assert_eq!(child(input, "name"), Value::Str("Grace".to_string()));
    assert_eq!(child(input, "other"), Value::Str("kept".to_string()));
}

#[tokio::test]
async fn a_finished_reflector_is_skipped_on_resume() {
    // issue #585: a successful reflector now records meta.completed_at
    // like any other effect, so --resume skips it whole, along with
    // the chain step after it staying skipped too (never reruns just
    // because an earlier positional pivot moved past it).
    let dir = temp_dir("reflector-resume");
    let replies_path = write(
        &dir,
        "scripted-replies.yaml",
        "prime.plan_it.inner.propose_steps:\n  - text: |\n      done: true\n",
    );
    let config = write(
        &dir,
        "config.json",
        &format!(
            r#"{{"runtime": {{"adapters": {{"scripted": {{"replies_file": "{}"}}}}}}}}"#,
            replies_path.display()
        ),
    );
    let doc = write(
        &dir,
        "doc.yml",
        "adapter: scripted\nmodel: test\neffects:\n  - type: reflector\n    name: plan_it\n    \
         prime_template: \"Plan it.\"\n    effects:\n      - type: prompt\n        \
         name: propose_steps\n        template: \"propose\"\n  - name: after\n    type: tool\n    \
         provider: uuid\n",
    );

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();
    let saved_completed_at = child(at_mut(&mut state, "prime.plan_it.meta"), "completed_at");
    assert_ne!(saved_completed_at, Value::None);
    let saved_after = child(at_mut(&mut state, "prime.after"), "value");

    let state_path = write_state(&dir, "state.json", &state);
    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;

    let second = run(&req).await;
    assert!(second.ok, "{:?}", second.error);
    let mut resumed = second.state.unwrap();
    let resumed_completed_at = child(at_mut(&mut resumed, "prime.plan_it.meta"), "completed_at");
    assert_eq!(
        resumed_completed_at, saved_completed_at,
        "the reflector is skipped whole, not rerun"
    );
    let resumed_iterations = at_mut(&mut resumed, "prime.plan_it.meta.iterations").clone();
    let saved_iterations = at_mut(&mut state, "prime.plan_it.meta.iterations").clone();
    assert_eq!(
        resumed_iterations, saved_iterations,
        "skipped whole: no second planning call appended a second iteration"
    );
    let resumed_after = child(at_mut(&mut resumed, "prime.after"), "value");
    assert_eq!(
        resumed_after, saved_after,
        "and the chain step after it is skipped too"
    );
}

#[tokio::test]
async fn a_reflector_that_failed_then_succeeds_on_resume_clears_its_old_error() {
    // issue #585 follow-up: `meta.error` is cleared at the start of
    // every reflector run (`exec/reflector.rs`'s own matching clear),
    // so a reflector that failed on a prior attempt and then succeeds
    // on resume is read as finished -- not kept failing forever by a
    // stale `meta.error` `effect_completed_ok` would otherwise still
    // see on every later resume.
    let dir = temp_dir("reflector-resume-clears-error");
    let replies_path = write(
        &dir,
        "scripted-replies.yaml",
        "prime.plan_it.inner.propose_steps:\n  - text: |\n      done: true\n",
    );
    let config = write(
        &dir,
        "config.json",
        &format!(
            r#"{{"runtime": {{"adapters": {{"scripted": {{"replies_file": "{}"}}}}}}}}"#,
            replies_path.display()
        ),
    );
    let doc = write(
        &dir,
        "doc.yml",
        "adapter: scripted\nmodel: test\neffects:\n  - type: reflector\n    name: plan_it\n    \
         prime_template: \"Plan it.\"\n    effects:\n      - type: prompt\n        \
         name: propose_steps\n        template: \"propose\"\n",
    );

    let first = run(&request(config.clone(), doc.clone())).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();
    // Simulate a prior attempt that failed: completed_at *and* error
    // both set, exactly what the failure path writes, left over from
    // before this issue's fix.
    insert(
        &mut state,
        "prime.plan_it.meta",
        "error",
        Value::Str("boom".to_string()),
    );
    set(&mut state, "prime.plan_it.value", Value::Bool(false));

    let state_path = write_state(&dir, "state.json", &state);
    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;

    let second = run(&req).await;
    assert!(second.ok, "{:?}", second.error);
    let mut resumed = second.state.unwrap();
    let resumed_meta = at_mut(&mut resumed, "prime.plan_it.meta").clone();
    assert_eq!(
        child(&resumed_meta, "error"),
        Value::None,
        "the stale error is cleared, not carried over from the failed attempt"
    );
    assert_ne!(child(&resumed_meta, "completed_at"), Value::None);
    assert_eq!(child(at_mut(&mut resumed, "prime.plan_it"), "value"), Value::Bool(true));
}

#[tokio::test]
async fn a_use_whose_resolved_inputs_changed_reruns_along_with_what_follows_it() {
    // issue #584: a finished `use`'s inputs, resolved against the
    // resumed run's own inputs, are checked against its recorded
    // `meta.inputs` -- changed, it reruns (a fresh `uuid` value), and
    // so does the chain step after it, by the usual positional rule.
    let dir = temp_dir("use-changed-inputs");
    let config = write(&dir, "config.json", "{}");
    let child_doc = write(
        &dir,
        "child.yml",
        "interface:\n  inputs:\n    x: {type: string, required: true}\n\
         effects:\n  - name: inner\n    type: tool\n    provider: uuid\n",
    );
    let doc = write(
        &dir,
        "doc.yml",
        &format!(
            "interface:\n  inputs:\n    x: {{type: string, default: a}}\n\
             effects:\n  - name: part\n    type: use\n    path: {}\n    \
             inputs: {{x: {{from: input.x}}}}\n  - name: after\n    type: tool\n    \
             provider: uuid\n",
            child_doc.display()
        ),
    );

    let mut first_req = request(config.clone(), doc.clone());
    first_req.inputs.insert("x".to_string(), "a".to_string());
    let first = run(&first_req).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();
    let saved_inner = child(at_mut(&mut state, "prime.part.inner"), "value");
    let saved_after = child(at_mut(&mut state, "prime.after"), "value");
    assert_eq!(
        child(at_mut(&mut state, "prime.part.meta.inputs"), "x"),
        Value::Str("a".to_string())
    );

    let state_path = write_state(&dir, "state.json", &state);
    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;
    req.inputs.insert("x".to_string(), "b".to_string());

    let second = run(&req).await;
    assert!(second.ok, "{:?}", second.error);
    let mut resumed = second.state.unwrap();
    let resumed_inner = child(at_mut(&mut resumed, "prime.part.inner"), "value");
    assert_ne!(resumed_inner, saved_inner, "changed inputs: part reruns");
    let resumed_after = child(at_mut(&mut resumed, "prime.after"), "value");
    assert_ne!(
        resumed_after, saved_after,
        "and so does the chain step after it"
    );
    assert_eq!(
        child(at_mut(&mut resumed, "prime.part.meta.inputs"), "x"),
        Value::Str("b".to_string())
    );
}

#[tokio::test]
async fn a_use_whose_resolved_inputs_are_unchanged_stays_skipped() {
    // issue #584's other half: an unchanged `-e`/run input leaves a
    // finished `use` -- and the chain step after it -- skipped whole,
    // as before this issue.
    let dir = temp_dir("use-unchanged-inputs");
    let config = write(&dir, "config.json", "{}");
    let child_doc = write(
        &dir,
        "child.yml",
        "interface:\n  inputs:\n    x: {type: string, required: true}\n\
         effects:\n  - name: inner\n    type: tool\n    provider: uuid\n",
    );
    let doc = write(
        &dir,
        "doc.yml",
        &format!(
            "interface:\n  inputs:\n    x: {{type: string, default: a}}\n\
             effects:\n  - name: part\n    type: use\n    path: {}\n    \
             inputs: {{x: {{from: input.x}}}}\n  - name: after\n    type: tool\n    \
             provider: uuid\n",
            child_doc.display()
        ),
    );

    let mut first_req = request(config.clone(), doc.clone());
    first_req.inputs.insert("x".to_string(), "a".to_string());
    let first = run(&first_req).await;
    assert!(first.ok, "{:?}", first.error);
    let mut state = first.state.unwrap();
    let saved_inner = child(at_mut(&mut state, "prime.part.inner"), "value");
    let saved_after = child(at_mut(&mut state, "prime.after"), "value");

    let state_path = write_state(&dir, "state.json", &state);
    let mut req = request(config, doc);
    req.state_path = Some(state_path);
    req.resume = true;
    req.inputs.insert("x".to_string(), "a".to_string());

    let second = run(&req).await;
    assert!(second.ok, "{:?}", second.error);
    let mut resumed = second.state.unwrap();
    let resumed_inner = child(at_mut(&mut resumed, "prime.part.inner"), "value");
    assert_eq!(
        resumed_inner, saved_inner,
        "unchanged inputs: part stays skipped"
    );
    let resumed_after = child(at_mut(&mut resumed, "prime.after"), "value");
    assert_eq!(
        resumed_after, saved_after,
        "and so does the chain step after it"
    );
}
