"""Unit tests for the shared curl helper (`circuitry/curl_support.py`).

Adapter-level canaries (Azure, ollama, the OpenAI-compatible helper) cover
the same masking through their own call paths; these tests exercise the
helper directly for cases that don't need a whole fake adapter.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path
from typing import Any

import pytest
from curl_test_support import RecordingJSONHandler, local_server

from circuitry.curl_support import curl_failure_message, run_curl


def test_url_userinfo_is_stripped() -> None:
    """Regression for #246 follow-up: a configured base_url with an embedded
    username/password (e.g. a self-hosted proxy at
    `https://user:pw@host/v1`) must not land verbatim in the message even
    though callers pass it straight through as `url`."""
    message = curl_failure_message(
        source="ollama",
        model="m",
        url="https://user:canarypw@example.test/v1/chat",
        returncode=22,
        stdout="",
        stderr="curl: (22) The requested URL returned error: 401",
    )
    assert "canarypw" not in message
    assert "user" not in message
    assert "example.test/v1/chat" in message


def test_url_userinfo_in_hint_is_also_stripped() -> None:
    """The ollama exit-7 hint interpolates `self.base_url` directly, not the
    `url` argument — the stripping must apply to the whole rendered message,
    not just the `url=` field."""
    message = curl_failure_message(
        source="ollama",
        model=None,
        url="http://localhost:11434/api/generate",
        returncode=7,
        stdout="",
        stderr="curl: (7) Failed to connect",
        hint="Ollama at http://user:canarypw@example.test:11434 is not reachable.",
    )
    assert "canarypw" not in message


def test_secret_split_by_truncation_is_still_masked() -> None:
    """A non-JSON body is truncated to 200 chars; if a secret straddled the
    cut, masking after truncation would leave a partial secret behind. The
    helper must mask before it truncates."""
    secret = "sk-canary-secret-0123456789"
    padding = "x" * 190
    body = padding + secret
    message = curl_failure_message(
        source="openai",
        model="m",
        url="https://example.test/v1/chat/completions",
        returncode=22,
        stdout=body,
        stderr="",
        secrets=[secret],
    )
    assert secret not in message
    assert "canary-secret" not in message


def test_secret_in_stderr_is_masked() -> None:
    secret = "sk-stderr-canary-999"
    message = curl_failure_message(
        source="openai",
        model="m",
        url="https://example.test/v1/chat/completions",
        returncode=7,
        stdout="",
        stderr=f"curl: (7) Failed to connect, tried key {secret}",
        secrets=[secret],
    )
    assert secret not in message


def test_credential_like_query_param_is_masked() -> None:
    """Regression for #280: a search API's key commonly travels as a query
    parameter (`web_search`'s `extra_params`), so a failed request must not
    store it verbatim in the run record via the error message's `url=`."""
    secret = "canary-query-key-123"
    message = curl_failure_message(
        source="web_search",
        url=f"https://api.example.test/search?q=yaml&key={secret}",
        returncode=22,
        stdout="",
        stderr="HTTP 401",
    )
    assert secret not in message
    assert "q=yaml" in message


def test_non_credential_query_params_are_not_masked() -> None:
    message = curl_failure_message(
        source="web_search",
        url="https://api.example.test/search?q=yaml&limit=5",
        returncode=22,
        stdout="",
        stderr="HTTP 401",
    )
    assert "q=yaml" in message
    assert "limit=5" in message


# ---------------------------------------------------------------------------
# run_curl — end to end against a local HTTP server, never a live provider.
# ---------------------------------------------------------------------------


class _RecordingHandler(RecordingJSONHandler):
    response_body = b'{"ok": true}'


@pytest.mark.skipif(shutil.which("curl") is None, reason="curl not on PATH")
def test_run_curl_delivers_headers_and_body_without_putting_them_on_argv(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    secret = "canary-bearer-token-xyz"
    body = '{"prompt": "canary prompt text"}'

    calls: list[list[str]] = []
    real_run = subprocess.run

    def spying_run(cmd: list[str], **kwargs: Any) -> Any:
        calls.append(cmd)
        return real_run(cmd, **kwargs)

    monkeypatch.setattr("subprocess.run", spying_run)

    with local_server(_RecordingHandler) as base_url:
        proc = run_curl(
            url=base_url + "/x",
            headers={"Authorization": f"Bearer {secret}"},
            data=body,
            timeout_seconds=5,
        )

    assert proc.returncode == 0
    assert proc.stdout == '{"ok": true}'
    assert _RecordingHandler.captured_headers["Authorization"] == f"Bearer {secret}"
    assert _RecordingHandler.captured_body.decode() == body

    assert len(calls) == 1
    cmd_str = " ".join(calls[0])
    assert secret not in cmd_str
    assert "canary prompt text" not in cmd_str
    assert calls[0][0] == "curl"
    assert calls[0][1] == "-q"


@pytest.mark.skipif(shutil.which("curl") is None, reason="curl not on PATH")
def test_run_curl_large_body_round_trips(monkeypatch: pytest.MonkeyPatch) -> None:
    """A body over 200 KiB (as a base64 image payload would be) must still
    arrive intact: argv was never involved, so there's no 128 KiB-per-arg
    ceiling to hit."""
    large_value = "a" * (250 * 1024)
    body = '{"image": "' + large_value + '"}'

    with local_server(_RecordingHandler) as base_url:
        proc = run_curl(url=base_url + "/x", data=body, timeout_seconds=5)

    assert proc.returncode == 0
    assert len(_RecordingHandler.captured_body) == len(body.encode())


@pytest.mark.skipif(shutil.which("curl") is None, reason="curl not on PATH")
def test_run_curl_ignores_curlrc(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """`-q` first means a stray `~/.curlrc` can't inject its own header."""
    curlrc = tmp_path / ".curlrc"
    curlrc.write_text('header = "X-Injected: leaked"\n')
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.delenv("CURL_HOME", raising=False)

    with local_server(_RecordingHandler) as base_url:
        run_curl(url=base_url + "/x", timeout_seconds=5)

    assert "X-Injected" not in _RecordingHandler.captured_headers


def test_run_curl_closes_its_pipe_fd(monkeypatch: pytest.MonkeyPatch) -> None:
    """The header pipe's read end must not leak across calls."""
    open_fds_before = len(os.listdir("/dev/fd"))

    def fake_run(cmd: list[str], **kwargs: Any) -> Any:
        class _Proc:
            returncode = 0
            stdout = ""
            stderr = ""

        return _Proc()

    monkeypatch.setattr("subprocess.run", fake_run)
    for _ in range(20):
        run_curl(
            url="http://example.invalid/x",
            headers={"Authorization": "Bearer x"},
            timeout_seconds=5,
        )
    open_fds_after = len(os.listdir("/dev/fd"))
    assert open_fds_after <= open_fds_before + 1
