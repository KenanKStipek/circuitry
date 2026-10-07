"""A closed terminal's SIGHUP through a real pty — possibly delivered
*twice* (#357 follow-up).

`tests/cli/test_run_sighup.py` sends SIGHUP straight to a `cof run`
subprocess, which is enough to cover a bare `kill -HUP` or a process
manager. What actually happens when the owner closes a terminal window
is different: an interactive shell (zsh, which is how the owner runs
`cof`) owns the pty, runs `cof` as its foreground job, and on hangup can
deliver SIGHUP to that job's whole process group *twice*, about a
millisecond apart — plain macOS bash only ever sends it once. A second
SIGHUP landing inside the same signal-handling window as the first must
not hit the "second signal" `os._exit` rule a genuine second SIGINT/
SIGTERM takes: that would abort the first SIGHUP's own cleanup
(`finally:`, `--out`, the `--last` stash) before it finishes.

This drives a real pseudo-terminal exactly the way closing a terminal
window does — an interactive shell as the pty's session leader, `cof`
started as its foreground job by typing the command, then the pty's
master fd closed out from under it — rather than importing a fixture
script that lives outside this repo.
"""

from __future__ import annotations

import json
import os
import select
import shlex
import shutil
import signal
import subprocess
import sys
import textwrap
import time
from pathlib import Path

import pytest

try:
    import pty
except ImportError:  # pragma: no cover - exercised only on non-POSIX platforms
    pty = None  # type: ignore[assignment]

_SHELL = shutil.which("zsh") or shutil.which("bash")

requires_pty_shell = pytest.mark.skipif(
    pty is None or os.name != "posix" or _SHELL is None,
    reason="requires a POSIX pty and an installed zsh or bash",
)
requires_sighup = pytest.mark.skipif(
    not hasattr(signal, "SIGHUP"), reason="SIGHUP does not exist on this platform"
)

#: Long enough that a run only finishes early because it was actually
#: cancelled, never because the branch happened to complete on its own.
_BRANCH_SLEEP_SECONDS = 60

#: Looser than the bare-subprocess tests in `test_run_sighup.py`: a real
#: pty plus an interactive shell's own startup adds overhead a direct
#: `Popen` never pays. What matters is "nowhere near `_BRANCH_SLEEP_SECONDS`".
_STOP_BOUND_SECONDS = 20.0

_CREDENTIAL_ENV_VARS = (
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "CYBERDINER_TOKEN",
    "CYBERDINER_EXPO_URL",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_AUTH_TOKEN",
    "NPM_TOKEN",
    "NPM_TOKEN_GITHUB",
    "NODE_AUTH_TOKEN",
)


def _sandboxed_env(home: Path) -> dict[str, str]:
    """A child-process env with a fake $HOME (CLAUDE.md hermeticity) and no
    credentials — this suite has no adapter to use them, real or fake."""
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in _CREDENTIAL_ENV_VARS and not k.startswith("CIRCUITRY_")
    }
    env.update(HOME=str(home), TERM="xterm-256color", COLUMNS="100", LINES="30")
    return env


def _wait_for_paths(paths: list[Path], *, timeout: float = 30.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if all(p.exists() for p in paths):
            return
        time.sleep(0.02)
    missing = [str(p) for p in paths if not p.exists()]
    raise TimeoutError(f"never started within {timeout}s: {missing}")


def _pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


@requires_pty_shell
@requires_sighup
def test_closed_terminal_sighup_does_not_abort_cleanup(tmp_path: Path) -> None:
    """Close a real pty running `cof` as an interactive shell's foreground
    job (zsh if installed, else bash) and expect exactly what a single,
    direct SIGHUP gives in `test_run_sighup.py`: the branch's process
    gone, `finally:` run, `--out`/the `--last` stash written, exit 129,
    and `--resume` picking the unfinished branch back up — never the
    immediate, no-cleanup exit a real second SIGINT/SIGTERM takes.
    """
    home = tmp_path / "home"
    home.mkdir()
    pidfile = tmp_path / "branch_pid"
    started = tmp_path / "branch_started"
    rc_file = tmp_path / "rc"
    out_path = tmp_path / "out.json"

    # Idempotent: a `--resume` rerun of this same unfinished branch exits
    # at once instead of sleeping the full duration all over again.
    script = (
        f"if [ -f {started} ]; then exit 0; fi; "
        f"echo $$ > {pidfile}; touch {started}; sleep {_BRANCH_SLEEP_SECONDS}"
    )
    orch = tmp_path / "pty_sighup.yml"
    orch.write_text(
        textwrap.dedent(f"""\
            effects:
              - type: tool
                name: step_a
                provider: uuid
              - type: dynamic
                name: d1
                flow: tree
                effects:
                  - type: tool
                    name: b0
                    provider: shell
                    params:
                      command: bash
                      args: ["-c", "{script}"]
                      allowed_commands: ["bash"]
                finally:
                  - type: tool
                    name: cleanup
                    provider: uuid
              - type: tool
                name: step_c
                provider: uuid
            """),
        encoding="utf-8",
    )

    # `cof` installs its own SIGHUP handler only once it is well inside
    # `run()` — this wrapper's own interpreter shares the foreground
    # job's process group with it for the brief window before that, and
    # a bare, unhandled SIGHUP would kill *this* process (SIG_DFL) before
    # `subprocess.call` returns and this ever gets to write `rc_file`.
    wrapper = tmp_path / "wrap.py"
    wrapper.write_text(
        textwrap.dedent(f"""\
            import signal, subprocess, sys
            signal.signal(signal.SIGHUP, lambda *a: None)
            rc = subprocess.call(sys.argv[1:])
            open({str(rc_file)!r}, "w").write(str(rc))
            """),
        encoding="utf-8",
    )

    env = _sandboxed_env(home)
    cmd = [
        sys.executable,
        str(wrapper),
        sys.executable,
        "-m",
        "circuitry.cli.app",
        "run",
        str(orch),
        "--out",
        str(out_path),
        "--quiet",
    ]

    assert _SHELL is not None
    shell_argv = (
        [_SHELL, "-f", "-i"]
        if _SHELL.endswith("zsh")
        else [_SHELL, "--norc", "--noprofile", "-i"]
    )

    pid, fd = pty.fork()  # type: ignore[union-attr]
    if pid == 0:
        os.chdir(tmp_path)
        os.execve(_SHELL, shell_argv, env)  # type: ignore[arg-type]

    try:
        os.write(fd, (" ".join(shlex.quote(c) for c in cmd) + "\n").encode())

        deadline = time.monotonic() + 30.0
        while time.monotonic() < deadline and not (pidfile.exists() and started.exists()):
            r, _, _ = select.select([fd], [], [], 0.05)
            if r:
                try:
                    os.read(fd, 65536)
                except OSError:
                    break
        if not (pidfile.exists() and started.exists()):
            pytest.fail("branch never started under the pty shell")
        # Let the typed command actually land as the shell's foreground
        # job (shell startup banner, the echoed command line) before
        # hanging up on it.
        time.sleep(0.3)

        t0 = time.monotonic()
        os.close(fd)  # the terminal goes away

        deadline = time.monotonic() + _STOP_BOUND_SECONDS
        while time.monotonic() < deadline and not rc_file.exists():
            time.sleep(0.05)
        elapsed = time.monotonic() - t0
        if not rc_file.exists():
            pytest.fail(f"cof never exited {_STOP_BOUND_SECONDS}s after the terminal closed")
        rc = int(rc_file.read_text(encoding="utf-8").strip())
    finally:
        try:
            os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            pass
        if pidfile.exists():
            leftover_pid = int(pidfile.read_text(encoding="utf-8").strip())
            if _pid_alive(leftover_pid):
                try:
                    os.kill(leftover_pid, signal.SIGKILL)
                except OSError:
                    pass

    assert rc == 129, f"cof exited {rc}, not 129"
    assert elapsed < _STOP_BOUND_SECONDS

    pid_val = int(pidfile.read_text(encoding="utf-8").strip())
    assert not _pid_alive(pid_val), f"pid {pid_val} survived the closed terminal"

    assert out_path.exists()
    state = json.loads(out_path.read_text(encoding="utf-8"))
    d1 = state["prime"]["d1"]
    assert d1["meta"]["completed_at"]
    assert d1["meta"]["error"] == "Interrupted (SIGHUP)"
    assert d1["cleanup"]["meta"]["completed_at"]
    assert state["runtime"]["last_run"]["completed_at"]
    assert "step_c" not in state["prime"]
    assert (home / ".config" / "circuitry" / "last-run.json").exists()

    resumed_out = tmp_path / "resumed.json"
    resume = subprocess.run(
        [
            sys.executable, "-m", "circuitry.cli.app", "run", str(orch),
            "--state", str(out_path), "--resume", "x",
            "--out", str(resumed_out), "--quiet",
        ],
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
        env=_sandboxed_env(home),
        cwd=tmp_path,
    )
    assert resume.returncode == 0, (resume.stdout, resume.stderr)
    resumed_state = json.loads(resumed_out.read_text(encoding="utf-8"))
    assert resumed_state["prime"]["step_c"]["meta"]["completed_at"]
