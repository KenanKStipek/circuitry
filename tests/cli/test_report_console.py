"""``cof run``'s own console line for a ``report`` effect: one ``▸ <message>``
line, printed on a real TTY unless ``--quiet``/``--json``/``--tail`` (#605)."""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest
import yaml

pytest.importorskip("typer")
from typer.testing import BytesIOCopy, CliRunner

from circuitry.cli.app import app

runner = CliRunner()


@pytest.fixture(autouse=True)
def _force_tty(monkeypatch: pytest.MonkeyPatch) -> None:
    """The report console line only prints on a real TTY stdout (the same
    auto-quiet rule cof already applies to other interactive output).
    Typer's ``CliRunner`` replaces ``sys.stdout`` with its own
    ``BytesIOCopy``-backed stream for the run (patching ``sys.stdout.isatty``
    directly would patch the wrong, pre-swap object), so this patches that
    stream class's own ``isatty`` instead -- every test here opts in by
    default."""
    monkeypatch.setattr(BytesIOCopy, "isatty", lambda self: True)


def _write_orch(tmp_path: Path) -> Path:
    orch: dict[str, Any] = {
        "effects": [
            {"type": "report", "message": "working on {{input.topic}}"},
            {
                "type": "report",
                "name": "summary",
                "level": "warning",
                "message": "found an issue",
            },
        ],
    }
    path = tmp_path / "orch.yml"
    path.write_text(yaml.dump(orch, sort_keys=False), encoding="utf-8")
    return path


def _invoke(tmp_path: Path, orch: Path, *extra: str) -> Any:
    out = tmp_path / "state.json"
    return runner.invoke(
        app,
        [
            "run",
            str(orch),
            "--dry-run",
            "--out",
            str(out),
            "-e",
            "topic=circuitry",
            *extra,
        ],
    )


def test_prints_one_line_per_report(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path)
    result = _invoke(tmp_path, orch)

    assert result.exit_code == 0, result.stdout
    # Not a plain `splitlines()` prefix check: a live status spinner is
    # active for the run (the TTY this fixture simulates), so the first
    # line's own text shares a terminal line with that spinner's
    # cursor-hide escape code.
    assert "▸ working on circuitry" in result.stdout
    assert "▸ found an issue" in result.stdout
    assert result.stdout.index("▸ working on circuitry") < result.stdout.index(
        "▸ found an issue"
    )


def test_a_warning_level_report_is_styled_like_cofs_own_warnings() -> None:
    """Unit-level, against a fresh ``Console`` (not the CLI's own
    module-level singleton, whose color system is detected once at
    import time and so can't be forced TTY-on for a test): a
    ``level: warning`` line must actually carry a color SGR code, not
    render as plain text the way an unregistered style name would."""
    import io

    from rich.console import Console

    from circuitry.cli.app import _make_report_console_observer

    buf = io.StringIO()
    rich_console = Console(file=buf, force_terminal=True, color_system="standard")
    observer = _make_report_console_observer(rich_console.print)
    observer("prime", "info", "working on circuitry", None)
    observer("prime", "warning", "found an issue", None)
    info_line, warning_line = buf.getvalue().splitlines()

    assert "\x1b[" not in info_line
    assert "▸ working on circuitry" in info_line
    assert "\x1b[33m" in warning_line
    assert "▸ found an issue" in warning_line


def test_a_message_containing_rich_markup_is_escaped_not_interpreted(tmp_path: Path) -> None:
    orch: dict[str, Any] = {
        "effects": [{"type": "report", "message": "saved [/tmp/out] and [bold]done[/bold]"}],
    }
    path = tmp_path / "orch.yml"
    path.write_text(yaml.dump(orch, sort_keys=False), encoding="utf-8")
    result = _invoke(tmp_path, path)

    assert result.exit_code == 0, result.stdout
    assert "▸ saved [/tmp/out] and [bold]done[/bold]" in result.stdout


def test_suppressed_under_quiet(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path)
    result = _invoke(tmp_path, orch, "--quiet")

    assert result.exit_code == 0, result.stdout
    assert "▸" not in result.stdout


def test_suppressed_under_tail(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path)
    result = _invoke(tmp_path, orch, "--tail")

    assert result.exit_code == 0, result.stdout
    assert "▸" not in result.stdout


def test_suppressed_on_a_non_tty_stdout(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(BytesIOCopy, "isatty", lambda self: False)
    orch = _write_orch(tmp_path)
    result = _invoke(tmp_path, orch)

    assert result.exit_code == 0, result.stdout
    assert "▸" not in result.stdout


def test_suppressed_under_json(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path)
    result = _invoke(tmp_path, orch, "--json")

    assert result.exit_code == 0, result.stdout
    assert "▸" not in result.stdout
