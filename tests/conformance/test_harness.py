"""Unit tests for `harness.load_case`'s `case.json` validation and for
`harness._sandboxed_env`'s hermeticity."""

from __future__ import annotations

import json
from pathlib import Path

import pytest
import yaml

from . import harness


def _write_case(tmp_path: Path, metadata: dict) -> Path:
    case_dir = tmp_path / "case"
    case_dir.mkdir()
    (case_dir / "case.json").write_text(json.dumps(metadata), encoding="utf-8")
    return case_dir


def test_load_case_fills_in_defaults(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"spec_case": "C0", "expect": "success"})
    metadata = harness.load_case(case_dir)
    assert metadata["orchestration"] == "orchestration.yml"
    assert metadata["engines"] == ["python"]
    assert metadata["cli_args"] == []


def test_load_case_rejects_unknown_key(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "engine": ["python"]})
    with pytest.raises(ValueError, match="unknown key"):
        harness.load_case(case_dir)


def test_load_case_requires_expect(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"spec_case": "C0"})
    with pytest.raises(ValueError, match="missing the required 'expect' key"):
        harness.load_case(case_dir)


def test_load_case_rejects_invalid_expect(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "Success"})
    with pytest.raises(ValueError, match="'expect' must be one of"):
        harness.load_case(case_dir)


def test_load_case_rejects_invalid_error_compare(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "failure", "error_compare": "Exact"})
    with pytest.raises(ValueError, match="'error_compare' must be one of"):
        harness.load_case(case_dir)


def test_load_case_rejects_invalid_engines(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "engines": ["electicity"]})
    with pytest.raises(ValueError, match="'engines' must be"):
        harness.load_case(case_dir)


def test_load_case_rejects_empty_engines(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "engines": []})
    with pytest.raises(ValueError, match="'engines' must be"):
        harness.load_case(case_dir)


def test_sandboxed_env_drops_credentials_and_circuitry_vars(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("OPENAI_API_KEY", "sk-leaked")
    monkeypatch.setenv("CIRCUITRY_MODEL", "x")
    monkeypatch.setenv("CIRCUITRY_CONFIG", "/nonexistent")
    case_dir = tmp_path / "case"
    case_dir.mkdir()
    home_dir = tmp_path / "home"

    env = harness._sandboxed_env(case_dir, home_dir)

    assert "OPENAI_API_KEY" not in env
    assert "CIRCUITRY_MODEL" not in env
    assert "CIRCUITRY_CONFIG" not in env
    assert env["HOME"] == str(home_dir)


def test_sandboxed_env_sets_leftover_replies_export_when_requested(tmp_path: Path) -> None:
    case_dir = tmp_path / "case"
    case_dir.mkdir()
    home_dir = tmp_path / "home"
    leftover_path = tmp_path / "leftover.json"

    env = harness._sandboxed_env(case_dir, home_dir, leftover_replies_path=leftover_path)

    assert env[harness.LEFTOVER_REPLIES_EXPORT_ENV_VAR] == str(leftover_path)


def test_load_case_rejects_malformed_known_divergence(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path,
        {
            "expect": "success",
            "engines": ["python", "electricity"],
            "known_divergence": {"location": "prime.x.value"},
        },
    )
    with pytest.raises(ValueError, match="'known_divergence' must be a mapping"):
        harness.load_case(case_dir)


def test_load_case_rejects_known_divergence_on_a_failure_case(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path,
        {
            "expect": "failure",
            "engines": ["python", "electricity"],
            "known_divergence": {"location": "prime.x.value", "electricity_value": 1},
        },
    )
    with pytest.raises(ValueError, match="only applies to a 'success' case"):
        harness.load_case(case_dir)


def test_load_case_rejects_known_divergence_with_one_engine(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path,
        {
            "expect": "success",
            "engines": ["python"],
            "known_divergence": {"location": "prime.x.value", "electricity_value": 1},
        },
    )
    with pytest.raises(ValueError, match="requires 'engines' to list both"):
        harness.load_case(case_dir)


def test_load_case_accepts_well_formed_known_divergence(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path,
        {
            "expect": "success",
            "engines": ["python", "electricity"],
            "known_divergence": {"location": "prime.x.value", "electricity_value": "other"},
        },
    )
    metadata = harness.load_case(case_dir)
    assert metadata["known_divergence"] == {
        "location": "prime.x.value",
        "electricity_value": "other",
    }


def test_load_case_rejects_non_bool_electricity_preview_ok(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path, {"expect": "success", "electricity_preview_ok": "yes"}
    )
    with pytest.raises(ValueError, match="'electricity_preview_ok' must be a bool"):
        harness.load_case(case_dir)


def test_materialize_cli_args_substitutes_mock_port() -> None:
    args = harness.materialize_cli_args(
        ["-e", f"url=http://127.0.0.1:{harness.MOCK_HTTP_PORT_PLACEHOLDER}/data"],
        mock_http_port=54321,
    )
    assert args == ["-e", "url=http://127.0.0.1:54321/data"]


def test_materialize_config_substitutes_mock_port(tmp_path: Path) -> None:
    case_dir = tmp_path / "case"
    case_dir.mkdir()
    (case_dir / "config.json").write_text(
        json.dumps(
            {
                "runtime": {
                    "adapters": {
                        "lmstudio": {
                            "base_url": f"http://127.0.0.1:{harness.MOCK_HTTP_PORT_PLACEHOLDER}/v1"
                        }
                    }
                }
            }
        ),
        encoding="utf-8",
    )
    materialized = harness.materialize_config(
        case_dir, "config.json", mock_http_port=54321, tmp_dir=tmp_path
    )
    data = json.loads(materialized.read_text(encoding="utf-8"))
    assert data["runtime"]["adapters"]["lmstudio"]["base_url"] == "http://127.0.0.1:54321/v1"


def test_unused_scripted_reply_fails_the_case(tmp_path: Path) -> None:
    """Issue #450's acceptance criterion: a deliberately unused scripted
    reply fails a case on the Python side. Runs a real `cof run` subprocess
    (the same way `test_python_runner.py` does) with a replies file that
    answers a path the document never asks, and asserts the harness's own
    leftover-replies export reports it -- exactly the check
    `test_python_runner.py::test_case` makes for every case."""
    case_dir = tmp_path / "case"
    case_dir.mkdir()
    (case_dir / "orchestration.yml").write_text(
        yaml.dump(
            {
                "name": "unused-reply",
                "adapter": "scripted",
                "model": "test",
                "effects": [{"type": "prompt", "name": "asked", "template": "hi"}],
            }
        ),
        encoding="utf-8",
    )
    (case_dir / "config.json").write_text(
        json.dumps(
            {"runtime": {"adapters": {"scripted": {"replies_file": "replies.yaml"}}}}
        ),
        encoding="utf-8",
    )
    (case_dir / "replies.yaml").write_text(
        yaml.dump(
            {
                "prime.asked": [{"text": "answered"}],
                "prime.never_asked": [{"text": "nobody wants this"}],
            }
        ),
        encoding="utf-8",
    )
    metadata = {
        "orchestration": "orchestration.yml",
        "config": "config.json",
        "cli_args": [],
        "timeout_seconds": harness.DEFAULT_TIMEOUT_SECONDS,
    }

    home_dir = tmp_path / "home"
    home_dir.mkdir()
    out_path = tmp_path / "out.json"
    leftover_replies_path = tmp_path / "leftover.json"

    result = harness.run_case(
        case_dir,
        metadata,
        out_path=out_path,
        home_dir=home_dir,
        leftover_replies_path=leftover_replies_path,
    )

    assert result.returncode == 0, result.stderr
    leftover = harness.read_leftover_replies(leftover_replies_path)
    assert leftover == {"prime.never_asked": 1}
    # This is exactly the condition `test_python_runner.py::test_case`
    # (and the electricity runner's own copy) assert `== {}` for every
    # case -- demonstrating it fails a case that leaves one unused.
    with pytest.raises(AssertionError):
        assert leftover == {}
