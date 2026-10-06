"""The step cache (#336) is removed (#352): `cof cache clear|stats` and
`cof run --no-cache` no longer exist on the CLI.
"""

from __future__ import annotations

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


def test_cache_key_on_a_prompt_effect_gives_an_unknown_key_warning_not_an_error(
    tmp_path: Path,
) -> None:
    """A document that still has `cache:` is not rejected \u2014 `cof check`
    reports it as an ordinary unrecognized-key warning, as it does for any
    other unknown key (#352)."""
    orch = tmp_path / "with_cache.yml"
    orch.write_text(
        "effects:\n"
        "  - type: prompt\n"
        "    name: greet\n"
        '    template: "Hello, {{name}}."\n'
        "    cache: true\n",
        encoding="utf-8",
    )
    result = runner.invoke(app, ["check", str(orch), "--skip-preflight"])
    assert result.exit_code == 0, result.output
    assert "unknown key 'cache'" in result.output


def test_cache_key_on_a_use_effect_is_also_only_a_warning(tmp_path: Path) -> None:
    """`cache:` on a container used to be a hard `cache_field_errors`
    failure (#270); it is now just an unrecognized key everywhere (#352)."""
    child = tmp_path / "child.yml"
    child.write_text(
        "effects:\n  - type: prompt\n    name: inner\n    template: hi\n",
        encoding="utf-8",
    )
    orch = tmp_path / "with_cache_on_use.yml"
    orch.write_text(
        "effects:\n"
        "  - type: use\n"
        "    name: child\n"
        f"    path: {child.name}\n"
        "    cache: true\n",
        encoding="utf-8",
    )
    result = runner.invoke(app, ["check", str(orch), "--skip-preflight"])
    assert result.exit_code == 0, result.output
    assert "unknown key 'cache'" in result.output
