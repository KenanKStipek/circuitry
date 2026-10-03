"""``_show_loop_progress`` (#271): the CLI-only decision of whether the
interactive loop-progress line may show at all — verbose, a real TTY, and
neither ``--quiet`` nor ``--json``.
"""

from __future__ import annotations

import pytest

from circuitry.cli.app import _show_loop_progress


def _patch_isatty(monkeypatch: pytest.MonkeyPatch, value: bool) -> None:
    import sys

    monkeypatch.setattr(sys.stdout, "isatty", lambda: value)


def test_off_when_not_a_tty_even_with_verbose(monkeypatch: pytest.MonkeyPatch) -> None:
    _patch_isatty(monkeypatch, False)
    assert _show_loop_progress(verbose=True, quiet=False, json_out=False) is False


def test_on_when_verbose_tty_and_not_quiet_or_json(monkeypatch: pytest.MonkeyPatch) -> None:
    _patch_isatty(monkeypatch, True)
    assert _show_loop_progress(verbose=True, quiet=False, json_out=False) is True


def test_off_under_quiet_even_with_verbose_tty(monkeypatch: pytest.MonkeyPatch) -> None:
    _patch_isatty(monkeypatch, True)
    assert _show_loop_progress(verbose=True, quiet=True, json_out=False) is False


def test_off_under_json_even_with_verbose_tty(monkeypatch: pytest.MonkeyPatch) -> None:
    _patch_isatty(monkeypatch, True)
    assert _show_loop_progress(verbose=True, quiet=False, json_out=True) is False


def test_off_without_verbose_even_on_a_tty(monkeypatch: pytest.MonkeyPatch) -> None:
    _patch_isatty(monkeypatch, True)
    assert _show_loop_progress(verbose=False, quiet=False, json_out=False) is False
