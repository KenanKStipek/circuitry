"""Unit tests for the shared curl failure-message helper (`_curl_errors.py`).

Adapter-level canaries (Azure, ollama, the OpenAI-compatible helper) cover
the same masking through their own call paths; these tests exercise the
helper directly for cases that don't need a whole fake adapter.
"""

from __future__ import annotations

from circuitry.adapters._curl_errors import curl_failure_message


def test_url_userinfo_is_stripped() -> None:
    """Regression for #246 follow-up: a configured base_url with an embedded
    username/password (e.g. a self-hosted proxy at
    `https://user:pw@host/v1`) must not land verbatim in the message even
    though callers pass it straight through as `url`."""
    message = curl_failure_message(
        adapter="ollama",
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
        adapter="ollama",
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
        adapter="openai",
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
        adapter="openai",
        model="m",
        url="https://example.test/v1/chat/completions",
        returncode=7,
        stdout="",
        stderr=f"curl: (7) Failed to connect, tried key {secret}",
        secrets=[secret],
    )
    assert secret not in message
