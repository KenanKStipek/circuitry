"""Runs the Python engine (`cof run`) fresh on every conformance case and
compares it with its committed `expected.json` under normalization
(electricity/DESIGN.md §12). Proves both determinism (the same document run
again produces the same normalized state) and the normalizer itself.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from . import harness
from .normalize import (
    assert_errors_equal,
    assert_out_serialization,
    assert_states_equal,
    assert_values_equal,
    normalize,
)

CASE_DIRS = harness.list_case_dirs()


def _case_ids() -> list[str]:
    return [d.name for d in CASE_DIRS]


@pytest.mark.parametrize("case_dir", CASE_DIRS, ids=_case_ids())
def test_case(case_dir: Path, tmp_path: Path) -> None:
    metadata = harness.load_case(case_dir)
    if "python" not in metadata["engines"]:
        pytest.skip(f"{case_dir.name}: does not apply to the python engine")

    home_dir = tmp_path / "home"
    home_dir.mkdir()
    out_path = tmp_path / "out.json"
    result = harness.run_case(case_dir, metadata, out_path=out_path, home_dir=home_dir)

    if metadata["expect"] == "success":
        assert result.returncode == 0, (
            f"expected success, got exit {result.returncode}\n"
            f"stdout: {result.stdout}\nstderr: {result.stderr}"
        )
        actual_text = out_path.read_text(encoding="utf-8")
        assert_out_serialization(actual_text, pretty=False)
        actual_state = json.loads(actual_text)
        expected_state = json.loads((case_dir / "expected.json").read_text(encoding="utf-8"))
        assert_states_equal(normalize(actual_state), normalize(expected_state))

        if metadata.get("also_pretty"):
            pretty_home = tmp_path / "home-pretty"
            pretty_home.mkdir()
            pretty_out = tmp_path / "out.pretty.json"
            pretty_result = harness.run_case(
                case_dir, metadata, out_path=pretty_out, home_dir=pretty_home, pretty=True
            )
            assert pretty_result.returncode == 0, (
                f"expected success (--pretty), got exit {pretty_result.returncode}\n"
                f"stdout: {pretty_result.stdout}\nstderr: {pretty_result.stderr}"
            )
            actual_pretty_text = pretty_out.read_text(encoding="utf-8")
            assert_out_serialization(actual_pretty_text, pretty=True)
            actual_pretty_state = json.loads(actual_pretty_text)
            expected_pretty_state = json.loads(
                (case_dir / "expected.pretty.json").read_text(encoding="utf-8")
            )
            assert_states_equal(
                normalize(actual_pretty_state), normalize(expected_pretty_state)
            )
            # Plain and --pretty of the same document carry the same value,
            # just laid out differently (§3.4.1).
            assert_values_equal(normalize(actual_state), normalize(actual_pretty_state))
        return

    if metadata["expect"] == "failure":
        assert result.returncode != 0, (
            f"expected a load/check failure, but exited 0\nstdout: {result.stdout}"
        )
        actual_error = harness.parse_cli_error(result.stdout)
        expected_error = json.loads((case_dir / "expected.json").read_text(encoding="utf-8"))[
            "error"
        ]
        error_compare = metadata.get("error_compare", "exact")
        assert_errors_equal(
            actual_error,
            expected_error,
            byte_for_byte=error_compare == "exact",
            location_pattern=metadata.get("location_pattern"),
        )
        return

    raise AssertionError(f"{case_dir.name}: unknown case.json 'expect': {metadata['expect']!r}")
