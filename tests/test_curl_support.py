"""Unit tests for the shared curl helper (`circuitry/curl_support.py`).

Adapter-level canaries (Azure, ollama, the OpenAI-compatible helper) cover
the same masking through their own call paths; these tests exercise the
helper directly for cases that don't need a whole fake adapter.
"""

from __future__ import annotations

import glob
import http.server
import os
import shutil
import subprocess
import tempfile
from pathlib import Path
from typing import Any

import pytest
from curl_test_support import RecordingJSONHandler, assert_not_in_argv, local_server

from circuitry.curl_support import (
    _curl_supports_retry_after_header,
    curl_failure_message,
    extract_retry_after,
    run_curl,
)


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


def test_run_curl_rejects_header_value_with_embedded_newline() -> None:
    """A `\n` in a header value would otherwise end that config-file line
    early, letting the rest be read as a new curl option."""
    with pytest.raises(ValueError):
        run_curl(
            url="https://example.test/x",
            headers={"X-Thing": "a\nurl = http://attacker.test/"},
            timeout_seconds=5,
        )


def test_run_curl_rejects_header_name_with_embedded_cr() -> None:
    with pytest.raises(ValueError):
        run_curl(
            url="https://example.test/x",
            headers={"X-Thing\r": "value"},
            timeout_seconds=5,
        )


def test_run_curl_rejects_url_with_embedded_newline() -> None:
    """Regression for #314: the URL now travels through the same
    config-file channel as headers, so it must be rejected the same way a
    header value is — a `\n` would otherwise end the `url = "..."` line
    early, letting the rest be read as a new curl option."""
    with pytest.raises(ValueError):
        run_curl(
            url='https://example.test/x\nheader = "X-Injected: leaked"',
            timeout_seconds=5,
        )


def test_run_curl_rejects_url_with_embedded_nul() -> None:
    with pytest.raises(ValueError):
        run_curl(url="https://example.test/x\0", timeout_seconds=5)


def test_run_curl_rejects_an_oversized_config_instead_of_blocking_forever() -> None:
    """The config (URL + headers) is written to an unread pipe before curl
    starts on POSIX; a write past the kernel's pipe buffer would block
    forever with no timeout in effect yet, so this is rejected up front
    instead."""
    huge_header_value = "x" * 100_000
    with pytest.raises(ValueError, match="over the"):
        run_curl(
            url="https://example.test/x",
            headers={"X-Big": huge_header_value},
            timeout_seconds=5,
        )


# ---------------------------------------------------------------------------
# run_curl — end to end against a local HTTP server, never a live provider.
# ---------------------------------------------------------------------------


class _RecordingHandler(RecordingJSONHandler):
    response_body = b'{"ok": true}'


@pytest.mark.skipif(shutil.which("curl") is None, reason="curl not on PATH")
def test_run_curl_on_windows_sends_the_config_via_a_temp_file_and_removes_it(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Regression for #315: Windows has no inheritable pipe fd, so the
    config travels through a file in the per-user temp directory instead,
    removed once curl is done with it — including on success."""
    # `pathlib.Path` reads `os.name` in its own constructor to pick
    # `WindowsPath`/`PosixPath` and would raise trying to build the former
    # on a real POSIX machine; `glob.glob` has no such dependency, so the
    # before/after check below uses it instead, even inside the patched
    # window.
    pattern = os.path.join(tempfile.gettempdir(), "circuitry-curl-*")
    before = set(glob.glob(pattern))

    monkeypatch.setattr(os, "name", "nt")
    real_popen = subprocess.Popen
    seen_config_paths: list[str] = []

    def spying_popen(cmd: list[str], **kwargs: Any) -> Any:
        config_path = cmd[cmd.index("--config") + 1]
        seen_config_paths.append(config_path)
        # The windows branch must actually have run: the config is a real
        # file under the temp dir, not `/dev/fd/<n>` (the POSIX path).
        assert os.path.isfile(config_path)
        assert os.path.dirname(config_path) == tempfile.gettempdir()
        return real_popen(cmd, **kwargs)

    # run_curl is tracked (`core.cancellation.run_tracked`, #356) via
    # `subprocess.Popen` + `communicate`, not `subprocess.run` — patching
    # the latter here would no longer intercept anything.
    monkeypatch.setattr("subprocess.Popen", spying_popen)
    with local_server(_RecordingHandler) as base_url:
        proc = run_curl(
            url=base_url + "/x",
            headers={"Authorization": "Bearer winsecret"},
            timeout_seconds=5,
        )

    assert proc.returncode == 0
    assert _RecordingHandler.captured_headers["Authorization"] == "Bearer winsecret"
    assert seen_config_paths and "/dev/fd/" not in seen_config_paths[0]
    after = set(glob.glob(pattern))
    assert after == before


def test_run_curl_on_windows_removes_the_temp_file_even_when_curl_is_missing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The temp file is removed in a `finally`, so a curl invocation that
    never even started (curl missing) still doesn't leak it."""
    pattern = os.path.join(tempfile.gettempdir(), "circuitry-curl-*")
    before = set(glob.glob(pattern))

    monkeypatch.setattr(os, "name", "nt")
    seen_config_paths: list[str] = []

    def fake_popen(cmd: list[str], **kwargs: Any) -> Any:
        config_path = cmd[cmd.index("--config") + 1]
        seen_config_paths.append(config_path)
        # Prove the Windows branch (and not the POSIX one) actually ran
        # before raising: the config file must exist right now.
        assert os.path.isfile(config_path)
        raise FileNotFoundError("curl")

    monkeypatch.setattr("subprocess.Popen", fake_popen)
    with pytest.raises(FileNotFoundError):
        run_curl(url="https://example.test/x", timeout_seconds=5)
    assert seen_config_paths and "/dev/fd/" not in seen_config_paths[0]
    after = set(glob.glob(pattern))
    assert after == before


@pytest.mark.skipif(shutil.which("curl") is None, reason="curl not on PATH")
def test_run_curl_delivers_headers_and_body_without_putting_them_on_argv(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    secret = "canary-bearer-token-xyz"
    body = '{"prompt": "canary prompt text"}'

    calls: list[list[str]] = []
    real_popen = subprocess.Popen

    def spying_popen(cmd: list[str], **kwargs: Any) -> Any:
        calls.append(cmd)
        return real_popen(cmd, **kwargs)

    # run_curl is tracked (`core.cancellation.run_tracked`, #356) via
    # `subprocess.Popen` + `communicate`, not `subprocess.run`.
    monkeypatch.setattr("subprocess.Popen", spying_popen)

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
    monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
    _RecordingHandler.captured_headers = {}

    with local_server(_RecordingHandler) as base_url:
        proc = run_curl(url=base_url + "/x", timeout_seconds=5)

    assert proc.returncode == 0
    assert "X-Injected" not in _RecordingHandler.captured_headers


def test_run_curl_closes_its_pipe_fd(monkeypatch: pytest.MonkeyPatch) -> None:
    """The header pipe's read end must not leak across calls."""
    open_fds_before = len(os.listdir("/dev/fd"))

    def fake_popen(cmd: list[str], **kwargs: Any) -> Any:
        class _Proc:
            returncode = 0

            def communicate(self, input: Any = None, timeout: Any = None) -> Any:
                return "", ""

            def __enter__(self) -> _Proc:
                return self

            def __exit__(self, *exc: Any) -> bool:
                return False

        return _Proc()

    monkeypatch.setattr("subprocess.Popen", fake_popen)
    for _ in range(20):
        run_curl(
            url="http://example.invalid/x",
            headers={"Authorization": "Bearer x"},
            timeout_seconds=5,
        )
    open_fds_after = len(os.listdir("/dev/fd"))
    assert open_fds_after <= open_fds_before + 1


# ---------------------------------------------------------------------------
# #314 — the URL itself must never touch argv either.
# ---------------------------------------------------------------------------


@pytest.mark.skipif(shutil.which("curl") is None, reason="curl not on PATH")
def test_run_curl_query_string_credential_never_touches_argv(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Regression for #314: a query-string credential (e.g. a search API
    key in `extra_params`) must not land on argv now that the whole URL
    travels through `--config` instead of being curl's final argument."""
    secret = "canary-query-key-789"
    calls: list[list[str]] = []
    real_popen = subprocess.Popen

    def spying_popen(cmd: list[str], **kwargs: Any) -> Any:
        calls.append(cmd)
        return real_popen(cmd, **kwargs)

    monkeypatch.setattr("subprocess.Popen", spying_popen)

    with local_server(_RecordingHandler) as base_url:
        proc = run_curl(url=f"{base_url}/x?key={secret}", timeout_seconds=5)

    assert proc.returncode == 0
    assert len(calls) == 1
    assert_not_in_argv(calls[0], secret)
    assert "--config" in calls[0]


@pytest.mark.skipif(shutil.which("curl") is None, reason="curl not on PATH")
def test_run_curl_userinfo_url_never_touches_argv(monkeypatch: pytest.MonkeyPatch) -> None:
    """Regression for #314: `user:pass@host` (e.g. a `base_url` set that
    way) must not land on argv either."""
    calls: list[list[str]] = []
    real_popen = subprocess.Popen

    def spying_popen(cmd: list[str], **kwargs: Any) -> Any:
        calls.append(cmd)
        return real_popen(cmd, **kwargs)

    monkeypatch.setattr("subprocess.Popen", spying_popen)

    with local_server(_RecordingHandler) as base_url:
        host_port = base_url.removeprefix("http://")
        url = f"http://canaryuser:canarypw@{host_port}/x"
        proc = run_curl(url=url, timeout_seconds=5)

    assert proc.returncode == 0
    assert len(calls) == 1
    assert_not_in_argv(calls[0], "canaryuser", "canarypw")


# ---------------------------------------------------------------------------
# #319 — Retry-After capture.
# ---------------------------------------------------------------------------


class _RetryAfterHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        self.send_response(429)
        self.send_header("Retry-After", "2")
        self.send_header("Content-Length", "2")
        self.end_headers()
        self.wfile.write(b"{}")

    def log_message(self, *args: Any) -> None:  # quiet test output
        pass


@pytest.mark.skipif(
    shutil.which("curl") is None or not _curl_supports_retry_after_header(),
    reason="curl not on PATH or too old for %header{retry-after}",
)
def test_run_curl_captures_retry_after_header() -> None:
    """Regression for #319: a 429's `Retry-After` must reach the caller on
    stderr (never argv, never a temp file), and the exit-code status line
    `_retry.py` parses must stay intact alongside it."""
    with local_server(_RetryAfterHandler) as base_url:
        proc = run_curl(url=base_url + "/x", timeout_seconds=5)

    assert proc.returncode == 22
    assert "curl: (22) The requested URL returned error: 429" in proc.stderr
    assert extract_retry_after(proc.stderr) == "2"


def test_curl_failure_message_never_includes_the_retry_after_marker() -> None:
    """The `circuitry-retry-after:` line `run_curl` writes to stderr on
    every call must never leak into a rendered failure message."""
    stderr = (
        "curl: (22) The requested URL returned error: 429\n"
        "circuitry-retry-after:2\n"
    )
    message = curl_failure_message(
        source="openai",
        url="https://example.test/v1/chat",
        returncode=22,
        stdout="",
        stderr=stderr,
    )
    assert "circuitry-retry-after" not in message


class _FakePopen:
    """Stands in for the real `subprocess.Popen` the curl-version probe
    calls directly (`circuitry.curl_support._real_popen_cls`, #356) —
    `communicate()` either returns the canned stdout or raises *raises*."""

    def __init__(self, returncode: int, stdout: str, raises: Exception | None = None) -> None:
        self.returncode = returncode
        self._stdout = stdout
        self._raises = raises

    def communicate(self, input: Any = None, timeout: Any = None) -> tuple[str, str]:
        if self._raises is not None:
            raise self._raises
        return (self._stdout, "")

    def __enter__(self) -> _FakePopen:
        return self

    def __exit__(self, *exc: Any) -> bool:
        return False


def test_curl_supports_retry_after_header_true_on_modern_curl(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _curl_supports_retry_after_header.cache_clear()

    def fake_popen(cmd: list[str], **kwargs: Any) -> Any:
        return _FakePopen(0, "curl 7.84.0 (x86_64)\n")

    monkeypatch.setattr("circuitry.curl_support._real_popen_cls", fake_popen)
    try:
        assert _curl_supports_retry_after_header() is True
    finally:
        _curl_supports_retry_after_header.cache_clear()


def test_curl_supports_retry_after_header_false_on_older_curl(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _curl_supports_retry_after_header.cache_clear()

    def fake_popen(cmd: list[str], **kwargs: Any) -> Any:
        return _FakePopen(0, "curl 7.83.1 (x86_64)\n")

    monkeypatch.setattr("circuitry.curl_support._real_popen_cls", fake_popen)
    try:
        assert _curl_supports_retry_after_header() is False
    finally:
        _curl_supports_retry_after_header.cache_clear()


def test_curl_supports_retry_after_header_false_when_probe_times_out(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A hung `curl --version` (SubprocessError, e.g. TimeoutExpired) must
    not escape the probe and break `run_curl` for every caller."""
    _curl_supports_retry_after_header.cache_clear()

    def fake_popen(cmd: list[str], **kwargs: Any) -> Any:
        return _FakePopen(0, "", raises=subprocess.TimeoutExpired(cmd, 5))

    monkeypatch.setattr("circuitry.curl_support._real_popen_cls", fake_popen)
    try:
        assert _curl_supports_retry_after_header() is False
    finally:
        _curl_supports_retry_after_header.cache_clear()


@pytest.mark.skipif(shutil.which("curl") is None, reason="curl not on PATH")
def test_run_curl_retry_after_absent_on_success() -> None:
    """No `Retry-After` header on a 200 — `extract_retry_after` reads an
    empty capture as `None`, not an empty string."""
    with local_server(_RecordingHandler) as base_url:
        proc = run_curl(url=base_url + "/x", timeout_seconds=5)

    assert proc.returncode == 0
    assert extract_retry_after(proc.stderr) is None
