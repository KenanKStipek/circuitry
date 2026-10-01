"""Tests for the ComfyUI tool plugin's curl plumbing.

Covers: #280 (no argv echo in error messages), #300 (workflow body on
stdin, not `-d`), and the general curl-hygiene (`-q` first, no header on
argv) that this lane applies to every curl invocation under
`src/circuitry/plugins/`.
"""

from __future__ import annotations

import http.server
import json
import shutil
import subprocess
import threading
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import Any

import pytest
from curl_test_support import assert_not_in_argv, assert_q_first

from circuitry.plugins.comfyui import ComfyUIPlugin


@pytest.fixture(autouse=True)
def _curl_present(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: "/usr/bin/curl" if n == "curl" else None)


def test_curl_json_post_sends_body_on_stdin_not_argv(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    canary = "canary prompt text " * 20

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> Any:
        captured["cmd"] = cmd
        captured["input"] = kwargs.get("input")
        return subprocess.CompletedProcess(cmd, 0, stdout=json.dumps({"ok": True}), stderr="")

    monkeypatch.setattr(subprocess, "run", fake_run)
    plugin = ComfyUIPlugin(base_url="http://localhost:8188")
    result = plugin._curl_json(
        url="http://localhost:8188/prompt",
        method="POST",
        payload={"prompt": canary},
        timeout_seconds=10,
    )
    assert result == {"ok": True}
    assert_q_first(captured["cmd"])
    assert_not_in_argv(captured["cmd"], canary)
    assert canary in (captured["input"] or "")


def test_curl_json_large_body_over_200kib_works(monkeypatch: pytest.MonkeyPatch) -> None:
    large_payload = {"image_b64": "a" * (250 * 1024)}

    def fake_run(cmd: list[str], **kwargs: Any) -> Any:
        body = kwargs.get("input") or ""
        assert len(body) > 200 * 1024
        return subprocess.CompletedProcess(cmd, 0, stdout=json.dumps({"ok": True}), stderr="")

    monkeypatch.setattr(subprocess, "run", fake_run)
    plugin = ComfyUIPlugin(base_url="http://localhost:8188")
    result = plugin._curl_json(
        url="http://localhost:8188/prompt",
        method="POST",
        payload=large_payload,
        timeout_seconds=10,
    )
    assert result == {"ok": True}


def test_curl_json_failure_does_not_echo_cmd(monkeypatch: pytest.MonkeyPatch) -> None:
    def fake_run(cmd: list[str], **kwargs: Any) -> Any:
        return subprocess.CompletedProcess(cmd, 22, stdout="", stderr="HTTP 500")

    monkeypatch.setattr(subprocess, "run", fake_run)
    plugin = ComfyUIPlugin(base_url="http://localhost:8188")
    with pytest.raises(RuntimeError) as exc:
        plugin._curl_json(
            url="http://localhost:8188/prompt",
            method="POST",
            payload={"prompt": "x"},
            timeout_seconds=10,
        )
    assert "cmd=" not in str(exc.value)


def test_curl_bytes_failure_does_not_echo_cmd(monkeypatch: pytest.MonkeyPatch) -> None:
    def fake_run(cmd: list[str], **kwargs: Any) -> Any:
        return subprocess.CompletedProcess(cmd, 22, stdout=b"", stderr=b"HTTP 404")

    monkeypatch.setattr(subprocess, "run", fake_run)
    plugin = ComfyUIPlugin(base_url="http://localhost:8188")
    with pytest.raises(RuntimeError) as exc:
        plugin._curl_bytes(url="http://localhost:8188/view?x=1", timeout_seconds=10)
    assert "cmd=" not in str(exc.value)


def test_curl_bytes_uses_q_first(monkeypatch: pytest.MonkeyPatch) -> None:
    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> Any:
        captured["cmd"] = cmd
        return subprocess.CompletedProcess(cmd, 0, stdout=b"bytes", stderr=b"")

    monkeypatch.setattr(subprocess, "run", fake_run)
    plugin = ComfyUIPlugin(base_url="http://localhost:8188")
    plugin._curl_bytes(url="http://localhost:8188/view?x=1", timeout_seconds=10)
    assert_q_first(captured["cmd"])


def test_upload_image_failure_does_not_echo_cmd(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    image = tmp_path / "ref.png"
    image.write_bytes(b"\x89PNG\r\n\x1a\n")

    def fake_run(cmd: list[str], **kwargs: Any) -> Any:
        return subprocess.CompletedProcess(cmd, 22, stdout="", stderr="HTTP 500")

    monkeypatch.setattr(subprocess, "run", fake_run)
    plugin = ComfyUIPlugin(base_url="http://localhost:8188")
    with pytest.raises(RuntimeError) as exc:
        plugin._upload_image(image_path=str(image), timeout_seconds=10)
    assert "cmd=" not in str(exc.value)


def test_upload_image_uses_q_first(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    image = tmp_path / "ref.png"
    image.write_bytes(b"\x89PNG\r\n\x1a\n")
    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> Any:
        captured["cmd"] = cmd
        return subprocess.CompletedProcess(
            cmd, 0, stdout=json.dumps({"name": "ref.png"}), stderr=""
        )

    monkeypatch.setattr(subprocess, "run", fake_run)
    plugin = ComfyUIPlugin(base_url="http://localhost:8188")
    plugin._upload_image(image_path=str(image), timeout_seconds=10)
    assert_q_first(captured["cmd"])


# ---------------------------------------------------------------------------
# End to end: a local HTTP server standing in for ComfyUI itself.
# ---------------------------------------------------------------------------

_IMAGE_BYTES = b"FAKEPNGDATA" * 10
_PROMPT_ID = "pid-1"


class _FakeComfyUIHandler(http.server.BaseHTTPRequestHandler):
    captured_prompt_body: bytes = b""

    def _json(self, payload: dict[str, Any]) -> None:
        body = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", 0))
        type(self).captured_prompt_body = self.rfile.read(length)
        self._json({"prompt_id": _PROMPT_ID})

    def do_GET(self) -> None:
        if self.path.startswith(f"/history/{_PROMPT_ID}"):
            self._json(
                {
                    _PROMPT_ID: {
                        "outputs": {
                            "9": {
                                "images": [
                                    {
                                        "filename": "out.png",
                                        "subfolder": "",
                                        "type": "output",
                                    }
                                ]
                            }
                        }
                    }
                }
            )
        elif self.path.startswith("/view"):
            self.send_response(200)
            self.send_header("Content-Type", "image/png")
            self.end_headers()
            self.wfile.write(_IMAGE_BYTES)
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, *args: Any) -> None:  # quiet test output
        pass


@contextmanager
def _fake_comfyui_server() -> Iterator[str]:
    server = http.server.HTTPServer(("127.0.0.1", 0), _FakeComfyUIHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_address[1]}"
    finally:
        server.shutdown()
        thread.join(timeout=5)


@pytest.mark.skipif(shutil.which("curl") is None, reason="curl not on PATH")
def test_execute_end_to_end_against_local_server_with_large_prompt(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Real curl, real local server, no mocking of subprocess.run: proves
    the whole queue -> poll -> view round trip works, that a prompt over
    200 KiB reaches the server intact, and that it never touches argv."""
    large_prompt = "canary prompt " * 20000  # well over 200 KiB
    assert len(large_prompt.encode()) > 200 * 1024

    calls: list[list[str]] = []
    real_run = subprocess.run

    def spying_run(cmd: list[str], **kwargs: Any) -> Any:
        calls.append(cmd)
        return real_run(cmd, **kwargs)

    monkeypatch.setattr(subprocess, "run", spying_run)
    monkeypatch.chdir(tmp_path)

    with _fake_comfyui_server() as base_url:
        plugin = ComfyUIPlugin(
            base_url=base_url,
            default_model="test-ckpt",
            image_dir="./images",
            poll_interval=0.01,
        )
        result = plugin.execute(
            params={"prompt": large_prompt, "model": "test-ckpt"},
            timeout_seconds=10,
        )

    saved_path = Path(str(result.value))
    assert saved_path.read_bytes() == _IMAGE_BYTES
    assert large_prompt.encode() in _FakeComfyUIHandler.captured_prompt_body

    for cmd in calls:
        assert_q_first(cmd)
        assert_not_in_argv(cmd, large_prompt[:200])
