from __future__ import annotations

import re
import shutil
import subprocess
from pathlib import Path

import click
import pytest
import typer.main
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.cli.man_pages import ALIASES, COMMAND_ORDER, all_page_names, render_page

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


@pytest.mark.parametrize("name", sorted(p.name for p in MAN1.glob("*.1")))
def test_every_committed_page_lints_cleanly(name: str) -> None:
    """Lints the actual committed `man/man1/*.1` tree -- cof, electricity
    and osi pages alike -- run from `man/` itself so a `.so man1/...`
    alias resolves the same way an installed MANPATH would."""
    mandoc = shutil.which("mandoc")
    groff = shutil.which("groff")
    if mandoc is not None:
        result = subprocess.run(
            [mandoc, "-Tlint", f"man1/{name}"],
            cwd=MAN1.parent,
            capture_output=True,
            text=True,
            check=False,
        )
        output = result.stdout + result.stderr
        if name == "cof-check.1":
            # `.so man1/cof-validate.1` (issue #764 rule 1) is the
            # conventional alias mechanism man(1) itself uses --
            # mandoc's own suggestion (`ln(1)` instead) would break
            # the one-source-of-truth guarantee the alias exists for.
            assert "so is fragile" in output
            assert result.returncode in (0, 2)
            return
        assert result.returncode == 0, output
        return
    if groff is not None:
        result = subprocess.run(
            [groff, "-man", "-Tutf8", "-ww", "-z", f"man1/{name}"],
            cwd=MAN1.parent,
            capture_output=True,
            text=True,
            check=False,
        )
        assert result.returncode == 0 and not result.stderr, result.stderr
        return
    pytest.skip("neither mandoc nor groff installed")


@pytest.mark.parametrize("name", all_page_names())
def test_page_has_no_literal_backtick(name: str) -> None:
    """A markdown code span renders as roff bold, never a literal
    backtick (issue #764)."""
    assert "`" not in render_page(name), f"cof-{name}(1) has a literal backtick"


def _click_root() -> click.Group:
    return typer.main.get_command(app)


def _is_option(param: click.Parameter) -> bool:
    # Typer's `TyperOption`/`TyperArgument` subclass a forked copy of
    # click's own `Parameter` (`typer._click`/`typer.core`), so
    # `isinstance(param, click.Option)` is always False against the real
    # click -- every param on a Typer-built command would read as neither.
    return getattr(param, "param_type_name", None) == "option"


def _flags(param: click.Parameter) -> list[str]:
    return list(param.opts) + list(getattr(param, "secondary_opts", None) or [])


def _all_commands_and_options() -> list[tuple[str, str]]:
    """(command_name, flag) for every option every top-level command (and
    `library refresh`) defines -- what test_every_option_appears_in_its_page
    checks against each page's own text. `check` is skipped: its page is
    the `.so man1/cof-validate.1` alias stub, not a rendering of `check`'s
    own (identical) options."""
    root = _click_root()
    pairs: list[tuple[str, str]] = []
    for name, cmd in root.commands.items():
        if name in ALIASES:
            continue
        if name == "library":
            pairs.extend(
                ("library", flag)
                for flag_cmd in cmd.commands.values()
                for param in flag_cmd.params
                if _is_option(param)
                for flag in _flags(param)
            )
            continue
        pairs.extend(
            (name, flag) for param in cmd.params if _is_option(param) for flag in _flags(param)
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


def test_command_order_covers_every_top_level_click_command() -> None:
    """A command Click actually defines but COMMAND_ORDER doesn't name
    would get no page at all, silently."""
    root = _click_root()
    assert set(root.commands) <= set(COMMAND_ORDER)


def test_man_defaults_to_the_cof_page() -> None:
    result = runner.invoke(app, ["man"])
    assert result.exit_code == 0
    assert ".TH COF 1" in result.stdout


def test_man_shows_a_command_page() -> None:
    result = runner.invoke(app, ["man", "run"])
    assert result.exit_code == 0
    assert ".TH COF-RUN 1" in result.stdout


def test_man_check_shows_validates_own_page() -> None:
    """An alias (`check`) shows its target's own content directly, both
    on a terminal and piped -- never the tiny `.so man1/cof-validate.1`
    stub, which only resolves relative to an installed MANPATH tree."""
    result = runner.invoke(app, ["man", "check"])
    assert result.exit_code == 0
    assert result.stdout == render_page("validate")
    assert ".so" not in result.stdout


def test_man_roff_flag_prints_roff() -> None:
    result = runner.invoke(app, ["man", "run", "--roff"])
    assert result.exit_code == 0
    assert result.stdout.startswith(".TH COF-RUN 1")


@pytest.mark.parametrize("name", all_page_names())
def test_page_cites_no_issue_numbers_or_design_docs(name: str) -> None:
    """Help text is documentation too (issue #764): no issue numbers
    or design-doc section references, in the generated page or the
    hand-written sections behind it."""
    page = render_page(name)
    assert not re.search(r"#\d{2,}", page), f"cof-{name}(1) cites an issue number"
    assert "DESIGN.md" not in page


def test_man_unknown_command_exits_2_and_lists_pages() -> None:
    result = runner.invoke(app, ["man", "bogus"])
    assert result.exit_code == 2
    assert "No such page" in result.stdout
    assert "run" in result.stdout
    assert "validate" in result.stdout


def test_pyproject_data_files_list_matches_all_page_names() -> None:
    """pyproject.toml's explicit share/man/man1 list (issue #764) names
    every page cof man/the generator know -- a page added to one
    without the other would otherwise ship silently incomplete."""
    text = (REPO_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    block = re.search(r'"share/man/man1"\s*=\s*\[(.*?)\]', text, re.DOTALL)
    assert block is not None, "pyproject.toml has no [tool.setuptools.data-files] share/man/man1 entry"
    listed = set(re.findall(r'"man/man1/([^"]+)"', block.group(1)))
    expected = {"cof.1" if n == "cof" else f"cof-{n}.1" for n in all_page_names()}
    assert listed == expected
