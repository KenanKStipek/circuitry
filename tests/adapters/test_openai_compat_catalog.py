"""Parametrized tests for the OpenAI-compatible adapter catalog.

The 11 adapters in this batch (groq, openrouter, perplexity, xai,
deepseek, together, fireworks, nvidia-nim, vllm, llamacpp, lmstudio)
all delegate transport to ``adapters/_openai_compat.py``. The transport
itself is exercised in detail by ``test_gemini.py``; this file
verifies the per-provider config (factory wiring, API key env var,
default base URL, conformance contract) without re-testing curl plumbing.
"""

from __future__ import annotations

import json
import shutil
from dataclasses import dataclass
from typing import Any

import pytest

from circuitry.adapters import build_adapter
from circuitry.adapters.conformance import validate_generate_result


@dataclass(frozen=True)
class FakeProc:
    returncode: int
    stdout: str = ""
    stderr: str = ""


def _ok_payload(text: str = "hello") -> str:
    return json.dumps(
        {
            "choices": [{"message": {"content": text}}],
            "usage": {"prompt_tokens": 4, "completion_tokens": 2},
        }
    )


# (adapter_name, env_var or "" if self-hosted, expected base_url substring)
CATALOG: list[tuple[str, str, str]] = [
    ("groq", "GROQ_API_KEY", "api.groq.com"),
    ("openrouter", "OPENROUTER_API_KEY", "openrouter.ai"),
    ("perplexity", "PERPLEXITY_API_KEY", "api.perplexity.ai"),
    ("xai", "XAI_API_KEY", "api.x.ai"),
    ("deepseek", "DEEPSEEK_API_KEY", "api.deepseek.com"),
    ("together", "TOGETHER_API_KEY", "api.together.xyz"),
    ("fireworks", "FIREWORKS_API_KEY", "api.fireworks.ai"),
    ("nvidia-nim", "NIM_API_KEY", "integrate.api.nvidia.com"),
    ("vllm", "", "localhost:8000"),
    ("llamacpp", "", "localhost:8080"),
    ("lmstudio", "", "localhost:1234"),
    # Round 2 — helper-drop adapters added in the second adapter batch.
    ("mistral", "MISTRAL_API_KEY", "api.mistral.ai"),
    ("ai21", "AI21_API_KEY", "api.ai21.com"),
    ("huggingface-inference", "HF_TOKEN", "router.huggingface.co"),
    ("tgi", "", "localhost:3000"),
    ("qwen-dashscope", "DASHSCOPE_API_KEY", "dashscope-intl.aliyuncs.com"),
    ("cohere", "COHERE_API_KEY", "api.cohere.com"),
]


@pytest.mark.parametrize("name,env_var,base_url_substr", CATALOG)
def test_factory_builds_each_adapter(
    name: str, env_var: str, base_url_substr: str
) -> None:
    adapter = build_adapter(adapter_name=name, runtime={})
    assert adapter.name == name
    assert base_url_substr in adapter.base_url


@pytest.mark.parametrize("name,env_var,base_url_substr", CATALOG)
def test_check_reports_missing_env_when_required(
    monkeypatch: pytest.MonkeyPatch,
    name: str,
    env_var: str,
    base_url_substr: str,
) -> None:
    """Cloud-hosted adapters must surface missing API key; self-hosted ones
    have no env requirement and report ok unless curl is missing."""
    if env_var:
        monkeypatch.delenv(env_var, raising=False)
    adapter = build_adapter(adapter_name=name, runtime={})
    r = adapter.check()
    if env_var:
        assert r.ok is False
        assert f"env:{env_var}" in r.missing
    else:
        if shutil.which("curl") is None:
            assert r.ok is False
            assert "binary:curl" in r.missing
        else:
            assert r.ok is True


@pytest.mark.parametrize("name,env_var,base_url_substr", CATALOG)
def test_generate_with_mocked_transport_passes_conformance(
    monkeypatch: pytest.MonkeyPatch,
    name: str,
    env_var: str,
    base_url_substr: str,
) -> None:
    """Each adapter, with mocked curl, returns a contract-conforming
    GenerateResult."""
    if env_var:
        monkeypatch.setenv(env_var, "test-key")

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        return FakeProc(returncode=0, stdout=_ok_payload(f"hi from {name}"))

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = build_adapter(adapter_name=name, runtime={})
    result = adapter.generate(model="any-model", prompt="ping")

    assert result.text == f"hi from {name}"
    assert validate_generate_result(result, adapter_name=name) == []
    # Self-hosted adapters must NOT add an Authorization header.
    has_auth = any(
        isinstance(c, str) and c.startswith("Authorization: Bearer")
        for c in captured["cmd"]
    )
    assert has_auth == bool(env_var)


@pytest.mark.parametrize("name,env_var,base_url_substr", CATALOG)
def test_runtime_overrides_base_url_and_default_model(
    name: str, env_var: str, base_url_substr: str
) -> None:
    """Per-adapter runtime config (under runtime.adapters.<name>) wins."""
    adapter = build_adapter(
        adapter_name=name,
        runtime={
            "adapters": {
                name: {
                    "base_url": "https://override.test/v1",
                    "default_model": "override-model",
                }
            }
        },
    )
    assert adapter.base_url == "https://override.test/v1"
    assert adapter.default_model == "override-model"


def test_self_hosted_generate_succeeds_without_api_key(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """vllm/llamacpp/lmstudio: no env var required; transport must not
    add an auth header."""
    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        return FakeProc(returncode=0, stdout=_ok_payload("local-llm"))

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = build_adapter(adapter_name="vllm", runtime={})
    result = adapter.generate(model="my-local-model", prompt="ping")
    assert result.text == "local-llm"
    assert not any(
        isinstance(c, str) and c.startswith("Authorization") for c in captured["cmd"]
    )


def test_nvidia_nim_can_disable_auth_via_runtime_override(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Self-hosted NIM containers don't need an API key — runtime
    override of api_key_env to empty string disables the auth header
    requirement."""
    monkeypatch.delenv("NIM_API_KEY", raising=False)

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        return FakeProc(returncode=0, stdout=_ok_payload("ok"))

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = build_adapter(
        adapter_name="nvidia-nim",
        runtime={
            "adapters": {
                "nvidia-nim": {
                    "api_key_env": "",
                    "base_url": "http://localhost:8000/v1",
                }
            }
        },
    )
    result = adapter.generate(model="m", prompt="ping")
    assert result.text == "ok"
    assert not any(
        isinstance(c, str) and c.startswith("Authorization") for c in captured["cmd"]
    )
