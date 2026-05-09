"""Verify credential-bearing fields are redacted in serialized state.

The runtime keeps the un-redacted dict for adapter construction, but the
snapshot embedded under `state["runtime"]["effective_settings"]` (which flows
into `--out`, `--json`, `--live-state`, and `last-run.json`) must be redacted.
"""

from __future__ import annotations

from pathlib import Path

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.redaction import REDACTED, redact
from circuitry.cli.runtime_shim import RunRequest, run


def _write(tmp_path: Path, name: str, content: str) -> Path:
    path = tmp_path / name
    path.write_text(content, encoding="utf-8")
    return path


_NOOP_ORCH = """
effects:
  - type: dynamic
    name: noop
    flow: chain
    effects: []
""".strip() + "\n"


def _embedded_runtime(state: dict) -> dict:
    return state["runtime"]["effective_settings"]["runtime"]


def test_api_key_is_redacted_in_embedded_state(tmp_path: Path) -> None:
    cfg = CircuitryConfig(
        default_adapter="openai",
        runtime={
            "adapters": {
                "openai": {
                    "base_url": "https://api.openai.com/v1",
                    "api_key": "sk-test-1234567890abcdef1234567890abcdef",
                    "timeout_seconds": 30,
                }
            }
        },
    )
    orch_path = _write(tmp_path, "noop.yml", _NOOP_ORCH)

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=True,
            config=cfg,
        )
    )
    assert result.ok is True

    embedded = _embedded_runtime(result.state)
    openai_cfg = embedded["adapters"]["openai"]
    assert openai_cfg["api_key"] == REDACTED
    assert openai_cfg["base_url"] == "https://api.openai.com/v1"
    assert openai_cfg["timeout_seconds"] == 30


def test_base_url_with_userinfo_is_redacted(tmp_path: Path) -> None:
    cfg = CircuitryConfig(
        default_adapter="openai",
        runtime={
            "adapters": {
                "openai": {
                    "base_url": "https://user:pass@host.example.com/v1",
                }
            }
        },
    )
    orch_path = _write(tmp_path, "noop.yml", _NOOP_ORCH)

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=True,
            config=cfg,
        )
    )
    assert result.ok is True

    embedded_url = _embedded_runtime(result.state)["adapters"]["openai"]["base_url"]
    # userinfo segment is gone, host is preserved
    assert "user:pass" not in embedded_url
    assert "host.example.com" in embedded_url
    assert REDACTED in embedded_url


def test_benign_runtime_passes_through_unchanged(tmp_path: Path) -> None:
    cfg = CircuitryConfig(
        default_adapter="ollama",
        runtime={
            "adapters": {
                "ollama": {
                    "base_url": "http://localhost:11434",
                    "default_model": "llama3.1:8b",
                    "timeout_seconds": 60,
                }
            }
        },
    )
    orch_path = _write(tmp_path, "noop.yml", _NOOP_ORCH)

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=True,
            config=cfg,
        )
    )
    assert result.ok is True

    embedded = _embedded_runtime(result.state)
    ollama_cfg = embedded["adapters"]["ollama"]
    assert ollama_cfg["base_url"] == "http://localhost:11434"
    assert ollama_cfg["default_model"] == "llama3.1:8b"
    assert ollama_cfg["timeout_seconds"] == 60


def test_live_runtime_dict_is_not_mutated(tmp_path: Path) -> None:
    """Sanity check: redaction must not corrupt the live dict the adapter uses."""
    secret = "sk-test-1234567890abcdef1234567890abcdef"
    runtime_dict = {"adapters": {"openai": {"api_key": secret}}}
    cfg = CircuitryConfig(default_adapter="openai", runtime=runtime_dict)
    orch_path = _write(tmp_path, "noop.yml", _NOOP_ORCH)

    run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=True,
            config=cfg,
        )
    )

    # The original dict is intact for adapter construction in real runs.
    assert runtime_dict["adapters"]["openai"]["api_key"] == secret


def test_redact_helper_handles_nested_lists_and_keys() -> None:
    payload = {
        "adapters": [
            {"name": "openai", "api_key": "sk-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
            {"name": "anthropic", "x_api_key": "long-token-string-1234567890abcd"},
        ],
        "headers": {"Authorization": "Bearer abc.def.ghi"},
        "ok": True,
        "url": "postgres://user:pw@db.example/circuitry",
    }
    out = redact(payload)
    assert out["adapters"][0]["api_key"] == REDACTED
    assert out["adapters"][1]["x_api_key"] == REDACTED
    assert out["headers"]["Authorization"] == REDACTED
    assert out["ok"] is True
    assert "user:pw" not in out["url"]
    assert "db.example" in out["url"]
