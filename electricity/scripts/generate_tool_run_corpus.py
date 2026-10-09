#!/usr/bin/env python3
"""Golden run corpus for lane C of issue #431's own `execute_tool`
(`electricity-vm::exec::tool`), built on `_run_corpus.py` the same way
`generate_vm_run_corpus.py` is -- every case is a real `cof run --out
--events --live-state`, not a hand-typed literal.

This is a *document*-level corpus: every case's own root is a `dynamic`
chain (Circuitry's own implicit `prime` container), so comparing a
case's own `--out`/`--events` against `electricity::run_orchestration`
needs lane B2's real `exec::dynamic` chain executor, not just this
lane's own `execute_tool` -- `tests/tool_run_corpus.rs`'s own comparison
tests are `#[ignore = "needs lane B2"]` for exactly that reason (issue
#431's own Lanes note: "whichever of B2/C merges second removes the
markers and makes them pass exactly"). Generating this corpus itself has
no such dependency: it only runs the real Python CLI.

Two of the review's own gaps on the function-level corpus
(`generate_tool_execute_corpus.py`) have no document-level equivalent
here and are deliberately not duplicated:

- `waiting_for` under contention needs two real tool dispatches racing
  for the same limiter slot at precisely the same instant -- not
  something a committed, replayable `--out`/`--events` snapshot can
  capture at all (the field is always back to `null` by the time a run
  finishes, and `--events` v1 has no "now waiting" event of its own);
  `tool.rs`'s own `a_contended_group_slot_reports_waiting_for_then_
  clears_it` test is the right tool for this, since it controls the
  limiter and the clock directly.
- the 64 KiB `raw` cap never fires through the real `json` tool (its own
  `ToolResult.raw` is always a tiny `{"mode": ...}`); `tool.rs`'s own
  `a_huge_raw_value_is_capped_through_execute_tools_own_wiring` test
  (a mock plugin) is the only way to exercise `execute_tool`'s own
  wiring for it at all.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI). Usage: python3 generate_tool_run_corpus.py [--check]
"""

from __future__ import annotations

import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from _run_corpus import normalize_result, render_corpus, run_cof, write_or_check

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity"
    / "tests"
    / "golden"
    / "tool_run_corpus.json"
)

SUCCESS_CHAIN_DOC = """\
effects:
  - type: tool
    name: encode
    provider: json
    params:
      mode: stringify
      input: {greeting: "hi"}
  - type: tool
    name: decode
    provider: json
    params:
      mode: parse
      input: "{{{prime.encode.value}}}"
"""

TOOL_ERROR_FAIL_DOC = """\
effects:
  - type: tool
    name: broken
    provider: json
    params:
      mode: parse
      input: "not json"
"""

TOOL_ERROR_SKIP_DOC = """\
effects:
  - type: tool
    name: broken
    on_error: skip
    provider: json
    params:
      mode: parse
      input: "not json"
  - type: tool
    name: after
    provider: json
    params: {mode: stringify, input: {ran: true}}
"""

RETRIES_EXPECT_EXHAUSTS_DOC = """\
effects:
  - type: tool
    name: never_matches
    on_error: skip
    provider: json
    retries: {max_attempts: 3, backoff_ms: 0}
    expect: "value == 2"
    params:
      mode: parse
      input: "1"
"""

RETRIES_EXPECT_PASSES_DOC = """\
effects:
  - type: tool
    name: matches_first_try
    provider: json
    expect: "value == 2"
    params:
      mode: parse
      input: "2"
"""

CEL_EXPECT_RAISES_DOC = """\
effects:
  - type: tool
    name: raising_expect
    on_error: skip
    provider: json
    retries: {max_attempts: 1, backoff_ms: 0}
    expect: "value.missing == 1"
    params:
      mode: parse
      input: "{}"
"""

ALLOWLIST_REFUSAL_DOC = """\
effects:
  - type: tool
    name: denied
    provider: json
    params: {mode: stringify, input: {a: 1}}
"""

PARAMS_JSON_MERGE_DOC = """\
effects:
  - type: tool
    name: merged
    provider: json
    params:
      mode: stringify
      input: {a: 1}
    params_json: '{"input": {"b": 2}}'
"""

# `true`/`1` would collide as dict keys (`True == 1` in Python) and trip
# electricity's own duplicate-key guard at YAML-load time before the
# `json` tool ever runs (`generate_tool_runtime_corpus.py`'s own
# function-level case already covers that collision directly) -- `1`/
# `2` are both non-string YAML keys that don't collide, so this case
# exercises non-string keys surviving the full load -> compile ->
# params -> `json` tool pipeline instead.
NON_STRING_PARAM_KEYS_DOC = """\
effects:
  - type: tool
    name: non_string_keys
    provider: json
    params:
      mode: stringify
      input:
        1: "one-value"
        2: "two-value"
"""

REDACTION_IN_PARAMS_RENDERED_DOC = """\
effects:
  - type: tool
    name: has_a_secret
    provider: json
    params:
      mode: stringify
      input: {api_key: "super-secret", name: "ok"}
"""


def build_case(
    *,
    name: str,
    orchestration: str,
    home_root: Path,
    config: dict | None = None,
) -> dict:
    case_dir = home_root / name
    case_dir.mkdir(parents=True)
    home_dir = case_dir / "home"
    home_dir.mkdir()
    (case_dir / "orchestration.yml").write_text(orchestration, encoding="utf-8")

    result = run_cof(case_dir, config=config, home_dir=home_dir)
    normalized = normalize_result(result, str(case_dir.resolve()))
    return {"name": name, "result": normalized}


def main() -> int:
    check = "--check" in sys.argv[1:]

    with tempfile.TemporaryDirectory(prefix="electricity-tool-run-corpus-") as tmp:
        root = Path(tmp).resolve()
        cases = [
            build_case(
                name="success_chain", orchestration=SUCCESS_CHAIN_DOC, home_root=root
            ),
            build_case(
                name="tool_error_fail",
                orchestration=TOOL_ERROR_FAIL_DOC,
                home_root=root,
            ),
            build_case(
                name="tool_error_skip",
                orchestration=TOOL_ERROR_SKIP_DOC,
                home_root=root,
            ),
            build_case(
                name="retries_expect_exhausts",
                orchestration=RETRIES_EXPECT_EXHAUSTS_DOC,
                home_root=root,
            ),
            build_case(
                name="retries_expect_passes",
                orchestration=RETRIES_EXPECT_PASSES_DOC,
                home_root=root,
            ),
            build_case(
                name="cel_expect_raises",
                orchestration=CEL_EXPECT_RAISES_DOC,
                home_root=root,
            ),
            build_case(
                name="allowlist_refusal",
                orchestration=ALLOWLIST_REFUSAL_DOC,
                home_root=root,
                config={"enabled_tools": []},
            ),
            build_case(
                name="params_json_merge",
                orchestration=PARAMS_JSON_MERGE_DOC,
                home_root=root,
            ),
            build_case(
                name="non_string_param_keys",
                orchestration=NON_STRING_PARAM_KEYS_DOC,
                home_root=root,
            ),
            build_case(
                name="redaction_in_params_rendered",
                orchestration=REDACTION_IN_PARAMS_RENDERED_DOC,
                home_root=root,
            ),
        ]

    text = render_corpus(cases)
    ok = write_or_check(OUTPUT, text, check=check)
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
