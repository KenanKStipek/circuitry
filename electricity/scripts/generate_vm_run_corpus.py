#!/usr/bin/env python3
"""Smoke golden run corpus for the M0-H VM (issue #431's Test strategy
section, lane A's own scaffold -- later lanes add the full acceptance-
criteria corpus through their own `generate_*_run_corpus.py`, reusing
`_run_corpus.py`).

Four synthetic cases, each run through a real `cof run --out --events
--live-state`:

- a `json`-tool chain (stringify -> parse -> extract);
- a CEL `if`, branching on an `-e` input;
- a tree `dynamic` with two parallel `json`-tool children;
- a failure: a `json` `parse` call given a non-string input.

Must be run with Python 3.11 (the lane venv locally;
`actions/setup-python` 3.11 in CI). Usage:
    python3 generate_vm_run_corpus.py [--check]
"""

from __future__ import annotations

import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from _run_corpus import (
    canonical_event_order,
    normalize_result,
    render_corpus,
    run_cof,
    write_or_check,
)

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity"
    / "tests"
    / "golden"
    / "run_corpus.json"
)

JSON_TOOL_CHAIN_DOC = """\
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
  - type: tool
    name: extract
    provider: json
    params:
      mode: extract
      input: "{{{prime.encode.value}}}"
      path: greeting
"""

CEL_IF_DOC = """\
effects:
  - type: if
    name: gate
    if:
      mode: cel
      expr: "has(state.input.n) && state.input.n > 3"
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
"""

TREE_DYNAMIC_DOC = """\
effects:
  - type: dynamic
    name: fan_out
    flow: tree
    effects:
      - type: tool
        name: left
        provider: json
        params: {mode: stringify, input: {branch: "left"}}
      - type: tool
        name: right
        provider: json
        params: {mode: stringify, input: {branch: "right"}}
"""

FAILURE_DOC = """\
effects:
  - type: tool
    name: broken
    provider: json
    params:
      mode: parse
      input: 123
"""


def build_case(
    *,
    name: str,
    orchestration: str,
    home_root: Path,
    inputs: dict[str, str] | None = None,
    branch_order: dict[str, list[str]] | None = None,
) -> dict:
    case_dir = home_root / name
    case_dir.mkdir(parents=True)
    home_dir = case_dir / "home"
    home_dir.mkdir()
    (case_dir / "orchestration.yml").write_text(orchestration, encoding="utf-8")

    result = run_cof(case_dir, inputs=inputs, home_dir=home_dir)
    # A tree `dynamic`'s own branches can finish in either order between
    # two runs of the same document (real thread scheduling) -- without
    # this, `tree_dynamic`'s own golden would be nondeterministic from
    # one generator run to the next (`canonical_event_order`'s own doc
    # comment has the full rationale). A no-op for every other case
    # (*branch_order* `None`, and none of them dispatch a `dynamic` tree
    # at all).
    result["events"] = canonical_event_order(result["events"], branch_order)
    normalized = normalize_result(result, str(case_dir.resolve()))
    return {"name": name, "inputs": inputs or {}, "result": normalized}


def main() -> int:
    check = "--check" in sys.argv[1:]

    with tempfile.TemporaryDirectory(prefix="electricity-vm-run-corpus-") as tmp:
        root = Path(tmp).resolve()
        cases = [
            build_case(
                name="json_tool_chain",
                orchestration=JSON_TOOL_CHAIN_DOC,
                home_root=root,
            ),
            build_case(
                name="cel_if",
                orchestration=CEL_IF_DOC,
                home_root=root,
                inputs={"n": "5"},
            ),
            build_case(
                name="tree_dynamic",
                orchestration=TREE_DYNAMIC_DOC,
                home_root=root,
                # `TREE_DYNAMIC_DOC`'s own two children are named, not
                # iterated -- their declared order (`left` before
                # `right`) is this generator's own, not anything
                # recoverable from the events alone
                # (`_branch_sort_key`'s own doc comment).
                branch_order={"prime.fan_out": ["left", "right"]},
            ),
            build_case(
                name="json_parse_failure", orchestration=FAILURE_DOC, home_root=root
            ),
        ]

    text = render_corpus(cases)
    ok = write_or_check(OUTPUT, text, check=check)
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
