from __future__ import annotations

import os
import shutil
import sqlite3
import subprocess
from pathlib import Path

import pytest

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run


if os.getenv("CIRCUITRY_RUN_INTEGRATION") != "1":
    pytest.skip(
        "Integration tests disabled. Set CIRCUITRY_RUN_INTEGRATION=1 to enable.",
        allow_module_level=True,
    )


def _has_ollama_model(model: str) -> bool:
    if shutil.which("ollama") is None:
        return False
    proc = subprocess.run(
        ["ollama", "list"],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        return False
    return model in proc.stdout


def _write_orchestration(path: Path) -> None:
    path.write_text(
        """
adapter: ollama
effects:
  - type: prompt
    name: echo
    template: "Echo exactly: {{input.message}}"
""".strip()
        + "\n",
        encoding="utf-8",
    )


@pytest.mark.integration
def test_sqlite_persistence_round_trip_with_local_ollama_inference(
    tmp_path: Path,
) -> None:
    model = os.getenv("CIRCUITRY_INTEGRATION_MODEL", "smollm2:135m")
    if not _has_ollama_model(model):
        pytest.skip(
            "Required local model not available in Ollama. "
            f"Pull '{model}' or set CIRCUITRY_INTEGRATION_MODEL to an installed model."
        )

    db_path = tmp_path / "state.db"
    orch = tmp_path / "orch.yml"
    _write_orchestration(orch)

    cfg = CircuitryConfig(
        default_adapter="ollama",
        default_model=model,
        runtime={
            "adapters": {
                "ollama": {"base_url": os.getenv("OLLAMA_BASE_URL", "http://localhost:11434")}
            },
            "persistence": {
                "enabled": True,
                "backend": "sqlite",
                "db_path": str(db_path),
                "table": "circuitry_runs",
            },
        },
    )

    first = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={"input": {"message": "ping"}},
            config=cfg,
        )
    )
    assert first.ok is True
    assert isinstance(first.state.get("prime", {}).get("echo", {}).get("value"), str)
    assert first.state["runtime"]["persistence"]["status"] == "persisted"

    second = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state=None,
            config=cfg,
        )
    )
    assert second.ok is True
    assert second.state["input"]["message"] == "ping"
    assert second.state["runtime"]["persistence"]["loaded_from_persistence"] is True

    with sqlite3.connect(db_path) as conn:
        rows = conn.execute("SELECT COUNT(*) FROM circuitry_runs").fetchone()
    assert rows is not None
    assert int(rows[0]) >= 2
