"""Tests for adapters whose URL or auth shape doesn't fit the generic
parametrized catalog: azure-openai, cloudflare-workers-ai, databricks,
replicate, watsonx.

cyberdiner has its own dedicated suite in test_cyberdiner.py — its
submit/poll job-broker shape doesn't fit this file's curl-based fakes.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import time
from dataclasses import dataclass
from typing import Any

import pytest
from curl_test_support import (
    RecordingJSONHandler,
    assert_not_in_argv,
    assert_q_first,
    local_server,
    read_config,
    read_config_url,
)

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
        captured["url"], captured["headers"] = read_config(cmd)
        return FakeProc(returncode=0, stdout=_ok_chat_payload("azure"))

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = build_adapter(adapter_name="azure-openai", runtime={})
    result = adapter.generate(model="my-deployment", prompt="ping")
    assert result.text == "azure"
    assert validate_generate_result(result, adapter_name="azure-openai") == []

    url = captured["url"]
    assert (
        url
        == "https://my-resource.openai.azure.com/openai/deployments/"
        "my-deployment/chat/completions?api-version=2024-10-21"
    )
    assert captured["headers"]["api-key"] == "secret-k"
    # Azure does NOT use Bearer auth.
    assert "Authorization" not in captured["headers"]
    assert_q_first(captured["cmd"])
    assert_not_in_argv(captured["cmd"], "secret-k", "ping")


def test_azure_curl_failure_masks_api_key_sent_via_extra_headers(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Regression for #246: Azure sends its key through `extra_headers`
    rather than the shared helper's Bearer path (`api_key_env=""`), so the
    old "mask api_key if non-empty" logic never ran and the key leaked into
    the error via the echoed curl command line.
    """
    secret = "sk-azure-canary-123"
    monkeypatch.setenv("AZURE_OPENAI_API_KEY", secret)
    monkeypatch.setenv(
        "AZURE_OPENAI_ENDPOINT", "https://example.invalid"
    )

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(
            returncode=22,
            stdout=json.dumps({"error": {"message": "DeploymentNotFound"}}),
            stderr="curl: (22) The requested URL returned error: 404",
        )

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = build_adapter(adapter_name="azure-openai", runtime={})
    with pytest.raises(RuntimeError) as exc:
        adapter.generate(model="dep", prompt="x")

    message = str(exc.value)
    assert secret not in message
    assert "cmd=" not in message
    assert "DeploymentNotFound" in message


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
        captured["url"] = read_config_url(cmd)
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
    assert "2024-08-01-preview" in captured["url"]


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
        captured["url"] = read_config_url(cmd)
        return FakeProc(returncode=0, stdout=_ok_chat_payload("cf"))

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = build_adapter(adapter_name="cloudflare-workers-ai", runtime={})
    result = adapter.generate(model="@cf/meta/llama-3.3", prompt="ping")
    assert result.text == "cf"
    url = captured["url"]
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
        captured["url"] = read_config_url(cmd)
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
    assert "from-config" in captured["url"]
    assert "from-env" not in captured["url"]


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
        captured["url"] = read_config_url(cmd)
        return FakeProc(returncode=0, stdout=_ok_chat_payload("db"))

    monkeypatch.setattr("subprocess.run", fake_run)
    adapter = build_adapter(adapter_name="databricks", runtime={})
    adapter.generate(model="endpoint-name", prompt="p")
    assert (
        captured["url"]
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
        captured["url"], captured["headers"] = read_config(cmd)
        return FakeProc(returncode=0, stdout=json.dumps(payload))

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = build_adapter(adapter_name="replicate", runtime={})
    result = adapter.generate(model="meta/meta-llama-3-70b-instruct", prompt="hi")
    assert result.text == "hello world"
    assert validate_generate_result(result, adapter_name="replicate") == []
    headers = captured["headers"]
    # `Prefer: wait=...` header must be sent.
    assert headers["Prefer"].startswith("wait=")
    # And model in URL.
    assert (
        "/v1/models/meta/meta-llama-3-70b-instruct/predictions"
        in captured["url"]
    )
    assert_q_first(captured["cmd"])
    assert_not_in_argv(captured["cmd"], "r-tok", "hi")


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


def test_replicate_large_input_over_200kib_sent_on_stdin_not_argv(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Regression for #300: a Replicate input carrying a base64 image can
    outgrow argv's 128 KiB-per-argument ceiling; it must go on stdin."""
    monkeypatch.setenv("REPLICATE_API_TOKEN", "r-tok")
    large_prompt = "a" * (250 * 1024)

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        captured["input"] = kwargs.get("input")
        return FakeProc(
            returncode=0,
            stdout=json.dumps({"id": "abc", "status": "succeeded", "output": ["ok"]}),
        )

    monkeypatch.setattr("subprocess.run", fake_run)
    adapter = build_adapter(adapter_name="replicate", runtime={})
    adapter.generate(model="meta/m", prompt=large_prompt)
    assert len(captured["input"]) > 200 * 1024
    assert_not_in_argv(captured["cmd"], large_prompt[:200])


class _ReplicateHandler(RecordingJSONHandler):
    response_body = json.dumps(
        {"id": "abc", "status": "succeeded", "output": ["hello from local server"]}
    ).encode()


@pytest.mark.skipif(shutil.which("curl") is None, reason="curl not on PATH")
def test_replicate_end_to_end_against_local_server(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Real curl, real local server (never a live provider): the token
    header and a large prompt both arrive correctly and neither touches
    argv."""
    secret = "canary-replicate-token"
    large_prompt = "canary prompt " * 20000
    monkeypatch.setenv("REPLICATE_API_TOKEN", secret)

    calls: list[list[str]] = []
    real_run = subprocess.run

    def spying_run(cmd: list[str], **kwargs: Any) -> Any:
        calls.append(cmd)
        return real_run(cmd, **kwargs)

    monkeypatch.setattr("subprocess.run", spying_run)

    with local_server(_ReplicateHandler) as base_url:
        adapter = build_adapter(
            adapter_name="replicate",
            runtime={"adapters": {"replicate": {"base_url": base_url}}},
        )
        result = adapter.generate(model="meta/m", prompt=large_prompt, timeout_seconds=10)

    assert result.text == "hello from local server"
    assert _ReplicateHandler.captured_headers["Authorization"] == f"Bearer {secret}"
    assert large_prompt.encode() in _ReplicateHandler.captured_body
    for cmd in calls:
        assert_q_first(cmd)
        assert_not_in_argv(cmd, secret, large_prompt[:200])


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
    headers_by_call: list[dict[str, str]] = []

    urls_by_call: list[str | None] = []

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        calls.append(list(cmd))
        url, headers = read_config(cmd)
        headers_by_call.append(headers)
        urls_by_call.append(url)
        if url and "iam.cloud.ibm.com" in url:
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
    assert "iam.cloud.ibm.com" in (urls_by_call[0] or "")
    assert "ml/v1/text/generation" in (urls_by_call[1] or "")
    assert headers_by_call[1]["Authorization"] == "Bearer iam-token-xyz"
    assert_q_first(calls[0])
    assert_q_first(calls[1])
    assert_not_in_argv(calls[0], "ibm-key")
    assert_not_in_argv(calls[1], "iam-token-xyz", "ping")


def test_watsonx_token_cache_avoids_second_iam_call(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _seed_watsonx_env(monkeypatch)

    calls: list[str] = []

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        url = read_config_url(cmd) or ""
        calls.append(url)
        if "iam.cloud.ibm.com" in url:
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


def test_watsonx_iam_apikey_form_field_never_touches_argv(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Regression for #264 part 3: the IAM exchange's `apikey=` form field
    is a secret and must travel on stdin with the rest of the body, not as
    a `-d` argument."""
    secret = "ibm-canary-apikey-777"
    monkeypatch.setenv("WATSONX_API_KEY", secret)
    monkeypatch.setenv("WATSONX_PROJECT_ID", "p")
    from circuitry.adapters import watsonx as watsonx_mod

    watsonx_mod._TOKEN_CACHE.clear()

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        captured["input"] = kwargs.get("input")
        return FakeProc(
            returncode=0,
            stdout=json.dumps({"access_token": "tok", "expires_in": 3600}),
        )

    monkeypatch.setattr("subprocess.run", fake_run)
    watsonx_mod._get_token(secret, timeout_seconds=10)
    assert_q_first(captured["cmd"])
    assert_not_in_argv(captured["cmd"], secret)
    assert secret in (captured["input"] or "")


def test_watsonx_large_prompt_over_200kib_sent_on_stdin_not_argv(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Regression for #300."""
    _seed_watsonx_env(monkeypatch)
    large_prompt = "a" * (250 * 1024)

    calls: list[dict[str, Any]] = []

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        calls.append({"cmd": cmd, "input": kwargs.get("input")})
        url = read_config_url(cmd) or ""
        if "iam.cloud.ibm.com" in url:
            return FakeProc(
                returncode=0,
                stdout=json.dumps({"access_token": "tok", "expires_in": 3600}),
            )
        return FakeProc(
            returncode=0,
            stdout=json.dumps({"results": [{"generated_text": "ok"}]}),
        )

    monkeypatch.setattr("subprocess.run", fake_run)
    adapter = build_adapter(adapter_name="watsonx", runtime={})
    adapter.generate(model="m", prompt=large_prompt)

    gen_call = calls[1]
    assert len(gen_call["input"]) > 200 * 1024
    assert_not_in_argv(gen_call["cmd"], large_prompt[:200])


class _WatsonXGenerationHandler(RecordingJSONHandler):
    response_body = json.dumps(
        {"results": [{"generated_text": "hi from local watsonx"}]}
    ).encode()


@pytest.mark.skipif(shutil.which("curl") is None, reason="curl not on PATH")
def test_watsonx_generation_end_to_end_against_local_server(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Real curl, real local server standing in for the generation endpoint
    (the IAM token exchange is pre-seeded to skip IBM's hardcoded URL, since
    that URL can't be redirected to a local server): the Bearer token and a
    large prompt both arrive correctly and neither touches argv."""
    secret = "canary-watsonx-key"
    monkeypatch.setenv("WATSONX_API_KEY", secret)
    monkeypatch.setenv("WATSONX_PROJECT_ID", "proj-1")
    from circuitry.adapters import watsonx as watsonx_mod

    watsonx_mod._TOKEN_CACHE[secret] = ("canary-iam-token", time.time() + 3600)

    large_prompt = "canary prompt " * 20000

    calls: list[list[str]] = []
    real_run = subprocess.run

    def spying_run(cmd: list[str], **kwargs: Any) -> Any:
        calls.append(cmd)
        return real_run(cmd, **kwargs)

    monkeypatch.setattr("subprocess.run", spying_run)

    with local_server(_WatsonXGenerationHandler) as base_url:
        adapter = build_adapter(
            adapter_name="watsonx",
            runtime={"adapters": {"watsonx": {"base_url": base_url}}},
        )
        result = adapter.generate(model="m", prompt=large_prompt, timeout_seconds=10)

    assert result.text == "hi from local watsonx"
    assert (
        _WatsonXGenerationHandler.captured_headers["Authorization"]
        == "Bearer canary-iam-token"
    )
    assert large_prompt.encode() in _WatsonXGenerationHandler.captured_body
    for cmd in calls:
        assert_q_first(cmd)
        assert_not_in_argv(cmd, secret, "canary-iam-token", large_prompt[:200])


def test_watsonx_iam_429_classifies_as_retryable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """#263 part 2 regression: the IAM token exchange previously raised a
    bare ``RuntimeError`` that never retried; it now raises
    :class:`AdapterCallError` classified from curl's exit/stderr, the same
    way the main generation request already was."""
    from circuitry.adapters._retry import AdapterCallError

    monkeypatch.setenv("WATSONX_API_KEY", "k")
    monkeypatch.setenv("WATSONX_PROJECT_ID", "p")
    from circuitry.adapters import watsonx as watsonx_mod

    watsonx_mod._TOKEN_CACHE.clear()

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(
            returncode=22,
            stderr="curl: (22) The requested URL returned error: 429",
        )

    monkeypatch.setattr("subprocess.run", fake_run)
    adapter = build_adapter(adapter_name="watsonx", runtime={})
    with pytest.raises(AdapterCallError) as exc:
        adapter.generate(model="m", prompt="p")
    assert exc.value.retry_info.retryable is True
    assert exc.value.retry_info.status == 429


def test_watsonx_iam_failure_retries_through_the_prompt_retry_loop(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("WATSONX_API_KEY", "k")
    monkeypatch.setenv("WATSONX_PROJECT_ID", "p")
    from circuitry.adapters import watsonx as watsonx_mod
    from circuitry.core.compiler import compile_orchestration
    from circuitry.core.dynamic import DynamicRuntime
    from circuitry.core.store import Store

    watsonx_mod._TOKEN_CACHE.clear()

    calls = {"n": 0}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        url = read_config_url(cmd) or ""
        if "iam.cloud.ibm.com" in url:
            calls["n"] += 1
            if calls["n"] == 1:
                return FakeProc(
                    returncode=22,
                    stderr="curl: (22) The requested URL returned error: 503",
                )
            return FakeProc(
                returncode=0,
                stdout=json.dumps({"access_token": "tok", "expires_in": 3600}),
            )
        return FakeProc(
            returncode=0,
            stdout=json.dumps({"results": [{"generated_text": "ok"}]}),
        )

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = build_adapter(adapter_name="watsonx", runtime={})
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "task",
                "template": "hi",
                "retries": {"max_attempts": 2, "backoff_ms": 0},
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})

    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    assert store.get("prime.task.value") == "ok"
    assert store.get("prime.task.meta.retries_used") == 1
    assert calls["n"] == 2
