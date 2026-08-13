"""Unit tests for the bare-``cof`` TTY gate."""

from __future__ import annotations

import sys

import pytest

from circuitry import tui


class _Stream:
    def __init__(self, tty: bool | Exception):
        self._tty = tty

    def isatty(self) -> bool:
        if isinstance(self._tty, Exception):
            raise self._tty
        return self._tty


def _set_streams(monkeypatch: pytest.MonkeyPatch, stdin, stdout) -> None:
    monkeypatch.setattr(sys, "stdin", stdin)
    monkeypatch.setattr(sys, "stdout", stdout)


@pytest.mark.parametrize(
    ("stdin_tty", "stdout_tty", "available", "expected"),
    [
        (True, True, True, True),
        (True, True, False, False),  # extra not installed
        (False, True, True, False),  # stdin redirected (`cof < file`)
        (True, False, True, False),  # stdout piped (`cof | cat`)
        (False, False, True, False),
    ],
)
def test_should_launch_tui_matrix(
    monkeypatch: pytest.MonkeyPatch,
    stdin_tty: bool,
    stdout_tty: bool,
    available: bool,
    expected: bool,
) -> None:
    _set_streams(monkeypatch, _Stream(stdin_tty), _Stream(stdout_tty))
    monkeypatch.setattr(tui, "textual_available", lambda: available)
    assert tui.should_launch_tui() is expected


def test_should_launch_tui_survives_broken_streams(monkeypatch: pytest.MonkeyPatch) -> None:
    """A closed/detached stream must not blow up the CLI entrypoint."""
    _set_streams(monkeypatch, _Stream(ValueError("closed")), _Stream(True))
    monkeypatch.setattr(tui, "textual_available", lambda: True)
    assert tui.should_launch_tui() is False


def test_should_launch_tui_handles_streams_without_isatty(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _set_streams(monkeypatch, object(), _Stream(True))
    monkeypatch.setattr(tui, "textual_available", lambda: True)
    assert tui.should_launch_tui() is False


def test_should_launch_tui_handles_none_streams(monkeypatch: pytest.MonkeyPatch) -> None:
    """Pythonw-style detached stdio leaves ``sys.stdin`` as ``None``."""
    _set_streams(monkeypatch, None, None)
    monkeypatch.setattr(tui, "textual_available", lambda: True)
    assert tui.should_launch_tui() is False


def test_textual_available_reports_missing_module(monkeypatch: pytest.MonkeyPatch) -> None:
    def _boom(name: str):
        raise ImportError(name)

    monkeypatch.setattr(tui.importlib.util, "find_spec", _boom)
    assert tui.textual_available() is False


def test_textual_available_true_when_installed() -> None:
    pytest.importorskip("textual")
    assert tui.textual_available() is True


def test_install_hint_names_the_extra() -> None:
    assert "circuitry-cof[tui]" in tui.INSTALL_HINT
    assert tui.INSTALL_HINT in tui.MISSING_EXTRA_MESSAGE
