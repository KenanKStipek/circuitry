"""`cof doctor` must report the same effective adapter/model `cof run` would
actually use (#265 part 1): both go through `resolve_config()`'s SANE_DEFAULTS
layering, not the bare `load_config()` that leaves None/None with no config
file on disk.
"""

from __future__ import annotations

from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


def test_doctor_reports_sane_defaults_with_no_config_file():
    result = runner.invoke(app, ["doctor"])

    assert result.exit_code in (0, 1)  # may fail backend detection; that's fine
    assert "Effective adapter" in result.stdout
    assert "None (source: default)" not in result.stdout
    assert "ollama" in result.stdout
    assert "llama3.1:8b" in result.stdout
