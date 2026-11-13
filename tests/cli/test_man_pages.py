from __future__ import annotations

import shutil
import subprocess
import tempfile
from pathlib import Path

import click
import pytest
import typer.main
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.cli.man_pages import all_page_names, render_page

runner = CliRunner()

REPO_ROOT = Path(__file__).resolve().parents[2]
MAN1 = REPO_ROOT / "man" / "man1"


def _committed_path(name: str) -> Path:
    return MAN1 / ("cof.1" if name == "cof" else f"cof-{name}.1")


@pytest.mark.parametrize("name", all_page_names())
def test_committed_page_matches_render_page(name: str) -> None:
    """man/man1/cof*.1 (issue #764) are committed snapshots of exactly
    what render_page computes -- the same thing cof man itself calls.
    A page added or changed without regenerating fails this."""
    path = _committed_path(name)
    assert path.exists(), f"{path} is missing -- run scripts/generate-man-pages.py"
    assert path.read_text(encoding="utf-8") == render_page(name), (
        f"{path} is stale -- run scripts/generate-man-pages.py"
    )


@pytest.mark.parametrize("name", all_page_names())
def test_page_renders_cleanly_with_mandoc(name: str) -> None:
    mandoc = shutil.which("mandoc")
    if mandoc is None:
        pytest.skip("mandoc not installed")
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        man1 = tmp_path / "man1"
        man1.mkdir()
        for other in all_page_names():
            (man1 / ("cof.1" if other == "cof" else f"cof-{other}.1")).write_text(
                render_page(other), encoding="utf-8"
            )
        target = man1 / ("cof.1" if name == "cof" else f"cof-{name}.1")
        result = subprocess.run(
            [mandoc, "-Tlint", str(target.relative_to(tmp_path))],
            cwd=tmp_path,
            capture_output=True,
            text=True,
            check=False,
        )
        if name == "check":
            # `.so man1/cof-validate.1` (issue #764 rule 1) is the
            # conventional alias mechanism man(1) itself uses --
            # mandoc's own suggestion (`ln(1)` instead) would break
            # the one-source-of-truth guarantee the alias exists for.
            assert "so is fragile" in result.stdout
            assert result.returncode in (0, 2)
            return
        assert result.returncode == 0, result.stderr


def _click_root() -> click.Group:
    return typer.main.get_command(app)


def _all_commands_and_options() -> list[tuple[str, str]]:
    """(command_name, flag) for every option every top-level command (and
    `library refresh`) defines -- what test_every_option_appears_in_its_page
    checks against each page's own text."""
    root = _click_root()
    pairs: list[tuple[str, str]] = []
    for name, cmd in root.commands.items():
        if name == "library":
            pairs.extend(
                ("library", param.opts[0])
                for flag_cmd in cmd.commands.values()
                for param in flag_cmd.params
                if isinstance(param, click.Option)
            )
            continue
        pairs.extend(
            (name, param.opts[0]) for param in cmd.params if isinstance(param, click.Option)
        )
    return pairs


@pytest.mark.parametrize("name,flag", _all_commands_and_options())
def test_every_option_appears_in_its_page(name: str, flag: str) -> None:
    page = render_page(name)
    assert flag in page, f"{flag} is missing from cof-{name}(1)"


def test_every_top_level_command_appears_on_the_cof_page() -> None:
    root = _click_root()
    page = render_page("cof")
    for name in root.commands:
        assert name in page, f"{name} is missing from cof(1)'s own COMMANDS section"


def test_man_defaults_to_the_cof_page() -> None:
    result = runner.invoke(app, ["man"])
    assert result.exit_code == 0
    assert ".TH COF 1" in result.stdout


def test_man_shows_a_command_page() -> None:
    result = runner.invoke(app, ["man", "run"])
    assert result.exit_code == 0
    assert ".TH COF-RUN 1" in result.stdout


def test_man_check_shows_the_so_alias() -> None:
    result = runner.invoke(app, ["man", "check"])
    assert result.exit_code == 0
    assert result.stdout == ".so man1/cof-validate.1\n"


def test_man_roff_flag_prints_roff() -> None:
    result = runner.invoke(app, ["man", "run", "--roff"])
    assert result.exit_code == 0
    assert result.stdout.startswith(".TH COF-RUN 1")


@pytest.mark.parametrize("name", all_page_names())
def test_page_cites_no_issue_numbers_or_design_docs(name: str) -> None:
    """Help text is documentation too (issue #764): no issue numbers
    or design-doc section references, in the generated page or the
    hand-written sections behind it."""
    import re

    page = render_page(name)
    assert not re.search(r"#\d{2,}", page), f"cof-{name}(1) cites an issue number"
    assert "DESIGN.md" not in page


def test_man_unknown_command_exits_2_and_lists_pages() -> None:
    result = runner.invoke(app, ["man", "bogus"])
    assert result.exit_code == 2
    assert "No such page" in result.stdout
    assert "run" in result.stdout
    assert "validate" in result.stdout
