"""A SIGINT mid-stream leaves no curl process behind (#674).

`ollama` streams by default; a real curl, reading a real (stalled) HTTP
response, is still alive when the Ctrl-C/SIGINT lands. `run_tracked`'s own
process tracking (`core.cancellation`) has to kill that curl the same way
it kills a non-streaming adapter's curl call -- curl runs in its own
session (`start_new_session`), so a plain signal to the parent process
would never reach it on its own.
"""

from __future__ import annotations

import http.server
import os
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

from _signal_test_support import _diagnose_and_fail, _pid_alive, _sandboxed_env

_FIXTURE = Path(__file__).parent / "_fixtures" / "run_streaming_prompt.py"

#: How long the mock server's handler blocks mid-response after its first
#: chunk -- long enough that curl is still reading when SIGINT lands,
#: short enough to bound this test's own worst case if cancellation ever
#: regressed (never an unbounded hang).
_STALL_SECONDS = 15.0


class _StallingOllamaHandler(http.server.BaseHTTPRequestHandler):
    """Answers `POST /api/generate` with one NDJSON line, then blocks
    (never sending `done: true`) -- a streamed reply that never finishes
    on its own, standing in for a model still generating when the user
    hits Ctrl-C. `request_marker` is touched the instant the request
    body is fully read, before the handler ever blocks, so the test knows
    curl is already past connect-and-send and into "waiting for more".
    """

    request_marker: Path

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        chunk = b'{"response": "partial ", "done": false}\n'
        self.wfile.write(f"{len(chunk):x}\r\n".encode())
        self.wfile.write(chunk + b"\r\n")
        self.wfile.flush()
        self.request_marker.touch()
        time.sleep(_STALL_SECONDS)

    def log_message(self, *args: object) -> None:  # quiet test output
        pass


def _find_child_pid(parent_pid: int, *, comm: str, timeout: float = 20.0) -> int:
    """Poll `ps` for a direct child of *parent_pid* named *comm* -- curl's
    own pid, found by parentage rather than by anything on its argv (which
    never carries the URL at all, by design: `curl_support.py`'s whole
    point)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        out = subprocess.run(
            ["ps", "-eo", "pid,ppid,comm"], capture_output=True, text=True, check=True
        ).stdout
        for line in out.splitlines()[1:]:
            parts = line.split(None, 2)
            if len(parts) != 3:
                continue
            pid_s, ppid_s, found_comm = parts
            if ppid_s == str(parent_pid) and comm in found_comm:
                return int(pid_s)
        time.sleep(0.05)
    raise TimeoutError(f"no {comm!r} child of pid {parent_pid} appeared within {timeout}s")


def test_sigint_during_a_streamed_reply_kills_curl_and_leaves_none_behind(
    tmp_path: Path,
) -> None:
    marker = tmp_path / "request-received"
    handler_cls = type(
        "Handler", (_StallingOllamaHandler,), {"request_marker": marker}
    )
    # `ThreadingHTTPServer`, not plain `HTTPServer`: the handler blocks for
    # `_STALL_SECONDS` inside `do_POST`, and a single-threaded server's own
    # `shutdown()` can't return until that in-flight call does.
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler_cls)
    server.daemon_threads = True
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    try:
        port = server.server_address[1]
        orch = tmp_path / "stream.yml"
        orch.write_text(
            "effects:\n  - type: prompt\n    name: greeting\n    template: hi\n",
            encoding="utf-8",
        )

        proc = subprocess.Popen(
            [sys.executable, str(_FIXTURE), str(orch), f"http://127.0.0.1:{port}"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=_sandboxed_env(tmp_path),
            cwd=tmp_path,
        )
        try:
            curl_pid = _find_child_pid(proc.pid, comm="curl")
        except TimeoutError:
            _diagnose_and_fail(proc, timeout=20.0, label="curl never started", pid=proc.pid)
            raise
        deadline = time.monotonic() + 5.0
        while time.monotonic() < deadline and not marker.exists():
            time.sleep(0.05)
        assert marker.exists(), "server never recorded the request"

        os.kill(proc.pid, signal.SIGINT)
        try:
            proc.wait(timeout=20.0)
        except subprocess.TimeoutExpired:
            _diagnose_and_fail(
                proc, timeout=20.0, label="run did not exit after SIGINT", pid=proc.pid
            )

        assert proc.returncode == 130, (proc.returncode, proc.stderr.read() if proc.stderr else "")

        deadline = time.monotonic() + 5.0
        while time.monotonic() < deadline and _pid_alive(curl_pid):
            time.sleep(0.05)
        assert not _pid_alive(curl_pid), f"curl pid {curl_pid} survived the SIGINT"
    finally:
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=5)
