"""ResilientConsole must survive a hung-up tty during a Live/Status region, not
just a plain ``print`` (#357 review finding 1), but must not swallow an
unrelated OSError from a redirected file or pipe (#357 review finding 4)."""

from __future__ import annotations

import errno
import io
import os

import pytest

from circuitry.output import ResilientConsole


class _HungUpTTY(io.TextIOBase):
    """A file-like object whose every write fails like a terminal that just
    hung up (EIO), not like a closed pipe (EPIPE)."""

    def __init__(self) -> None:
        self._devnull_fd = os.open(os.devnull, os.O_WRONLY)

    def write(self, text: str) -> int:
        raise OSError(errno.EIO, "Input/output error")

    def isatty(self) -> bool:
        return True

    def fileno(self) -> int:
        return self._devnull_fd

    def flush(self) -> None:
        pass

    def close(self) -> None:
        os.close(self._devnull_fd)
        super().close()


class _FullDisk(io.TextIOBase):
    """A file-like object whose every write fails like a full disk
    (ENOSPC) -- a real failure, not a gone reader."""

    def __init__(self) -> None:
        self._devnull_fd = os.open(os.devnull, os.O_WRONLY)

    def write(self, text: str) -> int:
        raise OSError(errno.ENOSPC, "No space left on device")

    def isatty(self) -> bool:
        return False

    def fileno(self) -> int:
        return self._devnull_fd

    def flush(self) -> None:
        pass

    def close(self) -> None:
        os.close(self._devnull_fd)
        super().close()


def test_status_survives_a_terminal_hangup() -> None:
    """A spinner's own flush on exit must not let a plain OSError (EIO)
    escape -- Rich's own `_check_buffer` only ever catches `BrokenPipeError`,
    which would otherwise skip `cof run`'s `--out`/`--last` writes and its
    exit-130/143/129 mapping (#357 review finding 1)."""
    tty = _HungUpTTY()
    console = ResilientConsole(file=tty, force_terminal=True)
    try:
        with console.status("[cyan]Running…[/cyan]"):
            pass
        console.print("after the hangup")
        console.line()
    finally:
        tty.close()
    assert console.quiet is True


def test_print_survives_a_terminal_hangup() -> None:
    tty = _HungUpTTY()
    console = ResilientConsole(file=tty, force_terminal=True)
    try:
        console.print("hello")
    finally:
        tty.close()
    assert console.quiet is True


def test_print_does_not_swallow_a_full_disk() -> None:
    """A write failing for a reason other than a gone terminal/reader (here,
    `ENOSPC`) must still raise -- only `EIO`/`EPIPE` are best-effort (#357
    review finding 4)."""
    disk = _FullDisk()
    console = ResilientConsole(file=disk, force_terminal=False)
    try:
        with pytest.raises(OSError) as exc_info:
            console.print("hello")
        assert exc_info.value.errno == errno.ENOSPC
    finally:
        disk.close()
    assert console.quiet is False
