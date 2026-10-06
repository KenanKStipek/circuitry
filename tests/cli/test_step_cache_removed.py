"""The step cache (#336) is removed (#352): `cof cache clear|stats` and
`cof run --no-cache` no longer exist on the CLI.
"""

from __future__ import annotations

import json
from pathlib import Path

from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


def test_cache_subcommand_is_gone() -> None:
    result = runner.invoke(app, ["cache", "clear"])
    assert result.exit_code != 0

    result = runner.invoke(app, ["cache", "stats"])
    assert result.exit_code != 0


def test_run_no_cache_flag_is_gone(tmp_path: Path) -> None:
    orch = tmp_path / "noop.yml"
    orch.write_text(
        "effects:\n"
        "  - type: prompt\n"
        "    name: greet\n"
        '    template: "Hello, {{name}}."\n',
        encoding="utf-8",
    )
    out = tmp_path / "state.json"
    result = runner.invoke(
        app, ["run", str(orch), "--no-cache", "--dry-run", "--out", str(out)]
    )
    assert result.exit_code != 0
    assert "no such option" in result.output.lower() or "no such option" in str(
        result.exception
    ).lower()


def test_cache_key_on_a_prompt_effect_runs_with_an_unknown_key_warning(
    tmp_path: Path,
) -> None:
    """A document that still has `cache:` is not rejected \u2014 it dispatches
    normally, as any other unrecognized key does (#352)."""
    orch = tmp_path / "with_cache.yml"
    orch.write_text(
        "effects:\n"
        "  - type: prompt\n"
        "    name: greet\n"
        '    template: "Hello, {{name}}."\n'
        "    cache: true\n",
        encoding="utf-8",
    )
    out = tmp_path / "state.json"
    result = runner.invoke(app, ["run", str(orch), "--dry-run", "--out", str(out)])
    assert result.exit_code == 0, result.output
    state = json.loads(out.read_text(encoding="utf-8"))
    assert "cache" not in state.get("prime", {}).get("greet", {}).get("meta", {})
