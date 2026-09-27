"""Tests for the gemini adapter (Google AI Studio via OpenAI-compat) and
the shared `_OpenAICompatibleHelper`.

These cover the helper directly (since gemini is its first consumer) so
the upcoming 10+ OpenAI-compat adapters can rely on it without each
duplicating the same transport tests.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any

import pytest

from circuitry.adapters import GeminiAdapter, build_adapter
from circuitry.adapters._openai_compat import (
    OpenAICompatibleConfig,
    chat_completion,
    check_dependencies,
)
from circuitry.adapters.conformance import validate_generate_result


@dataclass(frozen=True)
class FakeProc:
    returncode: int
    stdout: str = ""
    stderr: str = ""


def _ok_payload(text: str = "hello", *, sent: int = 5, recv: int = 3) -> str:
    return json.dumps(
        {
            "choices": [{"message": {"content": text}}],
            "usage": {"prompt_tokens": sent, "completion_tokens": recv},
        }
    )


# ---------- helper-level ----------


def test_chat_completion_happy_path(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("GOOGLE_API_KEY", "test-key")

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        return FakeProc(returncode=0, stdout=_ok_payload("hi from gemini"))

    monkeypatch.setattr("subprocess.run", fake_run)

    cfg = OpenAICompatibleConfig(
        base_url="https://example.test/v1",
        api_key_env="GOOGLE_API_KEY",
        default_model="gemini-2.5-flash",
    )
    result = chat_completion(cfg=cfg, model="gemini-2.5-pro", prompt="ping")

    assert result.text == "hi from gemini"
    assert result.tokens_sent == 5
    assert result.tokens_received == 3
    assert validate_generate_result(result, adapter_name="gemini") == []
    cmd = captured["cmd"]
    # endpoint + auth + JSON body all built correctly
    assert cmd[-1] == "https://example.test/v1/chat/completions"
    assert "Authorization: Bearer test-key" in cmd
    assert any("gemini-2.5-pro" in c for c in cmd if c.startswith("{"))


def test_chat_completion_missing_api_key_raises(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("GOOGLE_API_KEY", raising=False)
    cfg = OpenAICompatibleConfig(
        base_url="https://example.test/v1",
        api_key_env="GOOGLE_API_KEY",
        default_model="gemini-2.5-flash",
    )
    with pytest.raises(RuntimeError, match="GOOGLE_API_KEY"):
        chat_completion(cfg=cfg, model="gemini-2.5-flash", prompt="ping")


def test_chat_completion_no_auth_for_self_hosted(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """vllm/llama.cpp/LM Studio style: no api key required."""

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        # No Authorization header should be present.
        assert not any(c.startswith("Authorization") for c in cmd)
        return FakeProc(returncode=0, stdout=_ok_payload())

    monkeypatch.setattr("subprocess.run", fake_run)

    cfg = OpenAICompatibleConfig(
        base_url="http://localhost:8000/v1",
        api_key_env="",
        default_model="llama3",
    )
    result = chat_completion(cfg=cfg, model="llama3", prompt="ping")
    assert result.text == "hello"


def test_chat_completion_curl_failure_masks_api_key(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    secret = "sk-super-secret-12345"
    monkeypatch.setenv("PROVIDER_KEY", secret)

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=22, stderr="HTTP 401 Unauthorized")

    monkeypatch.setattr("subprocess.run", fake_run)

    cfg = OpenAICompatibleConfig(
        base_url="https://example.test/v1",
        api_key_env="PROVIDER_KEY",
        default_model="m",
    )
    with pytest.raises(RuntimeError) as exc:
        chat_completion(cfg=cfg, model="m", prompt="ping")
    assert secret not in str(exc.value)


def test_chat_completion_curl_failure_masks_extra_header_secret(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Regression for #246: adapters that pass a credential via
    `extra_headers` (Azure's `api-key`, and any future provider that isn't
    Bearer-authenticated) must have that value masked too — the old code
    only ever masked `api_key`, the Bearer-path credential."""
    secret = "sk-extra-header-canary-999"

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(
            returncode=22,
            stdout=json.dumps({"error": {"message": "bad credential"}}),
            stderr="curl: (22) The requested URL returned error: 401",
        )

    monkeypatch.setattr("subprocess.run", fake_run)

    cfg = OpenAICompatibleConfig(
        base_url="https://example.test/v1",
        api_key_env="",
        default_model="m",
    )
    with pytest.raises(RuntimeError) as exc:
        chat_completion(
            cfg=cfg, model="m", prompt="ping", extra_headers={"api-key": secret}
        )

    message = str(exc.value)
    assert secret not in message
    assert "cmd=" not in message
    assert "bad credential" in message


def test_chat_completion_curl_failure_does_not_mask_short_header_values(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Regression for #246 follow-up: only header values long enough to be
    a plausible credential are masked, so a short, non-secret header (e.g.
    an API version number) doesn't get replaced with `***` wherever its
    value happens to reappear in the message."""

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(
            returncode=22,
            stdout=json.dumps({"error": {"message": "bad version 1"}}),
            stderr="curl: (22) The requested URL returned error: 401",
        )

    monkeypatch.setattr("subprocess.run", fake_run)

    cfg = OpenAICompatibleConfig(
        base_url="https://example.test/v1",
        api_key_env="",
        default_model="m",
    )
    with pytest.raises(RuntimeError) as exc:
        chat_completion(
            cfg=cfg, model="m", prompt="ping", extra_headers={"api-version": "1"}
        )

    assert "bad version 1" in str(exc.value)


def test_chat_completion_curl_failure_does_not_leak_base_url_credentials(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Regression for #246 follow-up: a self-hosted OpenAI-compatible proxy
    configured with credentials embedded in its base_url (rather than via
    `api_key_env`) must not have them echoed into the error message."""

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=22, stderr="HTTP 401 Unauthorized")

    monkeypatch.setattr("subprocess.run", fake_run)

    cfg = OpenAICompatibleConfig(
        base_url="https://user:canarypw@example.test/v1",
        api_key_env="",
        default_model="m",
    )
    with pytest.raises(RuntimeError) as exc:
        chat_completion(cfg=cfg, model="m", prompt="ping")

    assert "canarypw" not in str(exc.value)


def test_chat_completion_non_json_response_raises(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("GOOGLE_API_KEY", "k")

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=0, stdout="<html>503</html>")

    monkeypatch.setattr("subprocess.run", fake_run)

    cfg = OpenAICompatibleConfig(
        base_url="https://example.test/v1",
        api_key_env="GOOGLE_API_KEY",
        default_model="m",
    )
    with pytest.raises(RuntimeError, match="non-JSON response"):
        chat_completion(cfg=cfg, model="m", prompt="ping")


def test_chat_completion_empty_choices_returns_empty_text(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("GOOGLE_API_KEY", "k")

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=0, stdout=json.dumps({"choices": []}))

    monkeypatch.setattr("subprocess.run", fake_run)

    cfg = OpenAICompatibleConfig(
        base_url="https://example.test/v1",
        api_key_env="GOOGLE_API_KEY",
        default_model="m",
    )
    result = chat_completion(cfg=cfg, model="m", prompt="ping")
    assert result.text == ""
    assert result.tokens_sent is None


# ---------- adapter-level ----------


def test_gemini_adapter_conformance(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("GOOGLE_API_KEY", "test-key")

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=0, stdout=_ok_payload("hi"))

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = GeminiAdapter()
    result = adapter.generate(model="gemini-2.5-flash", prompt="ping")

    assert result.text == "hi"
    assert validate_generate_result(result, adapter_name="gemini") == []


def test_gemini_check_reports_missing_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("GOOGLE_API_KEY", raising=False)
    r = GeminiAdapter().check()
    assert r.ok is False
    assert "env:GOOGLE_API_KEY" in r.missing


def test_gemini_check_ok_with_env(monkeypatch: pytest.MonkeyPatch) -> None:
    import shutil

    if shutil.which("curl") is None:
        pytest.skip("curl not on PATH")
    monkeypatch.setenv("GOOGLE_API_KEY", "test-key")
    r = GeminiAdapter().check()
    assert r.ok is True


def test_gemini_factory_builds_adapter() -> None:
    adapter = build_adapter(adapter_name="gemini", runtime={})
    assert adapter.name == "gemini"
    assert isinstance(adapter, GeminiAdapter)


def test_gemini_factory_respects_runtime_overrides() -> None:
    adapter = build_adapter(
        adapter_name="gemini",
        runtime={
            "adapters": {
                "gemini": {
                    "base_url": "https://example.test/v2",
                    "default_model": "gemini-2.5-pro",
                }
            }
        },
    )
    assert isinstance(adapter, GeminiAdapter)
    assert adapter.base_url == "https://example.test/v2"
    assert adapter.default_model == "gemini-2.5-pro"


def test_check_dependencies_reports_missing_curl(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr("shutil.which", lambda name: None)
    monkeypatch.setenv("GOOGLE_API_KEY", "k")
    cfg = OpenAICompatibleConfig(
        base_url="https://example.test/v1",
        api_key_env="GOOGLE_API_KEY",
        default_model="m",
    )
    r = check_dependencies(cfg)
    assert r.ok is False
    assert "binary:curl" in r.missing
