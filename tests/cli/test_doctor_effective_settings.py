"""`cof doctor` must report the same effective adapter/model `cof run` would
actually use (#265 part 1): both go through `resolve_config()`'s SANE_DEFAULTS
layering, not the bare `load_config()` that leaves None/None with no config
file on disk.
"""

from __future__ import annotations

from pathlib import Path

import pytest
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


@pytest.fixture(autouse=True)
def _locked_down_doctor_env(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Hermetic per the lane contract: a bare `doctor` invocation must not
    depend on the developer's `CIRCUITRY_MODEL`/`CIRCUITRY_ADAPTER`/
    `CIRCUITRY_CONFIG`, the cwd's own project config, or reach any live
    service. Locking the allowlists to empty stops `_check_extensions` from
    building and `check()`-ing every compiled-in adapter — including
    `cyberdiner`, which makes a real network call to a paid production
    service whenever `CYBERDINER_TOKEN`/`CYBERDINER_EXPO_URL` happen to be
    set in the environment running the suite; their mere presence must never
    be enough on its own (#265 part 5 / CLAUDE.md)."""
    monkeypatch.delenv("CIRCUITRY_MODEL", raising=False)
    monkeypatch.delenv("CIRCUITRY_ADAPTER", raising=False)
    monkeypatch.delenv("CIRCUITRY_CONFIG", raising=False)
    monkeypatch.setenv("CIRCUITRY_ENABLED_ADAPTERS", "")
    monkeypatch.setenv("CIRCUITRY_ENABLED_TOOLS", "")
    monkeypatch.setenv("CIRCUITRY_ENABLED_PLUGINS", "")
    monkeypatch.chdir(tmp_path)


def test_doctor_reports_sane_defaults_with_no_config_file():
    result = runner.invoke(app, ["doctor"])

    assert result.exit_code in (0, 1)  # may fail backend detection; that's fine
    assert "Effective adapter" in result.stdout
    assert "None (source: default)" not in result.stdout
    assert "ollama" in result.stdout
    assert "llama3.1:8b" in result.stdout
