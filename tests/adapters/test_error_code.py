"""Issue #660: `AdapterCallError.error_code` for every adapter that can
carry one.

Each curl-based adapter's failure path is exercised the same way
`test_curl_hygiene.py` already does for its message — a fake
``subprocess.Popen`` standing in for curl, never a real network call.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any

import pytest

from circuitry.adapters._retry import AdapterCallError
from circuitry.adapters.anthropic import AnthropicAdapter
from circuitry.adapters.factory import build_adapter
from circuitry.adapters.ollama import OllamaAdapter
from circuitry.adapters.openai import OpenAIAdapter
from circuitry.adapters.replicate import ReplicateAdapter


@dataclass(frozen=True)
class FakeProc:
    returncode: int
    stdout: str = ""
    stderr: str = ""

    def communicate(self, input: object = None, timeout: object = None) -> tuple[str, str]:
        return (self.stdout, self.stderr)

    def __enter__(self) -> FakeProc:
        return self

    def __exit__(self, *exc: object) -> bool:
        return False


def _fake_popen(returncode: int, stdout: str, stderr: str = "HTTP 400"):
    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=returncode, stdout=stdout, stderr=stderr)

    return fake_run


def test_openai_adapter_call_error_carries_error_code(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("OPENAI_API_KEY", "sk-canary")
    monkeypatch.setattr(
        "subprocess.Popen",
        _fake_popen(22, json.dumps({"error": {"code": "context_length_exceeded"}})),
    )
    with pytest.raises(AdapterCallError) as exc:
        OpenAIAdapter().generate(model="m", prompt="x")
    assert exc.value.error_code == "context_length_exceeded"


def test_openai_compatible_adapter_call_error_falls_back_to_type(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        "subprocess.Popen",
        _fake_popen(
            22, json.dumps({"error": {"code": None, "type": "server_error"}}), "HTTP 503"
        ),
    )
    adapter = build_adapter(adapter_name="vllm", runtime={})
    with pytest.raises(AdapterCallError) as exc:
        adapter.generate(model="m", prompt="x")
    assert exc.value.error_code == "server_error"


def test_anthropic_adapter_call_error_carries_error_type(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-ant-canary")
    monkeypatch.setattr(
        "subprocess.Popen",
        _fake_popen(22, json.dumps({"type": "error", "error": {"type": "overloaded_error"}})),
    )
    with pytest.raises(AdapterCallError) as exc:
        AnthropicAdapter().generate(model="m", prompt="x")
    assert exc.value.error_code == "overloaded_error"


def test_ollama_adapter_call_error_has_no_code_for_a_flat_body(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        "subprocess.Popen",
        _fake_popen(22, json.dumps({"error": "model 'm' not found"})),
    )
    with pytest.raises(AdapterCallError) as exc:
        OllamaAdapter().generate(model="m", prompt="x")
    assert exc.value.error_code is None


def test_replicate_adapter_call_error_carries_error_code(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("REPLICATE_API_TOKEN", "r8-canary")
    monkeypatch.setattr(
        "subprocess.Popen",
        _fake_popen(22, json.dumps({"error": {"code": "rate_limit_exceeded"}}), "HTTP 429"),
    )
    with pytest.raises(AdapterCallError) as exc:
        ReplicateAdapter().generate(model="owner/model", prompt="x")
    assert exc.value.error_code == "rate_limit_exceeded"


def test_watsonx_generate_adapter_call_error_carries_error_code(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("WATSONX_API_KEY", "k")
    monkeypatch.setenv("WATSONX_PROJECT_ID", "p")
    from circuitry.adapters import watsonx as watsonx_mod

    watsonx_mod._TOKEN_CACHE.clear()
    watsonx_mod._TOKEN_CACHE["k"] = ("cached-token", 9_999_999_999.0)

    monkeypatch.setattr(
        "subprocess.Popen",
        _fake_popen(22, json.dumps({"error": {"code": "invalid_input"}}), "HTTP 400"),
    )
    with pytest.raises(AdapterCallError) as exc:
        watsonx_mod.WatsonXAdapter().generate(model="m", prompt="x")
    assert exc.value.error_code == "invalid_input"
    watsonx_mod._TOKEN_CACHE.clear()


def test_watsonx_iam_exchange_adapter_call_error_carries_error_code(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("WATSONX_API_KEY", "k")
    monkeypatch.setenv("WATSONX_PROJECT_ID", "p")
    from circuitry.adapters import watsonx as watsonx_mod

    watsonx_mod._TOKEN_CACHE.clear()
    monkeypatch.setattr(
        "subprocess.Popen",
        _fake_popen(22, json.dumps({"errorCode": "BXNIM0415E"}), "HTTP 400"),
    )
    with pytest.raises(AdapterCallError) as exc:
        watsonx_mod.WatsonXAdapter().generate(model="m", prompt="x")
    # IAM's own error shape (`errorCode`) isn't the generic `error.code`
    # rule this issue covers, so no code is read from it.
    assert exc.value.error_code is None


def test_litellm_adapter_call_error_reads_the_exceptions_own_code(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import circuitry.adapters.litellm as litellm_mod

    class _FakeLitellmError(Exception):
        def __init__(self) -> None:
            super().__init__("boom")
            self.code = "insufficient_quota"
            self.status_code = 429

    class _FakeLitellmModule:
        @staticmethod
        def completion(**kwargs: Any) -> Any:
            raise _FakeLitellmError()

    monkeypatch.setitem(__import__("sys").modules, "litellm", _FakeLitellmModule())
    with pytest.raises(AdapterCallError) as exc:
        litellm_mod.LiteLLMAdapter().generate(model="openai/gpt-4o-mini", prompt="x")
    assert exc.value.error_code == "insufficient_quota"


def test_litellm_adapter_call_error_falls_back_to_type(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import circuitry.adapters.litellm as litellm_mod

    class _FakeLitellmError(Exception):
        def __init__(self) -> None:
            super().__init__("boom")
            self.type = "rate_limit_error"
            self.status_code = 429

    class _FakeLitellmModule:
        @staticmethod
        def completion(**kwargs: Any) -> Any:
            raise _FakeLitellmError()

    monkeypatch.setitem(__import__("sys").modules, "litellm", _FakeLitellmModule())
    with pytest.raises(AdapterCallError) as exc:
        litellm_mod.LiteLLMAdapter().generate(model="openai/gpt-4o-mini", prompt="x")
    assert exc.value.error_code == "rate_limit_error"


def test_litellm_adapter_call_error_has_no_code_without_one(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import circuitry.adapters.litellm as litellm_mod

    class _FakeLitellmModule:
        @staticmethod
        def completion(**kwargs: Any) -> Any:
            raise TimeoutError("timed out")

    monkeypatch.setitem(__import__("sys").modules, "litellm", _FakeLitellmModule())
    with pytest.raises(AdapterCallError) as exc:
        litellm_mod.LiteLLMAdapter().generate(model="openai/gpt-4o-mini", prompt="x")
    assert exc.value.error_code is None
