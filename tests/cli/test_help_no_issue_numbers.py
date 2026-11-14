"""No `--help` output, on either engine or `osi`, ever cites an issue
number (issue #776) -- `tests/cli/test_man_pages.py` already checks this
for cof's own generated man pages; this renders the actual `--help` text
every command prints, which `osi --help`/`osi watch --help` once did
(PR #764 removed cof's, electricity's and osi's, and this keeps them out).
"""

from __future__ import annotations

import re
import subprocess
from pathlib import Path

import conformance.harness as harness
import pytest
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()

OSCILLOSCOPE_DIR = Path(__file__).resolve().parents[2] / "oscilloscope"

#: `#123` and the bare "issue 123" form (case-insensitive) -- the two
#: shapes issue #776's own first comment found across `cof`, `electricity`
#: and `osi` before PR #764 removed them.
_ISSUE_NUMBER = re.compile(r"#\d+|issue\s+\d+", re.IGNORECASE)


def _assert_no_issue_numbers(label: str, text: str) -> None:
    match = _ISSUE_NUMBER.search(text)
    assert match is None, f"{label} --help cites an issue number: {match.group(0)!r}"


def _cof_command_names() -> list[str]:
    import typer.main

    root = typer.main.get_command(app)
    return sorted(root.commands)


@pytest.mark.parametrize("name", _cof_command_names())
def test_cof_command_help_cites_no_issue_number(name: str) -> None:
    result = runner.invoke(app, [name, "--help"])
    assert result.exit_code == 0, result.output
    _assert_no_issue_numbers(f"cof {name}", result.output)


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
    electricity_binary: Path, args: list[str]
) -> None:
    result = subprocess.run(
        [str(electricity_binary), *args, "--help"],
        capture_output=True, text=True, timeout=30, check=False,
    )
    assert result.returncode == 0, (result.stdout, result.stderr)
    _assert_no_issue_numbers(f"electricity {' '.join(args)}", result.stdout + result.stderr)


@pytest.fixture(scope="module")
def osi_binary() -> Path:
    """`osi` has no shared, pre-built binary of its own (unlike
    `electricity`'s `ELECTRICITY_BINARY`, which `quality.yml`'s
    `build-electricity` job uploads for the whole pytest matrix) -- built
    fresh here, and skipped rather than failed when `cargo` isn't on
    `PATH`, so this degrades the same way outside the one CI job
    (`man-pages`) that sets up a Rust toolchain alongside Python."""
    import shutil

    if shutil.which("cargo") is None:
        pytest.skip("cargo not available; osi binary cannot be built")
    proc = subprocess.run(
        ["cargo", "build", "-p", "oscilloscope", "--bin", "osi"],
        cwd=OSCILLOSCOPE_DIR, env=harness.cargo_env(),
        capture_output=True, text=True, timeout=600, check=False,
    )
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p oscilloscope --bin osi failed:\n{proc.stdout}\n{proc.stderr}")
    binary = OSCILLOSCOPE_DIR / "target" / "debug" / "osi"
    assert binary.exists(), f"cargo build succeeded but {binary} is missing"
    return binary


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
