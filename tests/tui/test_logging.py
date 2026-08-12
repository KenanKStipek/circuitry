"""TUI-mode logging: quiet on the terminal, unchanged everywhere else."""

from __future__ import annotations

import io
import logging
import sys
from pathlib import Path
from typing import Any

import pytest

from circuitry.tui.log import is_console_handler, tui_logging


class _FakeConsole:
    """Stand-in for ``rich.console.Console`` as RichHandler exposes it."""

    def __init__(self, file: Any) -> None:
        self.file = file


class _RichLike(logging.Handler):
    def __init__(self, file: Any) -> None:
        super().__init__()
        self.console = _FakeConsole(file)


@pytest.fixture
def clean_root() -> Any:
    """Root logger with our own handlers only, restored afterwards.

    pytest keeps its own capture handler attached to the root logger, so
    assertions here check membership rather than the exact handler list.
    """
    root = logging.getLogger()
    saved = list(root.handlers)
    level = root.level
    root.handlers.clear()
    root.setLevel(logging.INFO)
    try:
        yield root
    finally:
        root.handlers.clear()
        root.handlers.extend(saved)
        root.setLevel(level)


def test_console_handler_detection() -> None:
    assert is_console_handler(logging.StreamHandler(sys.stdout))
    assert is_console_handler(logging.StreamHandler(sys.stderr))
    assert is_console_handler(_RichLike(sys.stdout))
    assert not is_console_handler(logging.StreamHandler(io.StringIO()))
    assert not is_console_handler(_RichLike(io.StringIO()))
    assert not is_console_handler(logging.NullHandler())


def test_file_handlers_are_never_treated_as_console(tmp_path: Path) -> None:
    handler = logging.FileHandler(tmp_path / "cof.log")
    try:
        assert not is_console_handler(handler)
    finally:
        handler.close()


def test_console_output_is_the_default_outside_the_context(
    clean_root: logging.Logger, capsys: pytest.CaptureFixture[str]
) -> None:
    """Control: this is exactly the write that would shred a Textual frame."""
    clean_root.addHandler(logging.StreamHandler(sys.stdout))
    logging.getLogger("circuitry.demo").info("noisy")
    assert "noisy" in capsys.readouterr().out


def test_console_handler_is_detached_and_restored(clean_root: logging.Logger) -> None:
    console = logging.StreamHandler(sys.stdout)
    clean_root.addHandler(console)

    with tui_logging(clean_root) as detached:
        assert detached == [console]
        assert console not in clean_root.handlers

    assert console in clean_root.handlers


def test_file_logging_keeps_working_while_the_app_owns_the_screen(
    clean_root: logging.Logger, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """The `--log-file` half of the contract: records still land on disk."""
    log_file = tmp_path / "cof.log"
    file_handler = logging.FileHandler(log_file)
    clean_root.addHandler(logging.StreamHandler(sys.stdout))
    clean_root.addHandler(file_handler)

    try:
        with tui_logging(clean_root):
            logging.getLogger("circuitry.demo").info("mid-app record")
    finally:
        file_handler.close()

    assert "mid-app record" in log_file.read_text(encoding="utf-8")
    assert capsys.readouterr().out == ""


def test_no_handlers_means_no_last_resort_output(
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Without a placeholder, logging's lastResort would write to stderr."""
    logger = logging.getLogger("circuitry.tests.placeholder")
    logger.propagate = False
    logger.handlers.clear()
    logger.addHandler(logging.StreamHandler(sys.stderr))

    with tui_logging(logger):
        assert [h for h in logger.handlers if isinstance(h, logging.NullHandler)]
        logger.warning("would hit lastResort")

    captured = capsys.readouterr()
    assert captured.out == ""
    assert captured.err == ""
    assert not [h for h in logger.handlers if isinstance(h, logging.NullHandler)]
    logger.handlers.clear()


def test_handlers_are_restored_when_the_body_raises(clean_root: logging.Logger) -> None:
    console = logging.StreamHandler(sys.stderr)
    clean_root.addHandler(console)

    with pytest.raises(RuntimeError), tui_logging(clean_root):
        raise RuntimeError("app crashed")

    assert console in clean_root.handlers


def test_non_console_handlers_are_left_alone(clean_root: logging.Logger) -> None:
    buffer = logging.StreamHandler(io.StringIO())
    clean_root.addHandler(buffer)

    with tui_logging(clean_root) as detached:
        assert detached == []
        assert buffer in clean_root.handlers

    assert buffer in clean_root.handlers


def test_default_targets_cover_root_and_circuitry(
    clean_root: logging.Logger, capsys: pytest.CaptureFixture[str]
) -> None:
    """Called with no arguments it manages the loggers the CLI configures."""
    package = logging.getLogger("circuitry")
    saved = list(package.handlers)
    propagate = package.propagate
    package.handlers.clear()
    package.propagate = False
    package.addHandler(logging.StreamHandler(sys.stdout))
    clean_root.addHandler(logging.StreamHandler(sys.stdout))

    try:
        with tui_logging():
            package.info("silenced")
            logging.getLogger().info("also silenced")
        assert capsys.readouterr().out == ""
    finally:
        package.handlers.clear()
        package.handlers.extend(saved)
        package.propagate = propagate


def test_run_tui_wraps_the_app_in_tui_logging(
    clean_root: logging.Logger, monkeypatch: pytest.MonkeyPatch
) -> None:
    pytest.importorskip("textual")
    from circuitry import tui
    from circuitry.tui.app import CircuitryApp

    console = logging.StreamHandler(sys.stdout)
    clean_root.addHandler(console)
    handlers_during_run: list[list[logging.Handler]] = []

    monkeypatch.setattr(
        CircuitryApp, "run", lambda self: handlers_during_run.append(list(clean_root.handlers))
    )
    tui.run_tui()

    assert handlers_during_run and console not in handlers_during_run[0]
    assert console in clean_root.handlers


@pytest.mark.parametrize("level", ["info", "warning", "error"])
def test_records_never_reach_the_frame(
    clean_root: logging.Logger, capsys: pytest.CaptureFixture[str], level: str
) -> None:
    pytest.importorskip("textual")
    import asyncio

    from circuitry.tui.app import CircuitryApp

    clean_root.addHandler(logging.StreamHandler(sys.stdout))
    clean_root.addHandler(logging.StreamHandler(sys.stderr))

    async def drive() -> tuple[str, str]:
        app = CircuitryApp()
        async with app.run_test(size=(80, 24)) as pilot:
            await pilot.pause()
            before = "\n".join(s.text for s in app.screen._compositor.render_strips())
            getattr(logging.getLogger("circuitry.demo"), level)("noise from a plugin")
            await pilot.pause()
            after = "\n".join(s.text for s in app.screen._compositor.render_strips())
        return before, after

    with tui_logging(clean_root):
        before, after = asyncio.run(drive())

    assert before == after
    captured = capsys.readouterr()
    assert captured.out == ""
    assert captured.err == ""
