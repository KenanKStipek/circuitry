//! Exercises the golden run corpus `electricity/scripts/generate_vm_run_
//! corpus.py` writes (issue #431's Test strategy section). Lane B2 has
//! landed (`electricity_vm::execute_root`'s real interpreter), so the
//! three comparison tests that were `#[ignore = "needs lanes B-C"]`
//! now actually run each case's document through `electricity::
//! run_orchestration` and compare against the committed golden,
//! normalized the same way `electricity/scripts/_run_corpus.py` does
//! (`support::normalize`'s own doc comment has the two deliberate
//! exceptions: the CLI-invocation-shape fields, and the tree case's
//! own event ordering).

mod support;

use electricity::run::RunRequest;
use electricity_vm::CancellationToken;
use indexmap::IndexMap;
use serde_json::Value as Json;
use std::fs;
use std::path::PathBuf;
use support::normalize::{normalize, normalize_event, strip_invocation_shape_fields};
use support::run_corpus::{CorpusCase, load_corpus};

#[test]
fn the_corpus_has_at_least_the_four_smoke_cases() {
    let cases = load_corpus();
    let names: Vec<&str> = cases.iter().map(|case| case.name.as_str()).collect();
    for expected in [
        "json_tool_chain",
        "cel_if",
        "tree_dynamic",
        "json_parse_failure",
    ] {
        assert!(
            names.contains(&expected),
            "expected a case named {expected:?}, got {names:?}"
        );
    }
}

#[test]
fn every_case_name_is_unique() {
    let cases = load_corpus();
    let mut names: Vec<&str> = cases.iter().map(|case| case.name.as_str()).collect();
    let unique_count = {
        names.sort_unstable();
        names.dedup();
        names.len()
    };
    assert_eq!(unique_count, load_corpus().len());
}

#[test]
fn a_successful_case_has_a_zero_returncode_and_a_final_state() {
    let cases = load_corpus();
    let case = cases
        .iter()
        .find(|case| case.name == "json_tool_chain")
        .expect("json_tool_chain case");
    assert_eq!(case.result.returncode, 0);
    assert!(case.result.state.is_some());
    assert!(!case.result.events.is_empty());
}

#[test]
fn the_failure_case_has_a_nonzero_returncode() {
    let cases = load_corpus();
    let case = cases
        .iter()
        .find(|case| case.name == "json_parse_failure")
        .expect("json_parse_failure case");
    assert_ne!(case.result.returncode, 0);
}

#[test]
fn every_events_entry_has_a_sequence_number_and_a_kind() {
    let cases = load_corpus();
    for case in &cases {
        for event in &case.result.events {
            assert!(
                event.get("seq").is_some_and(|v| v.is_number()),
                "{}: an --events entry is missing a numeric 'seq': {event}",
                case.name
            );
            assert!(
                event.get("ev").is_some_and(|v| v.is_string()),
                "{}: an --events entry is missing a string 'ev': {event}",
                case.name
            );
        }
    }
}

#[test]
fn the_tree_case_has_a_dispatch_event_with_two_branches() {
    let cases = load_corpus();
    let case = cases
        .iter()
        .find(|case| case.name == "tree_dynamic")
        .expect("tree_dynamic case");
    let dispatch = case
        .result
        .events
        .iter()
        .find(|event| event.get("ev").and_then(|v| v.as_str()) == Some("dispatch"))
        .expect("a dispatch event");
    assert_eq!(dispatch["branches"].as_u64(), Some(2));
}

// ---------------------------------------------------------------------
// The four smoke cases' own documents and inputs, copied verbatim from
// `electricity/scripts/generate_vm_run_corpus.py` -- the golden corpus
// itself doesn't carry the document text, only `cof run`'s own result.
// ---------------------------------------------------------------------

const JSON_TOOL_CHAIN_DOC: &str = "\
effects:
  - type: tool
    name: encode
    provider: json
    params:
      mode: stringify
      input: {greeting: \"hi\"}
  - type: tool
    name: decode
    provider: json
    params:
      mode: parse
      input: \"{{{prime.encode.value}}}\"
  - type: tool
    name: extract
    provider: json
    params:
      mode: extract
      input: \"{{{prime.encode.value}}}\"
      path: greeting
";

const CEL_IF_DOC: &str = "\
effects:
  - type: if
    name: gate
    if:
      mode: cel
      expr: \"has(state.input.n) && state.input.n > 3\"
    then:
      - type: tool
        name: yes_branch
        provider: json
        params: {mode: stringify, input: {ok: true}}
    else:
      - type: tool
        name: no_branch
        provider: json
        params: {mode: stringify, input: {ok: false}}
";

const TREE_DYNAMIC_DOC: &str = "\
effects:
  - type: dynamic
    name: fan_out
    flow: tree
    effects:
      - type: tool
        name: left
        provider: json
        params: {mode: stringify, input: {branch: \"left\"}}
      - type: tool
        name: right
        provider: json
        params: {mode: stringify, input: {branch: \"right\"}}
";

const FAILURE_DOC: &str = "\
effects:
  - type: tool
    name: broken
    provider: json
    params:
      mode: parse
      input: 123
";

static TEMP_DIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A fresh directory for *name* -- counter-suffixed, not just
/// *name*-and-pid-keyed, since three separate `#[tokio::test]`
/// functions below each call [`run_case`] with the same case *name*
/// and run concurrently (the default for every `#[tokio::test]` in one
/// binary): without the counter, two of them would race on the exact
/// same path.
fn temp_dir(name: &str) -> PathBuf {
    let n = TEMP_DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "electricity-run-corpus-compare-{name}-{}-{n}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

struct RanCase {
    state: Json,
    events: Vec<Json>,
    live_state_text: String,
    out_text: String,
}

/// Runs *orchestration* through `electricity::run_orchestration` the
/// same shape every other case in this corpus is run with (an empty
/// `{}` config, `--out`/`--events`/`--live-state` all given), returning
/// each as parsed/normalized JSON ready to compare against a
/// [`CorpusCase`].
async fn run_case(name: &str, orchestration: &str, inputs: &[(&str, &str)]) -> RanCase {
    let dir = temp_dir(name);
    let config_path = dir.join("config.json");
    fs::write(&config_path, "{}").unwrap();
    let doc_path = dir.join("orchestration.yml");
    fs::write(&doc_path, orchestration).unwrap();
    let out_path = dir.join("out.json");
    let events_path = dir.join("events.jsonl");
    let live_state_path = dir.join("live_state.json");

    let mut input_map = IndexMap::new();
    for (k, v) in inputs {
        input_map.insert(k.to_string(), v.to_string());
    }
    let req = RunRequest {
        config_path,
        orchestration_path: doc_path,
        inputs: input_map,
        out_path: Some(out_path.clone()),
        pretty: false,
        live_state_path: Some(live_state_path.clone()),
        events_path: Some(events_path.clone()),
    };
    let token = CancellationToken::new();
    let result = electricity::run_orchestration(&req, &token).await;
    let state = result.state.expect("every smoke case still writes state");
    let state_text = electricity::out::render_state(&state, false);
    fs::write(&out_path, &state_text).unwrap();

    let root = dir.to_str().unwrap();
    let state_json: Json = serde_json::from_str(&state_text).unwrap();
    let state_json = normalize(None, &state_json, root);
    let state_json = strip_invocation_shape_fields(&state_json);

    let events: Vec<Json> = fs::read_to_string(&events_path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| normalize_event(&serde_json::from_str(l).unwrap(), root))
        .collect();

    let live_state_text = fs::read_to_string(&live_state_path).unwrap_or_default();

    RanCase {
        state: state_json,
        events,
        live_state_text,
        out_text: state_text,
    }
}

fn corpus_case<'a>(cases: &'a [CorpusCase], name: &str) -> &'a CorpusCase {
    cases
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("{name}: no such case"))
}

fn expected_state(case: &CorpusCase, root_placeholder: &str) -> Json {
    let state = case
        .result
        .state
        .clone()
        .unwrap_or_else(|| panic!("{}: corpus case has no state", case.name));
    // Already normalized by the generator (`<root>` etc.) -- `root` here
    // is a sentinel that never appears in the committed text, so this
    // extra pass is a no-op beyond `strip_invocation_shape_fields`.
    let _ = root_placeholder;
    strip_invocation_shape_fields(&state)
}

#[tokio::test]
async fn electricity_run_orchestration_matches_the_corpus_state() {
    let cases = load_corpus();
    for (name, doc, inputs) in [
        ("json_tool_chain", JSON_TOOL_CHAIN_DOC, &[][..]),
        ("cel_if", CEL_IF_DOC, &[("n", "5")][..]),
        ("json_parse_failure", FAILURE_DOC, &[][..]),
    ] {
        let case = corpus_case(&cases, name);
        let ran = run_case(name, doc, inputs).await;
        assert_eq!(
            ran.state,
            expected_state(case, "<root>"),
            "{name}: state diverges from the golden corpus"
        );
    }

    // `tree_dynamic` has two concurrent branches -- its own state is
    // still compared exactly (the merge is by declared index order,
    // deterministic regardless of which branch's tool call actually
    // finishes first); only its own *events* get the looser,
    // order-insensitive comparison (the test below).
    let case = corpus_case(&cases, "tree_dynamic");
    let ran = run_case("tree_dynamic", TREE_DYNAMIC_DOC, &[]).await;
    assert_eq!(
        ran.state,
        expected_state(case, "<root>"),
        "tree_dynamic: state diverges from the golden corpus"
    );
}

/// *events*, as a multiset keyed by (`ev`, `path`, whether it reports
/// an error) -- `seq`/`id`/timing dropped entirely. Used only for the
/// tree case's own events (see this file's module doc comment for why
/// that one case doesn't get the ordered comparison the other three
/// do).
fn event_multiset(events: &[Json]) -> Vec<(String, String, bool)> {
    let mut keys: Vec<(String, String, bool)> = events
        .iter()
        .map(|e| {
            let ev = e.get("ev").and_then(Json::as_str).unwrap_or("").to_string();
            let path = e
                .get("path")
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_string();
            let has_error = e.get("error").is_some();
            (ev, path, has_error)
        })
        .collect();
    keys.sort();
    keys
}

#[tokio::test]
async fn electricity_events_match_the_corpus_events() {
    let cases = load_corpus();

    for (name, doc, inputs) in [
        ("json_tool_chain", JSON_TOOL_CHAIN_DOC, &[][..]),
        ("cel_if", CEL_IF_DOC, &[("n", "5")][..]),
        ("json_parse_failure", FAILURE_DOC, &[][..]),
    ] {
        let case = corpus_case(&cases, name);
        let ran = run_case(name, doc, inputs).await;
        // These three are purely sequential (no concurrent dynamic), so
        // canonical order already equals real emission order on both
        // sides -- `seq`/`id` land identically without any reordering.
        assert_eq!(
            ran.events, case.result.events,
            "{name}: --events diverges from the golden corpus"
        );
    }

    // `tree_dynamic`: per issue #431's own acceptance criteria, two
    // engines' events for a concurrent case are compared "as the
    // start-before-child/child-end-before-container partial order and
    // the per-path multiset, not the interleaving" -- applied here too,
    // since the golden was recorded from a *different* engine
    // (`cof run`) than the one under test.
    let case = corpus_case(&cases, "tree_dynamic");
    let ran = run_case("tree_dynamic", TREE_DYNAMIC_DOC, &[]).await;
    assert_eq!(
        event_multiset(&ran.events),
        event_multiset(&case.result.events),
        "tree_dynamic: --events' own (ev, path, has_error) multiset diverges"
    );
}

#[tokio::test]
async fn electricity_live_state_matches_the_out_state() {
    for (name, doc, inputs) in [
        ("json_tool_chain", JSON_TOOL_CHAIN_DOC, &[][..]),
        ("cel_if", CEL_IF_DOC, &[("n", "5")][..]),
        ("tree_dynamic", TREE_DYNAMIC_DOC, &[][..]),
        ("json_parse_failure", FAILURE_DOC, &[][..]),
    ] {
        let ran = run_case(name, doc, inputs).await;
        assert_eq!(
            ran.live_state_text, ran.out_text,
            "{name}: --live-state's own final write isn't byte-identical to --out"
        );
    }
}
