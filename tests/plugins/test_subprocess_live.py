"""``meta.live`` for a running subprocess tool (#741): a real ``shell``
step, run directly through ``ToolRuntime`` (no CLI subprocess needed), with
a small fake command that prints on both streams over a few hundred
milliseconds, gated by files instead of sleeps so the assertions never
race the timing.
"""

from __future__ import annotations

import os
import signal
import stat
import sys
import threading
import time
from pathlib import Path
from typing import Any

import pytest

from circuitry.cli.interrupts import SigTermInterrupt, sigterm_as_interrupt
from circuitry.core.store import Store
from circuitry.core.tool import ToolDefinition, ToolRuntime

#: A fake command: for each ``line`` in argv, waits for a file named
#: ``<gate_dir>/<line>`` to exist, then prints ``line`` to stdout (or
#: stderr, for an argument prefixed ``err:``) and flushes immediately.
#: ``FAKE_GATE_DIR`` names *gate_dir*. Bounds its own lifetime (#661): it
#: exits once reparented (its starting pytest process died without
#: killing it) or at a hard 120s deadline, never an unbounded wait.
_FAKE_SCRIPT = """\
#!{python}
import os, sys, time

gate_dir = os.environ["FAKE_GATE_DIR"]
starting_parent = os.getppid()
deadline = time.monotonic() + 120
for arg in sys.argv[1:]:
    stream = sys.stderr if arg.startswith("err:") else sys.stdout
    text = arg[4:] if arg.startswith("err:") else arg
    gate = os.path.join(gate_dir, arg.replace(":", "_"))
    while not os.path.exists(gate):
        if os.getppid() != starting_parent or time.monotonic() > deadline:
            sys.exit(1)
        time.sleep(0.01)
    stream.write(text + "\\n")
    stream.flush()
"""


def _write_fake(bin_dir: Path, name: str = "faketool") -> Path:
    bin_dir.mkdir(parents=True, exist_ok=True)
    path = bin_dir / name
    path.write_text(_FAKE_SCRIPT.format(python=sys.executable), encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return path


def _gate(gate_dir: Path, arg: str) -> None:
    gate_dir.mkdir(parents=True, exist_ok=True)
    (gate_dir / arg.replace(":", "_")).touch()


def _run_shell(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    args: list[str],
    *,
    name: str = "x",
) -> tuple[ToolDefinition, Store, list[dict[str, Any] | None], Path]:
    """Builds a ``shell`` ``ToolDefinition`` running ``faketool`` with
    *args*, and a ``Store`` that records every ``meta.live`` write (the
    value, or ``None`` for an ``unset``) in arrival order. Returns
    ``(defn, store, live_calls, gate_dir)`` -- the caller runs
    ``ToolRuntime(defn).execute(store=store, ctx={})`` itself, gating
    *faketool*'s lines with :func:`_gate` as it goes.
    """
    bin_dir = tmp_path / "bin"
    _write_fake(bin_dir)
    gate_dir = tmp_path / "gates"
    monkeypatch.setenv("PATH", f"{bin_dir}{os.pathsep}{os.environ['PATH']}")
    monkeypatch.setenv("FAKE_GATE_DIR", str(gate_dir))
    defn = ToolDefinition(
        name=name,
        provider="shell",
        params={"command": "faketool", "allowed_commands": ["faketool"], "args": args},
    )
    # A live tracker is only ever built when something is watching
    # (`store.on_write is not None`) -- a no-op observer here stands in
    # for `--live-state`/an embedding caller's own state observer (#741).
    store = Store({}, on_write=lambda _state: None)
    live_calls: list[dict[str, Any] | None] = []
    original_set = store.set
    original_unset = store.unset

    def _rec_set(path: str, value: Any) -> None:
        if path.endswith(".meta.live"):
            live_calls.append(value)
        original_set(path, value)

    def _rec_unset(path: str) -> None:
        if path.endswith(".meta.live"):
            live_calls.append(None)
        original_unset(path)

    store.set = _rec_set  # type: ignore[method-assign]
    store.unset = _rec_unset  # type: ignore[method-assign]
    return defn, store, live_calls, gate_dir


def test_live_samples_show_the_tail_growing_and_keep_streams_apart(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    args = ["line1", "err:oops1", "line2", "err:oops2"]
    defn, store, live_calls, gate_dir = _run_shell(tmp_path, monkeypatch, args)

    def _release() -> None:
        for arg in args:
            time.sleep(0.05)
            _gate(gate_dir, arg)

    releaser = threading.Thread(target=_release, daemon=True)
    releaser.start()
    ToolRuntime(defn).execute(store=store, ctx={})
    releaser.join(5)

    snapshots = [c for c in live_calls if c is not None]
    assert len(snapshots) >= 2, "too few live samples to show growth"
    stdout_tails = [s["stdout_tail"] for s in snapshots]
    stderr_tails = [s["stderr_tail"] for s in snapshots]
    assert stdout_tails[-1] == "line1\nline2"
    assert stderr_tails[-1] == "oops1\noops2"
    # Monotonically growing, never shrinking mid-run.
    assert all(
        len(stdout_tails[i]) <= len(stdout_tails[i + 1]) for i in range(len(stdout_tails) - 1)
    )
    final_snapshot = snapshots[-1]
    assert final_snapshot["stdout_bytes"] == len("line1\nline2\n")
    assert final_snapshot["stderr_bytes"] == len("oops1\noops2\n")
    assert live_calls[-1] is None, "meta.live was never cleared"
    assert "live" not in store.state["x"]["meta"]
    assert store.state["x"]["meta"]["stdout"] == "line1\nline2\n"
    assert store.state["x"]["meta"]["stderr"] == "oops1\noops2\n"


def test_live_tail_stays_bounded_with_large_output(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    lines = [f"line{i}" for i in range(40)]
    defn, store, live_calls, gate_dir = _run_shell(tmp_path, monkeypatch, lines)
    for arg in lines:
        _gate(gate_dir, arg)
    ToolRuntime(defn).execute(store=store, ctx={})

    snapshots = [c for c in live_calls if c is not None]
    assert snapshots, "no live samples were published"
    last_tail = snapshots[-1]["stdout_tail"]
    assert last_tail.splitlines() == [f"line{i}" for i in range(20, 40)]
    for snapshot in snapshots:
        assert len(snapshot["stdout_tail"].encode("utf-8")) <= 4096
    assert snapshots[-1]["stdout_bytes"] == sum(len(f"{line}\n") for line in lines)


def test_live_tail_stays_bounded_with_a_line_longer_than_4kib(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A single line far longer than the 4 KiB cap is clipped to its own
    last 4096 bytes, at a character boundary, the moment it lands in the
    tail -- never held in full, and never dropped as a fragment once it
    is the only line left to keep (#741).
    """
    huge = "y" * 10000
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    script = bin_dir / "hugeline"
    script.write_text(
        f"""#!{sys.executable}
import sys
sys.stdout.write({huge!r} + "\\n")
sys.stdout.flush()
""",
        encoding="utf-8",
    )
    script.chmod(script.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    monkeypatch.setenv("PATH", f"{bin_dir}{os.pathsep}{os.environ['PATH']}")

    defn = ToolDefinition(
        name="x", provider="shell", params={"command": "hugeline", "allowed_commands": ["hugeline"]}
    )
    store = Store({}, on_write=lambda _state: None)
    live_calls: list[dict[str, Any] | None] = []
    original_set = store.set

    def _rec(path: str, value: Any) -> None:
        if path.endswith(".meta.live") and isinstance(value, dict):
            live_calls.append(value)
        original_set(path, value)

    store.set = _rec  # type: ignore[method-assign]

    ToolRuntime(defn).execute(store=store, ctx={})

    snapshots = [c for c in live_calls if c is not None]
    assert snapshots, "no live samples were published"
    for snapshot in snapshots:
        assert len(snapshot["stdout_tail"].encode("utf-8")) <= 4096
    assert snapshots[-1]["stdout_tail"] == huge[-4096:]
    assert store.state["x"]["meta"]["stdout"] == huge + "\n"


def test_the_current_partial_line_shows_live_before_any_newline(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A line with no newline yet (nothing printed to end it, and the
    process hasn't exited either) still shows live, as the tail's own
    last line -- the issue's own "printf 'working…'; sleep 60" case
    (#741). Gated by files, like the rest of this module's own fake, so
    the assertion can't race the write.
    """
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    gate_dir = tmp_path / "gates"
    script = bin_dir / "partialtool"
    script.write_text(
        f"""#!{sys.executable}
import os, sys, time
sys.stdout.write("working...")
sys.stdout.flush()
gate = os.path.join({str(gate_dir)!r}, "go")
while not os.path.exists(gate):
    time.sleep(0.01)
""",
        encoding="utf-8",
    )
    script.chmod(script.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    monkeypatch.setenv("PATH", f"{bin_dir}{os.pathsep}{os.environ['PATH']}")

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={"command": "partialtool", "allowed_commands": ["partialtool"]},
    )
    store = Store({}, on_write=lambda _state: None)
    live_calls: list[dict[str, Any] | None] = []
    original_set = store.set

    def _rec(path: str, value: Any) -> None:
        if path.endswith(".meta.live") and isinstance(value, dict):
            live_calls.append(value)
        original_set(path, value)

    store.set = _rec  # type: ignore[method-assign]

    def _release() -> None:
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if any(
                isinstance(c, dict) and c.get("stdout_tail") == "working..." for c in live_calls
            ):
                break
            time.sleep(0.01)
        gate_dir.mkdir(parents=True, exist_ok=True)
        (gate_dir / "go").touch()

    releaser = threading.Thread(target=_release, daemon=True)
    releaser.start()
    ToolRuntime(defn).execute(store=store, ctx={})
    releaser.join(10)

    assert any(
        isinstance(c, dict) and c.get("stdout_tail") == "working..." for c in live_calls
    ), "the partial line never showed live before the process exited"


def test_binary_output_is_decoded_with_replacement(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Invalid bytes mid-stream still fail the step at the end, a strict
    decode exactly like a run with no live wiring at all (#741's own
    ``run_tracked`` change never touches that) -- but the live tail
    decodes the very same raw bytes separately and lossily
    (``errors="replace"``), so it shows the invalid bytes too, as
    U+FFFD, rather than stopping at the last valid line (#741).
    (``_TailBuffer`` itself, tested directly below, covers the
    replacement decoding for invalid bytes.)
    """
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    script = bin_dir / "binarytool"
    script.write_text(
        f"""#!{sys.executable}
import sys, time
sys.stdout.write("before\\n")
sys.stdout.flush()
time.sleep(0.3)
sys.stdout.buffer.write(b"\\xff\\xfeafter\\n")
sys.stdout.buffer.flush()
""",
        encoding="utf-8",
    )
    script.chmod(script.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    monkeypatch.setenv("PATH", f"{bin_dir}{os.pathsep}{os.environ['PATH']}")

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={
            "command": "binarytool",
            "allowed_commands": ["binarytool"],
            "allow_nonzero": True,
        },
    )
    store = Store({}, on_write=lambda _state: None)
    live_calls: list[dict[str, Any] | None] = []
    original_set = store.set

    def _rec(path: str, value: Any) -> None:
        if path.endswith(".meta.live") and isinstance(value, dict):
            live_calls.append(value)
        original_set(path, value)

    store.set = _rec  # type: ignore[method-assign]

    with pytest.raises(UnicodeDecodeError):
        ToolRuntime(defn, verbose=False).execute(store=store, ctx={})

    assert live_calls, "no live sample was published before the decode error"
    assert live_calls[-1]["stdout_tail"] == "before\n��after"


def test_tail_buffer_decodes_binary_output_with_replacement() -> None:
    """The tail itself (``_TailBuffer``, fed raw bytes directly, the same
    shape a reader thread feeds it) decodes invalid UTF-8 with
    replacement rather than raising -- independent of, and never as
    strict as, the final ``meta.stdout``/``meta.stderr`` decode (#741).
    """
    from circuitry.subprocess_live import _TailBuffer

    buf = _TailBuffer()
    buf.append(b"before\xff\xfeafter\n")
    assert buf.text() == "before��after"


def test_the_final_state_equals_a_run_without_live_wiring(tmp_path: Path) -> None:
    """``on_live`` only ever adds a ``meta.live`` key while the attempt
    runs; the finished node is byte-identical to one from a plugin call
    with no ``on_live`` at all (#741)."""
    from circuitry.plugins._subprocess import resolve_binary, run_binary

    binary = resolve_binary(["echo"])
    assert binary is not None
    without_live = run_binary(binary=binary, args=["hello", "world"])

    calls: list[dict[str, Any] | None] = []
    with_live = run_binary(binary=binary, args=["hello", "world"], on_live=calls.append)
    assert calls and calls[-1] is None
    assert with_live == without_live


def test_the_final_state_equals_a_run_without_live_wiring_for_stderr_crlf_and_stdin(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The same byte-identical guarantee as the test above, but for the
    paths that test's own ``echo`` call never exercises (#741): stderr,
    CRLF translation, and input delivered over ``stdin`` -- each compared
    with and without ``on_live`` wired.
    """
    from circuitry.plugins._subprocess import resolve_binary, run_binary

    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    script = bin_dir / "stderrandcrlf"
    script.write_text(
        f"""#!{sys.executable}
import sys
sys.stdout.write("line one\\r\\nline two\\r\\n")
sys.stderr.write("oops\\r\\n")
sys.stdout.flush()
sys.stderr.flush()
""",
        encoding="utf-8",
    )
    script.chmod(script.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    without_live = run_binary(binary=str(script), args=[])
    with_live = run_binary(binary=str(script), args=[], on_live=lambda _s: None)
    assert with_live == without_live
    assert with_live.stdout == "line one\nline two\n"
    assert with_live.stderr == "oops\n"

    cat_binary = resolve_binary(["cat"])
    assert cat_binary is not None
    without_live_stdin = run_binary(binary=cat_binary, args=[], stdin="from stdin\n")
    with_live_stdin = run_binary(
        binary=cat_binary, args=[], stdin="from stdin\n", on_live=lambda _s: None
    )
    assert with_live_stdin == without_live_stdin
    assert with_live_stdin.stdout == "from stdin\n"


@pytest.fixture(autouse=True)
def _restore_process_signal_state() -> Any:
    sigs = [signal.SIGINT, signal.SIGTERM]
    previous = {sig: signal.getsignal(sig) for sig in sigs}
    yield
    for sig, handler in previous.items():
        signal.signal(sig, handler)
    from circuitry.core.cancellation import get_token

    get_token().reset()


@pytest.mark.skipif(os.name != "posix", reason="process groups and real signals are POSIX-only")
def test_cancelling_a_running_subprocess_clears_meta_live(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # The gate for "line1" is never created, so faketool blocks forever on
    # it -- still running, and so still cancellable -- until the signal
    # below kills it.
    args = ["line1"]
    defn, store, live_calls, _gate_dir = _run_shell(tmp_path, monkeypatch, args, name="x")

    outcome: list[BaseException] = []

    def _sender() -> None:
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if isinstance(store.state.get("x", {}).get("meta", {}).get("live"), dict):
                break
            time.sleep(0.01)
        os.kill(os.getpid(), signal.SIGTERM)

    with sigterm_as_interrupt():
        sender = threading.Thread(target=_sender)
        sender.start()
        try:
            ToolRuntime(defn).execute(store=store, ctx={})
        except BaseException as exc:
            outcome.append(exc)
        sender.join(10)

    assert len(outcome) == 1 and isinstance(outcome[0], SigTermInterrupt)
    assert "live" not in store.state["x"]["meta"]
    assert live_calls[-1] is None, "meta.live was never cleared by the cancellation"
