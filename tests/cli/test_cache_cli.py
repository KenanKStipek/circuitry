"""`cof cache clear`/`cof cache stats` and `cof run --no-cache` (#270)."""

from __future__ import annotations

import json
from pathlib import Path

from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.core.step_cache import StepCache

runner = CliRunner()


def test_cache_stats_on_an_empty_cache() -> None:
    result = runner.invoke(app, ["cache", "stats", "--json"])
    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    assert payload == {"entries": 0, "bytes": 0, "path": payload["path"]}


def test_cache_stats_counts_entries() -> None:
    StepCache().put("k1", "v1", created_at="2026-01-01T00:00:00+00:00")
    result = runner.invoke(app, ["cache", "stats", "--json"])
    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    assert payload["entries"] == 1


def test_cache_clear_removes_entries_and_reports_the_count() -> None:
    StepCache().put("k1", "v1", created_at="2026-01-01T00:00:00+00:00")
    StepCache().put("k2", "v2", created_at="2026-01-01T00:00:00+00:00")
    result = runner.invoke(app, ["cache", "clear", "--json"])
    assert result.exit_code == 0, result.output
    assert json.loads(result.output) == {"removed": 2}
    assert StepCache().stats()["entries"] == 0


def test_run_no_cache_flag_writes_nothing(tmp_path: Path) -> None:
    orch = tmp_path / "o.yml"
    orch.write_text(
        """
effects:
  - type: tool
    name: step1
    provider: json
    params:
      op: parse
      input: '{"a": 1}'
    cache: true
""".lstrip(),
        encoding="utf-8",
    )
    result = runner.invoke(app, ["run", str(orch), "--no-cache", "--json"])
    assert result.exit_code == 0, result.output
    assert StepCache().stats()["entries"] == 0
