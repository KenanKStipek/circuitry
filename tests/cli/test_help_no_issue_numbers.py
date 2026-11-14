"""No `--help` output, on either engine or `osi`, ever cites an issue
number (issue #776) -- `tests/cli/test_man_pages.py` already checks this
for cof's own generated man pages; this renders the actual `--help` text
every command prints, which `osi --help`/`osi watch --help` once did
(PR #769 removed cof's, electricity's and osi's, and this keeps them out).
"""

from __future__ import annotations

import os
import re
import subprocess
from pathlib import Path
from typing import Any

import conformance.harness as harness
import pytest
import typer.main
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()

OSCILLOSCOPE_DIR = Path(__file__).resolve().parents[2] / "oscilloscope"

#: Set by `generate-man-pages.py --check`'s own build when neither this nor
#: `oscilloscope/target/debug/osi` exists -- mirrors `harness.
#: ELECTRICITY_BINARY_ENV_VAR`, but with no artifact-upload job of its own
#: (unlike `electricity`), so this never builds, only locates: a job with
#: no Rust cache (every pytest-matrix job outside `man-pages`) would
#: otherwise pay a cold `cargo build` the first time this module imports
#: (issue #775's own removed cost).
OSI_BINARY_ENV_VAR = "OSI_BINARY"

#: `#123` and the bare "issue 123" form (case-insensitive) -- the two
#: shapes issue #776's own first comment found across `cof`, `electricity`
#: and `osi` before PR #769 removed them.
_ISSUE_NUMBER = re.compile(r"#\d+|issue\s+\d+", re.IGNORECASE)


def _assert_no_issue_numbers(label: str, text: str) -> None:
    match = _ISSUE_NUMBER.search(text)
    assert match is None, f"{label} --help cites an issue number: {match.group(0)!r}"


def _cof_command_paths() -> list[tuple[str, ...]]:
    """Every `cof` command and sub-command path, including a typer
    sub-app's own nested commands (`library refresh`) -- not just the
    root group's immediate children."""

    def walk(cmd: Any, prefix: tuple[str, ...]) -> list[tuple[str, ...]]:
        # Duck-typed, not `isinstance(cmd, click.Group)`: typer wraps a
        # forked copy of click's own types (`typer._click`), so a
        # `CircuitryGroup`/`TyperGroup` is never an instance of the real
        # `click.core.Group` (`man_pages.py`'s own `Command = Any` comment,
        # same reason) -- confirmed directly, that check silently walked
        # nothing at all.
        paths: list[tuple[str, ...]] = []
        children = getattr(cmd, "commands", None)
        if isinstance(children, dict):
            for name in sorted(children):
                child_prefix = (*prefix, name)
                paths.append(child_prefix)
                paths.extend(walk(children[name], child_prefix))
        return paths

    root: Any = typer.main.get_command(app)
    return walk(root, ())


@pytest.mark.parametrize("path", _cof_command_paths(), ids=" ".join)
def test_cof_command_help_cites_no_issue_number(path: tuple[str, ...]) -> None:
    result = runner.invoke(app, [*path, "--help"])
    assert result.exit_code == 0, result.output
    _assert_no_issue_numbers(f"cof {' '.join(path)}", result.output)


def test_cof_root_help_cites_no_issue_number() -> None:
    result = runner.invoke(app, ["--help"])
    assert result.exit_code == 0, result.output
    _assert_no_issue_numbers("cof", result.output)


@pytest.fixture(scope="module")
def electricity_binary() -> Path:
    binary, failure = harness.resolve_electricity_binary()
    if failure is not None:
        pytest.fail(failure)
    if binary is None:
        pytest.skip("cargo not available; electricity binary cannot be built")
    return binary


@pytest.mark.parametrize("args", [[], ["run"]])
def test_electricity_help_cites_no_issue_number(
    electricity_binary: Path, args: list[str], tmp_path: Path
) -> None:
    result = subprocess.run(
        [str(electricity_binary), *args, "--help"],
        capture_output=True, text=True, timeout=30, check=False,
        env={"HOME": str(tmp_path / "home")},
    )
    assert result.returncode == 0, (result.stdout, result.stderr)
    _assert_no_issue_numbers(f"electricity {' '.join(args)}", result.stdout + result.stderr)


@pytest.fixture(scope="module")
def osi_binary() -> Path:
    """Never builds `osi`: a pytest-matrix job with no Rust cache for
    `oscilloscope` (every job but `man-pages`) would pay a cold `cargo
    build` the first time this fixture ran. Uses `OSI_BINARY` when set,
    else the binary `scripts/generate-man-pages.py --check` already built
    in the same CI job (`man-pages`); skips, never builds, when neither
    exists -- the same degraded-outside-`man-pages` behaviour as before,
    without the cold-build cost."""
    prebuilt = os.environ.get(OSI_BINARY_ENV_VAR)
    if prebuilt:
        binary = Path(prebuilt)
        if binary.is_file() and os.access(binary, os.X_OK):
            return binary
        pytest.fail(f"{OSI_BINARY_ENV_VAR}={binary} does not point to an executable file")
    binary = OSCILLOSCOPE_DIR / "target" / "debug" / "osi"
    if binary.is_file() and os.access(binary, os.X_OK):
        return binary
    pytest.skip(f"no osi binary built; set {OSI_BINARY_ENV_VAR} or run in the man-pages CI job")


@pytest.mark.parametrize("args", [[], ["watch"]])
def test_osi_help_cites_no_issue_number(
    osi_binary: Path, args: list[str], tmp_path: Path
) -> None:
    result = subprocess.run(
        [str(osi_binary), *args, "--help"],
        capture_output=True, text=True, timeout=30, check=False,
        env={"HOME": str(tmp_path / "home"), "OSI_HISTORY_DIR": str(tmp_path / "history")},
    )
    assert result.returncode == 0, (result.stdout, result.stderr)
    _assert_no_issue_numbers(f"osi {' '.join(args)}", result.stdout + result.stderr)
