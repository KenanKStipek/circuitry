#!/usr/bin/env python3
"""Golden `--out`/stdout for `-e` seeding's own success-path corner cases
(issue #431's run-wiring step 4/10; PR #441 review finding 1), built on
`_run_corpus.py` the same way `generate_vm_run_failures.py` is.

`generate_vm_run_failures.py` covers finding 1b/1c (a pre-execution
failure's own partial `state["input"]`); this generator covers 1a (an
optional, `null`-valued `-e` input must not survive into a *successful*
run's own `state["input"]`) and 1d (`-e` keys that are not lifted under
`input` at all -- a namespace name or `_`-prefixed key, or `extra` when
an `-e input=...` entry already wins outright -- must still land at the
state root, exactly as `core/state_ns.py::migrate_legacy_state` leaves
them).

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI). Usage:
    python3 generate_vm_run_seeding.py [--check]
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
    / "run_seeding.json"
)

# `type: integer`, not `string` -- a declared `type: string` input's own
# raw `-e` text is restored before this even reaches `build_input_
# namespace` (`cli_inline_entries`'s own job), so `-e x=null` would stay
# the literal string `"null"`, never `None`/absent at all. Only a
# non-string-typed, optional, no-`default` input actually exercises
# `apply_declared_inputs`' own "absent and no default -> drop the key"
# branch this case is about.
OPTIONAL_STRING_INPUT_DOC = """\
interface:
  inputs:
    x:
      type: integer
effects: []
"""

NO_DECLARED_INPUTS_DOC = """\
effects: []
"""


def build_case(
    *,
    name: str,
    orchestration: str,
    home_root: Path,
    inputs: dict[str, str] | None = None,
) -> dict:
    case_dir = home_root / name
    case_dir.mkdir(parents=True)
    home_dir = case_dir / "home"
    home_dir.mkdir()
    (case_dir / "orchestration.yml").write_text(orchestration, encoding="utf-8")

    result = run_cof(case_dir, inputs=inputs, home_dir=home_dir)
    normalized = normalize_result(result, str(case_dir.resolve()))
    return {
        "name": name,
        "orchestration": orchestration,
        "config": None,
        "inputs": inputs or {},
        "result": normalized,
    }


def main() -> int:
    check = "--check" in sys.argv[1:]

    with tempfile.TemporaryDirectory(prefix="electricity-vm-run-seeding-") as tmp:
        root = Path(tmp).resolve()
        cases = [
            # 1a: `x` is declared but not required and has no default --
            # a `null` seed is dropped entirely, not left behind as a
            # leftover `null` leaf once `check_interface_inputs` has run.
            build_case(
                name="e_null_optional_input_is_dropped_on_success",
                orchestration=OPTIONAL_STRING_INPUT_DOC,
                home_root=root,
                inputs={"x": "null"},
            ),
            # 1d-i: `_tag` is `_`-prefixed -- never lifted under `input`
            # at all, so it must still land at the state root.
            build_case(
                name="e_underscore_prefixed_key_stays_at_the_root",
                orchestration=NO_DECLARED_INPUTS_DOC,
                home_root=root,
                inputs={"_tag": "x"},
            ),
            # 1d-ii: an explicit `-e input=...` wins outright
            # (`migrate_legacy_state`'s own early return of the whole
            # dict, untouched) -- a sibling `-e extra=...` is never
            # lifted into `input` either, and must stay at the root.
            build_case(
                name="e_input_key_wins_outright_extra_key_stays_at_root",
                orchestration=NO_DECLARED_INPUTS_DOC,
                home_root=root,
                inputs={"input": '{"a": 1}', "extra": "2"},
            ),
        ]

    text = render_corpus(cases)
    ok = write_or_check(OUTPUT, text, check=check)
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
