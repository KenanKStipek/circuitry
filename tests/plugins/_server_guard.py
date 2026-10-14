"""Keeps a throwaway test server from outliving the process that started it
(#412): a background watchdog polls whether the process that spawned the
server is still its parent, and kills its whole process group the moment
that stops being true (the server was reparented because its parent died)
or a maximum lifetime elapses, whichever comes first.

Run directly, this module *is* the wrapper: it arms the watchdog around
itself, then ``exec``s the real command in its own place, so any pid or
process group a caller captured before starting it keeps naming the real
server. Imported, :func:`start_ppid_watchdog` lets a test-authored server
script (one that serves in its own process rather than ``exec``ing a
command) arm the same protection around itself.
"""

from __future__ import annotations

import os
import signal
import subprocess
import sys
import time

_POLL_SECONDS = 0.5
_DEFAULT_MAX_LIFETIME_SECONDS = 300.0


def _current_parent_of(pid: int) -> int | None:
    """*pid*'s current parent pid, or ``None`` once *pid* itself is gone."""
    out = subprocess.run(
        ["ps", "-o", "ppid=", "-p", str(pid)], capture_output=True, text=True, check=False
    )
    text = out.stdout.strip()
    return int(text) if text.isdigit() else None


def _watch(*, server_pid: int, expected_parent: int, max_lifetime: float) -> None:
    deadline = time.monotonic() + max_lifetime
    while True:
        time.sleep(_POLL_SECONDS)
        if os.getppid() != server_pid:
            return  # the server (our own forking parent) already exited
        parent = _current_parent_of(server_pid)
        if parent is None:
            return  # the server already exited on its own
        if parent != expected_parent or time.monotonic() >= deadline:
            try:
                os.killpg(os.getpgid(server_pid), signal.SIGKILL)
            except OSError:
                pass
            return


def start_ppid_watchdog(max_lifetime: float = _DEFAULT_MAX_LIFETIME_SECONDS) -> None:
    """Fork a background watchdog that kills this process's whole process
    group once this process is reparented (its original parent died) or
    *max_lifetime* seconds pass, whichever comes first."""
    expected_parent = os.getppid()
    server_pid = os.getpid()
    if os.fork() == 0:
        try:
            _watch(server_pid=server_pid, expected_parent=expected_parent, max_lifetime=max_lifetime)
        finally:
            os._exit(0)


def main(argv: list[str]) -> int:
    max_lifetime = _DEFAULT_MAX_LIFETIME_SECONDS
    i = 0
    while i < len(argv):
        if argv[i] == "--max-lifetime":
            max_lifetime = float(argv[i + 1])
            i += 2
        elif argv[i] == "--marker":
            i += 2  # unused here: kept so `ps`/`lsof` can find the watchdog forked
            # below by it, since exec replaces this process's own argv with the
            # real command's but the watchdog, sharing its process group, keeps it
        elif argv[i] == "--":
            i += 1
            break
        else:
            break
    command = argv[i:]
    if not command:
        print("_server_guard: no command given after '--'", file=sys.stderr)
        return 2
    start_ppid_watchdog(max_lifetime)
    os.execvp(command[0], command)
    return 1  # unreachable: execvp only returns by raising


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
