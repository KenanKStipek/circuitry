"""Tests for adapters whose URL or auth shape doesn't fit the generic
parametrized catalog: azure-openai, cloudflare-workers-ai, databricks,
replicate, watsonx.

cyberdiner has its own dedicated suite in test_cyberdiner.py — its
submit/poll job-broker shape doesn't fit this file's curl-based fakes.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any

import pytest

from circuitry.adapters import build_adapter
from circuitry.adapters.azure_openai import AzureOpenAIAdapter
from circuitry.adapters.cloudflare_workers_ai import CloudflareWorkersAIAdapter
from circuitry.adapters.conformance import validate_generate_result
from circuitry.adapters.databricks import DatabricksAdapter
from circuitry.adapters.replicate import ReplicateAdapter
from circuitry.adapters.watsonx import WatsonXAdapter


@dataclass(frozen=True)
class FakeProc:
    returncode: int
    stdout: str = ""
    stderr: str = ""


# ---------------------------------------------------------------------------
# azure-openai
# ---------------------------------------------------------------------------


def _ok_chat_payload(text: str = "hi") -> str:
    return json.dumps(
        {
            "choices": [{"message": {"content": text}}],
            "usage": {"prompt_tokens": 4, "completion_tokens": 2},
        }
    )


def test_azure_url_includes_deployment_and_api_version(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AZURE_OPENAI_API_KEY", "secret-k")
    monkeypatch.setenv(
        "AZURE_OPENAI_ENDPOINT", "https://my-resource.openai.azure.com"
    )

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        return FakeProc(returncode=0, stdout=_ok_chat_payload("azure"))

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = build_adapter(adapter_name="azure-openai", runtime={})
    result = adapter.generate(model="my-deployment", prompt="ping")
    assert result.text == "azure"
    assert validate_generate_result(result, adapter_name="azure-openai") == []

    url = captured["cmd"][-1]
    assert (
        url
        == "https://my-resource.openai.azure.com/openai/deployments/"
        "my-deployment/chat/completions?api-version=2024-10-21"
    )
    assert "api-key: secret-k" in captured["cmd"]
    # Azure does NOT use Bearer auth.
    assert not any(
        c.startswith("Authorization: Bearer") for c in captured["cmd"]
    )


def test_azure_check_reports_missing_endpoint(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("AZURE_OPENAI_ENDPOINT", raising=False)
    monkeypatch.delenv("AZURE_OPENAI_API_KEY", raising=False)
    r = AzureOpenAIAdapter().check()
    assert r.ok is False
    assert "env:AZURE_OPENAI_API_KEY" in r.missing
    assert "env:AZURE_OPENAI_ENDPOINT" in r.missing


def test_azure_runtime_overrides_api_version(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AZURE_OPENAI_API_KEY", "k")

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        return FakeProc(returncode=0, stdout=_ok_chat_payload())

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = build_adapter(
        adapter_name="azure-openai",
        runtime={
            "adapters": {
                "azure-openai": {
                    "endpoint": "https://other.openai.azure.com",
                    "api_version": "2024-08-01-preview",
                }
            }
        },
    )
    adapter.generate(model="dep", prompt="ping")
    assert "2024-08-01-preview" in captured["cmd"][-1]


# ---------------------------------------------------------------------------
# cloudflare-workers-ai
# ---------------------------------------------------------------------------


def test_cloudflare_resolves_account_id_from_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("CF_ACCOUNT_ID", "abc123")
    monkeypatch.setenv("CF_API_TOKEN", "tok")

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        return FakeProc(returncode=0, stdout=_ok_chat_payload("cf"))

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = build_adapter(adapter_name="cloudflare-workers-ai", runtime={})
    result = adapter.generate(model="@cf/meta/llama-3.3", prompt="ping")
    assert result.text == "cf"
    url = captured["cmd"][-1]
    assert (
        url
        == "https://api.cloudflare.com/client/v4/accounts/abc123/ai/v1/chat/completions"
    )


def test_cloudflare_check_reports_missing_account_and_token(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("CF_ACCOUNT_ID", raising=False)
    monkeypatch.delenv("CF_API_TOKEN", raising=False)
    r = CloudflareWorkersAIAdapter().check()
    assert r.ok is False
    assert "env:CF_API_TOKEN" in r.missing
    assert "env:CF_ACCOUNT_ID" in r.missing


def test_cloudflare_runtime_account_id_overrides_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("CF_ACCOUNT_ID", "from-env")
    monkeypatch.setenv("CF_API_TOKEN", "tok")

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        return FakeProc(returncode=0, stdout=_ok_chat_payload())

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = build_adapter(
        adapter_name="cloudflare-workers-ai",
        runtime={
            "adapters": {
                "cloudflare-workers-ai": {"account_id": "from-config"},
            }
        },
    )
    adapter.generate(model="@cf/m", prompt="ping")
    assert "from-config" in captured["cmd"][-1]
    assert "from-env" not in captured["cmd"][-1]


# ---------------------------------------------------------------------------
# databricks
# ---------------------------------------------------------------------------


def test_databricks_resolves_host_from_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("DATABRICKS_HOST", "adb-1234.5.azuredatabricks.net")
    monkeypatch.setenv("DATABRICKS_TOKEN", "tok")

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        return FakeProc(returncode=0, stdout=_ok_chat_payload("db"))

    monkeypatch.setattr("subprocess.run", fake_run)
    adapter = build_adapter(adapter_name="databricks", runtime={})
    adapter.generate(model="endpoint-name", prompt="p")
    assert (
        captured["cmd"][-1]
        == "https://adb-1234.5.azuredatabricks.net/serving-endpoints/chat/completions"
    )


def test_databricks_check_reports_missing_host(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("DATABRICKS_HOST", raising=False)
    monkeypatch.delenv("DATABRICKS_TOKEN", raising=False)
    r = DatabricksAdapter().check()
    assert r.ok is False
    assert "env:DATABRICKS_HOST" in r.missing
    assert "env:DATABRICKS_TOKEN" in r.missing


# ---------------------------------------------------------------------------
# replicate
# ---------------------------------------------------------------------------


def test_replicate_synchronous_succeeded(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("REPLICATE_API_TOKEN", "r-tok")

    payload = {
        "id": "abc",
        "status": "succeeded",
        "output": ["hello ", "world"],
    }

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        return FakeProc(returncode=0, stdout=json.dumps(payload))

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = build_adapter(adapter_name="replicate", runtime={})
    result = adapter.generate(model="meta/meta-llama-3-70b-instruct", prompt="hi")
    assert result.text == "hello world"
    assert validate_generate_result(result, adapter_name="replicate") == []
    # `Prefer: wait=...` header must be sent.
    assert any(c.startswith("Prefer: wait=") for c in captured["cmd"])
    # And model in URL.
    assert (
        "/v1/models/meta/meta-llama-3-70b-instruct/predictions"
        in captured["cmd"][-1]
    )


def test_replicate_still_processing_raises_with_id(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("REPLICATE_API_TOKEN", "tok")
    payload = {"id": "p123", "status": "processing"}

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=0, stdout=json.dumps(payload))

    monkeypatch.setattr("subprocess.run", fake_run)
    adapter = build_adapter(adapter_name="replicate", runtime={})
    with pytest.raises(RuntimeError, match="p123"):
        adapter.generate(model="meta/m", prompt="hi")


def test_replicate_rejects_model_without_owner_slash() -> None:
    adapter = ReplicateAdapter()
    import os
    os.environ["REPLICATE_API_TOKEN"] = "k"
    try:
        with pytest.raises(ValueError, match="owner/name"):
            adapter.generate(model="just-a-name", prompt="p")
    finally:
        os.environ.pop("REPLICATE_API_TOKEN", None)


def test_replicate_check_reports_missing_token(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("REPLICATE_API_TOKEN", raising=False)
    r = ReplicateAdapter().check()
    assert r.ok is False
    assert "env:REPLICATE_API_TOKEN" in r.missing


def test_replicate_curl_failure_masks_token(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    secret = "r8_super_secret_42"
    monkeypatch.setenv("REPLICATE_API_TOKEN", secret)

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=22, stderr="HTTP 401")

    monkeypatch.setattr("subprocess.run", fake_run)
    adapter = build_adapter(adapter_name="replicate", runtime={})
    with pytest.raises(RuntimeError) as exc:
        adapter.generate(model="meta/m", prompt="p")
    assert secret not in str(exc.value)


# ---------------------------------------------------------------------------
# watsonx
# ---------------------------------------------------------------------------


def _seed_watsonx_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("WATSONX_API_KEY", "ibm-key")
    monkeypatch.setenv("WATSONX_PROJECT_ID", "proj-1")
    # Reset module-level token cache between tests.
    from circuitry.adapters import watsonx as watsonx_mod

    watsonx_mod._TOKEN_CACHE.clear()


def test_watsonx_two_step_iam_then_generate(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _seed_watsonx_env(monkeypatch)

    # First subprocess.run call → IAM token; second → generation.
    calls: list[list[str]] = []

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        calls.append(list(cmd))
        if "iam.cloud.ibm.com" in " ".join(cmd):
            return FakeProc(
                returncode=0,
                stdout=json.dumps(
                    {"access_token": "iam-token-xyz", "expires_in": 3600}
                ),
            )
        # generation
        return FakeProc(
            returncode=0,
            stdout=json.dumps(
                {
                    "results": [
                        {
                            "generated_text": "from watsonx",
                            "input_token_count": 5,
                            "generated_token_count": 3,
                        }
                    ]
                }
            ),
        )

    monkeypatch.setattr("subprocess.run", fake_run)
    adapter = build_adapter(adapter_name="watsonx", runtime={})
    result = adapter.generate(model="meta-llama/llama-3-3-70b-instruct", prompt="ping")
    assert result.text == "from watsonx"
    assert result.tokens_sent == 5
    assert result.tokens_received == 3
    assert validate_generate_result(result, adapter_name="watsonx") == []

    # IAM call first, generation second.
    assert "iam.cloud.ibm.com" in " ".join(calls[0])
    gen_cmd = " ".join(calls[1])
    assert "ml/v1/text/generation" in gen_cmd
    assert "Authorization: Bearer iam-token-xyz" in calls[1]


def test_watsonx_token_cache_avoids_second_iam_call(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _seed_watsonx_env(monkeypatch)

    calls: list[str] = []

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        joined = " ".join(cmd)
        calls.append(joined)
        if "iam.cloud.ibm.com" in joined:
            return FakeProc(
                returncode=0,
                stdout=json.dumps(
                    {"access_token": "tok", "expires_in": 3600}
                ),
            )
        return FakeProc(
            returncode=0,
            stdout=json.dumps({"results": [{"generated_text": "ok"}]}),
        )

    monkeypatch.setattr("subprocess.run", fake_run)
    adapter = build_adapter(adapter_name="watsonx", runtime={})
    adapter.generate(model="m", prompt="a")
    adapter.generate(model="m", prompt="b")

    iam_calls = [c for c in calls if "iam.cloud.ibm.com" in c]
    assert len(iam_calls) == 1  # only the first generate hit IAM


def test_watsonx_check_reports_missing_envs(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("WATSONX_API_KEY", raising=False)
    monkeypatch.delenv("WATSONX_PROJECT_ID", raising=False)
    r = WatsonXAdapter().check()
    assert r.ok is False
    assert "env:WATSONX_API_KEY" in r.missing
    assert "env:WATSONX_PROJECT_ID" in r.missing


def test_watsonx_iam_failure_masks_api_key(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    secret = "ibm-super-secret-42"
    monkeypatch.setenv("WATSONX_API_KEY", secret)
    monkeypatch.setenv("WATSONX_PROJECT_ID", "p")
    from circuitry.adapters import watsonx as watsonx_mod

    watsonx_mod._TOKEN_CACHE.clear()

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=22, stderr=f"HTTP 401: bad key {secret}")

    monkeypatch.setattr("subprocess.run", fake_run)
    adapter = build_adapter(adapter_name="watsonx", runtime={})
    with pytest.raises(RuntimeError) as exc:
        adapter.generate(model="m", prompt="p")
    assert secret not in str(exc.value)
