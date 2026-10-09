#!/usr/bin/env python3
"""Golden `--out`/stdout for every one of `electricity::run_orchestration`'s
own pre-execution failure paths (issue #431's run-wiring steps 5-14; PR
#441 review finding 9), built on `_run_corpus.py` the same way
`generate_vm_run_corpus.py` is.

Each case is a real `cof run --out --events --live-state --config ...`
against a document (and, for two cases, a config file) deliberately
shaped to fail at one specific step -- never `execute_root` itself,
which is exercised by `generate_vm_run_corpus.py`'s own cases instead.
`electricity-config`'s own `generate_config_corpus.py` already covers
`resolve_config`/`effective_settings`/`validate_complexity`/
`validate_persistence` at the unit level (defaults/user-file/env-overlay
included); this generator is about the *run-level* `--out` shape each
of those failures (and the structural/compile/group ones that crate
doesn't reach at all) actually leaves behind, not the error text alone.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI). Usage:
    python3 generate_vm_run_failures.py [--check]
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
    / "run_failures.json"
)

LOAD_ERROR_DOC = """\
effects: []
effects: []
"""

EFFECTIVE_SETTINGS_SHAPE_ERROR_DOC = """\
runtime: "not an object"
effects: []
"""

COMPLEXITY_ERROR_DOC = """\
runtime:
  complexity:
    routing:
      enabled: true
effects: []
"""

CONCURRENCY_ERROR_DOC = """\
runtime:
  max_concurrency: -1
effects: []
"""

PERSISTENCE_VALIDATION_ERROR_DOC = """\
runtime:
  persistence:
    enabled: true
    backend: sqlite
effects: []
"""

MISSING_REQUIRED_INPUT_DOC = """\
interface:
  inputs:
    name:
      type: string
      required: true
effects: []
"""

STRUCTURAL_ERROR_DOC = """\
{}
"""

DUPLICATE_EFFECT_NAME_DOC = """\
effects:
  - name: dup
    type: tool
    provider: json
    params: {mode: stringify, input: {}}
  - name: dup
    type: tool
    provider: json
    params: {mode: stringify, input: {}}
"""

UNKNOWN_GROUP_DOC = """\
effects:
  - name: a
    type: tool
    provider: json
    group: no-such-group
    params: {mode: stringify, input: {}}
"""


def build_case(*, name: str, orchestration: str, home_root: Path) -> dict:
    case_dir = home_root / name
    case_dir.mkdir(parents=True)
    home_dir = case_dir / "home"
    home_dir.mkdir()
    (case_dir / "orchestration.yml").write_text(orchestration, encoding="utf-8")

    result = run_cof(case_dir, home_dir=home_dir)
    normalized = normalize_result(result, str(case_dir.resolve()))
    return {"name": name, "result": normalized}


def main() -> int:
    check = "--check" in sys.argv[1:]

    with tempfile.TemporaryDirectory(prefix="electricity-vm-run-failures-") as tmp:
        root = Path(tmp).resolve()
        cases = [
            build_case(name="load_error", orchestration=LOAD_ERROR_DOC, home_root=root),
            build_case(
                name="effective_settings_shape_error",
                orchestration=EFFECTIVE_SETTINGS_SHAPE_ERROR_DOC,
                home_root=root,
            ),
            build_case(
                name="complexity_error", orchestration=COMPLEXITY_ERROR_DOC, home_root=root
            ),
            build_case(
                name="concurrency_error", orchestration=CONCURRENCY_ERROR_DOC, home_root=root
            ),
            build_case(
                name="persistence_validation_error",
                orchestration=PERSISTENCE_VALIDATION_ERROR_DOC,
                home_root=root,
            ),
            build_case(
                name="missing_required_input",
                orchestration=MISSING_REQUIRED_INPUT_DOC,
                home_root=root,
            ),
            build_case(
                name="structural_error", orchestration=STRUCTURAL_ERROR_DOC, home_root=root
            ),
            build_case(
                name="duplicate_effect_name",
                orchestration=DUPLICATE_EFFECT_NAME_DOC,
                home_root=root,
            ),
            build_case(
                name="unknown_group", orchestration=UNKNOWN_GROUP_DOC, home_root=root
            ),
        ]

    text = render_corpus(cases)
    ok = write_or_check(OUTPUT, text, check=check)
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
