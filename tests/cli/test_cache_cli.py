"""`cof cache clear`/`cof cache stats` and `cof run --no-cache` (#270)."""

from __future__ import annotations

import json
from pathlib import Path

from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.core.step_cache import StepCache

runner = CliRunner()

# Real entry names are a sha256 hex digest (what `compute_cache_key` always
# returns) — `clear`/`stats` only match that shape.
_KEY_A = "a" * 64
_KEY_B = "b" * 64


def test_cache_stats_on_an_empty_cache() -> None:
    result = runner.invoke(app, ["cache", "stats", "--json"])
    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    assert payload == {"entries": 0, "bytes": 0, "path": payload["path"]}


def test_cache_stats_counts_entries() -> None:
    StepCache().put(_KEY_A, "v1", created_at="2026-01-01T00:00:00+00:00")
    result = runner.invoke(app, ["cache", "stats", "--json"])
    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    assert payload["entries"] == 1


def test_cache_clear_removes_entries_and_reports_the_count() -> None:
    StepCache().put(_KEY_A, "v1", created_at="2026-01-01T00:00:00+00:00")
    StepCache().put(_KEY_B, "v2", created_at="2026-01-01T00:00:00+00:00")
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


def test_run_last_does_not_override_an_explicit_no_cache(tmp_path: Path) -> None:
    """`cof run --last` replays the previous run's flags, but an explicit
    `--no-cache` on *this* invocation must still win — #270 review finding 6:
    dropping it would turn "bypass the cache" into a cache hit the user
    explicitly asked not to get."""
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
    out1 = tmp_path / "out1.json"
    first = runner.invoke(app, ["run", str(orch), "--out", str(out1)])
    assert first.exit_code == 0, first.output
    assert StepCache().stats()["entries"] == 1

    # Tamper with the one stored entry so a read would be observable.
    cache = StepCache()
    entry = next(iter(cache._root.glob("*.json")))
    key = entry.stem
    cache.put(key, "tampered", created_at="2026-01-01T00:00:00+00:00")

    # --last replays every flag including the stashed --out (out1), so no
    # --out is passed here — the replayed run's state lands back at out1.
    second = runner.invoke(app, ["run", "--last", "--no-cache"])
    assert second.exit_code == 0, second.output
    state = json.loads(out1.read_text(encoding="utf-8"))
    # Dispatched for real, not the tampered cached value.
    assert state["prime"]["step1"]["value"] == {"a": 1}
    assert "cache" not in state["prime"]["step1"]["meta"]
    # --no-cache on this invocation: still never written.
    assert cache.get(key, ttl_seconds=None).value == "tampered"
