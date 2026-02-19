from __future__ import annotations

from copy import deepcopy
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import circuitry.cli.runtime_shim as runtime_shim
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run


@dataclass
class FakePersistenceBackend:
    store_by_orchestration: dict[str, dict[str, Any]] = field(default_factory=dict)
    fail_load: bool = False
    fail_save: bool = False

    @property
    def backend_name(self) -> str:
        return "postgres"

    def describe(self) -> dict[str, Any]:
        return {"backend": "postgres", "table": "circuitry_runs", "sslmode": "require"}

    def load_latest_state(self, *, orchestration_path: str) -> dict[str, Any] | None:
        if self.fail_load:
            raise RuntimeError("db load down")
        existing = self.store_by_orchestration.get(orchestration_path)
        return deepcopy(existing) if existing is not None else None

    def save_run_snapshot(
        self,
        *,
        orchestration_path: str,
        run_id: str,
        ok: bool,
        error: str | None,
        state: dict[str, Any],
    ) -> None:
        del run_id, ok, error
        if self.fail_save:
            raise RuntimeError("db write down")
        self.store_by_orchestration[orchestration_path] = deepcopy(state)


def _write(tmp_path: Path, name: str, content: str) -> Path:
    path = tmp_path / name
    path.write_text(content, encoding="utf-8")
    return path


def test_persistence_round_trip_hydrates_and_persists_state(
    tmp_path: Path,
    monkeypatch: Any,
) -> None:
    backend = FakePersistenceBackend()
    monkeypatch.setattr(
        runtime_shim,
        "build_persistence_backend",
        lambda runtime: backend,
    )

    orch = _write(
        tmp_path,
        "orch.yml",
        """
adapter: openai
model: gpt-4o-mini
effects:
  - type: prompt
    name: greet
    template: "hello {{input.user}}"
""".strip()
        + "\n",
    )

    cfg = CircuitryConfig(runtime={"persistence": {"enabled": True, "backend": "postgres", "dsn": "postgres://demo"}})

    first = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=False,
            initial_state={"input": {"user": "Ada"}},
            config=cfg,
        )
    )
    assert first.ok is True
    assert first.state["runtime"]["persistence"]["persisted"] is True
    assert first.state["runtime"]["persistence"]["loaded_from_persistence"] is False

    second = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=False,
            initial_state=None,
            config=cfg,
        )
    )
    assert second.ok is True
    assert second.state["input"]["user"] == "Ada"
    assert second.state["runtime"]["persistence"]["loaded_from_persistence"] is True
    assert second.state["runtime"]["persistence"]["status"] == "persisted"


def test_persistence_load_failure_is_reported_with_metadata(
    tmp_path: Path,
    monkeypatch: Any,
) -> None:
    backend = FakePersistenceBackend(fail_load=True)
    monkeypatch.setattr(runtime_shim, "build_persistence_backend", lambda runtime: backend)

    orch = _write(
        tmp_path,
        "orch.yml",
        """
adapter: openai
model: gpt-4o-mini
effects:
  - type: prompt
    name: greet
    template: "hello"
""".strip()
        + "\n",
    )
    cfg = CircuitryConfig(runtime={"persistence": {"enabled": True, "backend": "postgres", "dsn": "postgres://demo"}})

    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=False,
            initial_state=None,
            config=cfg,
        )
    )

    assert result.ok is False
    assert "Failed to load persisted state" in (result.error or "")
    persistence = result.state["runtime"]["persistence"]
    assert persistence["status"] == "load_failed"
    assert "db load down" in persistence["error"]


def test_persistence_save_failure_is_reported_with_metadata(
    tmp_path: Path,
    monkeypatch: Any,
) -> None:
    backend = FakePersistenceBackend(fail_save=True)
    monkeypatch.setattr(runtime_shim, "build_persistence_backend", lambda runtime: backend)

    orch = _write(
        tmp_path,
        "orch.yml",
        """
adapter: openai
model: gpt-4o-mini
effects:
  - type: prompt
    name: greet
    template: "hello"
""".strip()
        + "\n",
    )
    cfg = CircuitryConfig(runtime={"persistence": {"enabled": True, "backend": "postgres", "dsn": "postgres://demo"}})

    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=False,
            initial_state={"input": {"user": "Ada"}},
            config=cfg,
        )
    )

    assert result.ok is False
    assert "Failed to persist runtime state" in (result.error or "")
    persistence = result.state["runtime"]["persistence"]
    assert persistence["status"] == "save_failed"
    assert "db write down" in persistence["error"]
