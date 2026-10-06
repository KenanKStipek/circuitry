"""The step cache (#336) is removed (#352), but a developer's existing
`--last` stash written before this change can still carry a `no_cache` key
(`cof run --no-cache` used to persist it). `cof run --last` must still load
and replay that stash rather than choke on an unknown key.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from circuitry.cli import app as app_module
from circuitry.cli.app import app

runner = CliRunner()


def _redirect_global_config(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> Path:
    fake_dir = tmp_path / "config-home"
    fake_dir.mkdir(parents=True, exist_ok=True)
    fake_last_run = fake_dir / "last-run.json"
    monkeypatch.setattr(app_module, "GLOBAL_CONFIG_DIR", fake_dir)
    monkeypatch.setattr(app_module, "_LAST_RUN_PATH", fake_last_run)
    return fake_last_run


def _write_noop(tmp_path: Path) -> Path:
    orch = tmp_path / "noop.yml"
    orch.write_text(
        "effects:\n"
        "  - type: prompt\n"
        "    name: greet\n"
        '    template: "Hello, {{name}}."\n',
        encoding="utf-8",
    )
    return orch


@pytest.mark.parametrize("stashed_no_cache", [True, False])
def test_last_replay_loads_a_stash_with_a_legacy_no_cache_key(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, stashed_no_cache: bool
) -> None:
    last_run_path = _redirect_global_config(monkeypatch, tmp_path)
    orch_path = _write_noop(tmp_path)

    last_run_path.write_text(
        json.dumps(
            {
                "orchestration": str(orch_path),
                "config": None,
                "state": None,
                "out": None,
                "pretty": False,
                "print_state": False,
                "dry_run": True,
                "json_out": False,
                "quiet": True,
                "verbose": False,
                "live_state": None,
                "env_vars": ["name=World"],
                "tail": False,
                "no_cache": stashed_no_cache,
            }
        )
        + "\n",
        encoding="utf-8",
    )

    result = runner.invoke(app, ["run", "--last"])
    assert result.exit_code == 0, result.output
