"""Live-container integration test for the ``surrealdb`` runtime plugin.

Requires a reachable SurrealDB instance. Point it at one with
``SURREAL_URL`` (default ``ws://localhost:8000/rpc``) plus
``SURREAL_USER`` / ``SURREAL_PASS`` (default SurrealDB dev credentials
``root`` / ``root``), matching the CI service container this test is
meant to run against.

Not wired into any CI workflow yet — the runtime-plugin code and its
unit tests (``tests/runtime_plugins/test_surrealdb.py``) are covered
today via a fake SDK; this file is the live counterpart for whoever
adds the ``surrealdb/surrealdb`` service container to
``.github/workflows/quality.yml``.
"""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path

import pytest

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run

if os.getenv("CIRCUITRY_RUN_INTEGRATION") != "1":
    pytest.skip(
        "Integration tests disabled. Set CIRCUITRY_RUN_INTEGRATION=1 to enable.",
        allow_module_level=True,
    )

if importlib.util.find_spec("surrealdb") is None:
    pytest.skip(
        "surrealdb SDK not installed. `pip install circuitry-cof[surrealdb]`.",
        allow_module_level=True,
    )


def _write_orchestration(path: Path) -> None:
    path.write_text(
        """
adapter: openai
model: gpt-4o-mini
effects:
  - type: prompt
    name: greet
    template: "Say hello to {{input.name}} in a creative way."
""".strip()
        + "\n",
        encoding="utf-8",
    )


@pytest.mark.integration
def test_surrealdb_run_persists_one_run_and_n_effect_rows(tmp_path: Path) -> None:
    from surrealdb import Surreal  # type: ignore[import-not-found]

    url = os.getenv("SURREAL_URL", "ws://localhost:8000/rpc")
    namespace = os.getenv("SURREAL_NAMESPACE", "circuitry_test")
    database = os.getenv("SURREAL_DATABASE", "circuitry_test")
    user = os.getenv("SURREAL_USER", "root")
    password = os.getenv("SURREAL_PASS", "root")

    orch = tmp_path / "orch.yml"
    _write_orchestration(orch)

    cfg = CircuitryConfig(
        plugins=["circuitry.runtime_plugins.surrealdb"],
        runtime={
            "runtime_plugins": {
                "surrealdb": {
                    "url": url,
                    "namespace": namespace,
                    "database": database,
                    "user": user,
                    "password": password,
                },
            },
        },
    )

    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=False,
            initial_state={"input": {"name": "World"}},
            config=cfg,
        )
    )
    assert result.ok is True
    persisted_run_id = result.state["runtime"]["last_run"]["run_id"]

    client = Surreal(url)
    try:
        client.signin({"username": user, "password": password})
        client.use(namespace, database)

        run_rows = client.query(
            "SELECT * FROM runs WHERE run_id = $run_id", {"run_id": persisted_run_id}
        )
        effect_rows = client.query(
            "SELECT * FROM effects WHERE run_id = $run_id", {"run_id": persisted_run_id}
        )
    finally:
        client.close()

    assert len(run_rows) == 1
    assert run_rows[0]["status"] == "success"
    # One row for the leaf ``greet`` prompt plus one for the implicit root
    # dynamic itself (state_path "prime") — same shape the sqlite/clickhouse
    # persistence tests assert on. SurrealDB doesn't guarantee row order for
    # an unordered SELECT, so check membership rather than index 0.
    state_paths = {row["state_path"] for row in effect_rows}
    assert "prime.greet" in state_paths
    assert "prime" in state_paths
