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
    assert_events_equal,
    assert_out_serialization,
    assert_states_equal,
    assert_values_equal,
    normalize_for_comparison,
    redact_runtime_strings,
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
    events_path = tmp_path / "events.jsonl"
    live_state_path = tmp_path / "live.json"
    leftover_replies_path = tmp_path / "leftover-replies.json"

    with harness.mock_http_server(case_dir, metadata) as server:
        replacements = harness.case_redaction_replacements(
            case_dir, mock_http_port=server.port if server else None
        )
        result = harness.run_case(
            case_dir,
            metadata,
            out_path=out_path,
            home_dir=home_dir,
            events_path=events_path,
            live_state_path=live_state_path,
            leftover_replies_path=leftover_replies_path,
            mock_http_port=server.port if server else None,
        )
        recorded_requests = list(server.requests) if server is not None else None

    harness.assert_no_leftover_replies(
        leftover_replies_path, case_dir=case_dir, metadata=metadata, case_name=case_dir.name
    )

    if metadata["expect"] == "success":
        assert result.returncode == 0, (
            f"expected success, got exit {result.returncode}\n"
            f"stdout: {result.stdout}\nstderr: {result.stderr}"
        )
        actual_text = out_path.read_text(encoding="utf-8")
        assert_out_serialization(actual_text, pretty=False)
        actual_state = json.loads(actual_text)
        expected_state = json.loads((case_dir / "expected.json").read_text(encoding="utf-8"))
        assert_states_equal(
            normalize_for_comparison(actual_state, replacements),
            normalize_for_comparison(expected_state, replacements),
        )

        expected_events_text = (case_dir / "expected.events.jsonl").read_text(encoding="utf-8")
        assert_events_equal(events_path.read_text(encoding="utf-8"), expected_events_text)

        # `--live-state`'s final write always ends equal to `--out`
        # (electricity/DESIGN.md §10.5, issue #431's acceptance criterion).
        assert live_state_path.read_bytes() == out_path.read_bytes(), (
            "--live-state's final write is not byte-identical to --out"
        )

        harness.assert_recorded_http_requests(
            case_dir, metadata, recorded_requests, replacements=replacements
        )

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
                normalize_for_comparison(actual_pretty_state, replacements),
                normalize_for_comparison(expected_pretty_state, replacements),
            )
            # Plain and --pretty of the same document carry the same value,
            # just laid out differently (§3.4.1).
            assert_values_equal(
                normalize_for_comparison(actual_state, replacements),
                normalize_for_comparison(actual_pretty_state, replacements),
            )
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
            redact_runtime_strings(actual_error, replacements),
            redact_runtime_strings(expected_error, replacements),
            byte_for_byte=error_compare == "exact",
            location_pattern=metadata.get("location_pattern"),
        )

        actual_out_text = out_path.read_text(encoding="utf-8")
        assert_out_serialization(actual_out_text, pretty=False)
        actual_out_state = json.loads(actual_out_text)
        expected_out_state = json.loads(
            (case_dir / "expected.out.json").read_text(encoding="utf-8")
        )
        assert_states_equal(
            normalize_for_comparison(actual_out_state, replacements),
            normalize_for_comparison(expected_out_state, replacements),
        )
        return

    raise AssertionError(f"{case_dir.name}: unknown case.json 'expect': {metadata['expect']!r}")
