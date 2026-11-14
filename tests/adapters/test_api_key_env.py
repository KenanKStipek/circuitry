"""Cross-cutting tests for `runtime.adapters.<name>.api_key_env` (#669): a
per-adapter override of which environment variable supplies the credential,
for the adapters not already covered by `test_openai_compat_catalog.py`'s
parametrized sweep (openai, anthropic, replicate, watsonx, azure-openai,
cloudflare-workers-ai, databricks), plus the shared factory validator and
the config-only security rule enforced in `cli.effective_settings`.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any

import pytest
from curl_test_support import assert_not_in_argv, read_config, read_config_headers

from circuitry.adapters import build_adapter
from circuitry.adapters.factory import adapter_api_key_env_errors
from circuitry.cli.effective_settings import (
    orchestration_host_setting_warnings,
    resolve_effective_settings,
)


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


def _ok_chat_payload(text: str = "hi") -> str:
    return json.dumps(
        {
            "choices": [{"message": {"content": text}}],
            "usage": {"prompt_tokens": 4, "completion_tokens": 2},
        }
    )


# ---------------------------------------------------------------------------
# openai / anthropic
# ---------------------------------------------------------------------------


def test_openai_api_key_env_override(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    monkeypatch.setenv("OPENAI_TEAM_KEY", "team-key-value")

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        captured["headers"] = read_config_headers(cmd)
        return FakeProc(returncode=0, stdout=_ok_chat_payload("ok"))

    monkeypatch.setattr("subprocess.Popen", fake_run)

    adapter = build_adapter(
        adapter_name="openai",
        runtime={"adapters": {"openai": {"api_key_env": "OPENAI_TEAM_KEY", "stream": False}}},
    )
    result = adapter.generate(model="gpt-4o-mini", prompt="ping")
    assert result.text == "ok"
    assert captured["headers"]["Authorization"] == "Bearer team-key-value"
    assert_not_in_argv(captured["cmd"], "team-key-value")


def test_openai_api_key_env_override_missing_reports_named_variable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("OPENAI_API_KEY", "set-but-unused")
    monkeypatch.delenv("OPENAI_TEAM_KEY", raising=False)
    adapter = build_adapter(
        adapter_name="openai",
        runtime={"adapters": {"openai": {"api_key_env": "OPENAI_TEAM_KEY"}}},
    )
    r = adapter.check()
    assert r.ok is False
    assert r.missing == ["env:OPENAI_TEAM_KEY"]


def test_anthropic_api_key_env_override(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    monkeypatch.setenv("CLAUDE_TEAM_KEY", "claude-team-value")

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        captured["headers"] = read_config_headers(cmd)
        return FakeProc(returncode=0, stdout=_ok_chat_payload())

    monkeypatch.setattr("subprocess.Popen", fake_run)

    adapter = build_adapter(
        adapter_name="anthropic",
        runtime={
            "adapters": {"anthropic": {"api_key_env": "CLAUDE_TEAM_KEY", "stream": False}}
        },
    )
    adapter.generate(model="claude-sonnet-5", prompt="ping")
    assert captured["headers"]["x-api-key"] == "claude-team-value"
    assert_not_in_argv(captured["cmd"], "claude-team-value")


# ---------------------------------------------------------------------------
# replicate / watsonx / azure-openai / cloudflare-workers-ai / databricks
# ---------------------------------------------------------------------------


def test_replicate_api_key_env_override(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("REPLICATE_API_TOKEN", raising=False)
    monkeypatch.setenv("REPLICATE_TEAM_TOKEN", "replicate-team-value")

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        captured["headers"] = read_config_headers(cmd)
        return FakeProc(
            returncode=0,
            stdout=json.dumps({"id": "abc", "status": "succeeded", "output": ["ok"]}),
        )

    monkeypatch.setattr("subprocess.Popen", fake_run)

    adapter = build_adapter(
        adapter_name="replicate",
        runtime={"adapters": {"replicate": {"api_key_env": "REPLICATE_TEAM_TOKEN"}}},
    )
    result = adapter.generate(model="meta/meta-llama-3-70b-instruct", prompt="hi")
    assert result.text == "ok"
    assert captured["headers"]["Authorization"] == "Bearer replicate-team-value"
    assert_not_in_argv(captured["cmd"], "replicate-team-value")


def test_watsonx_api_key_env_override(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("WATSONX_API_KEY", raising=False)
    monkeypatch.setenv("WATSONX_TEAM_KEY", "watsonx-team-value")
    monkeypatch.setenv("WATSONX_PROJECT_ID", "proj-1")

    captured: dict[str, Any] = {}

    class _IamCapturingProc:
        def __init__(self, returncode: int, stdout: str) -> None:
            self.returncode = returncode
            self.stdout = stdout
            self.stderr = ""

        def communicate(
            self, input: str | None = None, timeout: object = None
        ) -> tuple[str, str]:
            if input is not None:
                captured.setdefault("iam_bodies", []).append(input)
            return (self.stdout, self.stderr)

        def __enter__(self) -> _IamCapturingProc:
            return self

        def __exit__(self, *exc: object) -> bool:
            return False

    def fake_run(cmd: list[str], **kwargs: Any) -> _IamCapturingProc:
        call = read_config(cmd)
        captured.setdefault("calls", []).append(call)
        url = call[0]
        if url and "iam.cloud.ibm.com" in url:
            return _IamCapturingProc(
                0, json.dumps({"access_token": "iam-token", "expires_in": 3600})
            )
        return _IamCapturingProc(0, json.dumps({"results": [{"generated_text": "ok"}]}))

    monkeypatch.setattr("subprocess.Popen", fake_run)

    adapter = build_adapter(
        adapter_name="watsonx",
        runtime={"adapters": {"watsonx": {"api_key_env": "WATSONX_TEAM_KEY"}}},
    )
    result = adapter.generate(model="m", prompt="p")
    assert result.text == "ok"
    iam_call = next(c for c in captured["calls"] if c[0] and "iam.cloud.ibm.com" in c[0])
    # The team key reached the IAM exchange (on stdin, never argv/headers)...
    assert any("apikey=watsonx-team-value" in body for body in captured["iam_bodies"])
    # ...and never anywhere read_config can see (URL/headers only).
    assert "watsonx-team-value" not in str(iam_call)


def test_azure_openai_api_key_env_override(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("AZURE_OPENAI_API_KEY", raising=False)
    monkeypatch.setenv("AZURE_TEAM_KEY", "azure-team-value")
    monkeypatch.setenv("AZURE_OPENAI_ENDPOINT", "https://my-resource.openai.azure.com")

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["headers"] = read_config_headers(cmd)
        return FakeProc(returncode=0, stdout=_ok_chat_payload("azure"))

    monkeypatch.setattr("subprocess.Popen", fake_run)

    adapter = build_adapter(
        adapter_name="azure-openai",
        runtime={
            "adapters": {"azure-openai": {"api_key_env": "AZURE_TEAM_KEY", "stream": False}}
        },
    )
    result = adapter.generate(model="dep", prompt="ping")
    assert result.text == "azure"
    assert captured["headers"]["api-key"] == "azure-team-value"
    assert "Authorization" not in captured["headers"]


def test_cloudflare_workers_ai_api_key_env_override(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("CF_API_TOKEN", raising=False)
    monkeypatch.setenv("CF_TEAM_TOKEN", "cf-team-value")
    monkeypatch.setenv("CF_ACCOUNT_ID", "abc123")

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["headers"] = read_config_headers(cmd)
        return FakeProc(returncode=0, stdout=_ok_chat_payload("cf"))

    monkeypatch.setattr("subprocess.Popen", fake_run)

    adapter = build_adapter(
        adapter_name="cloudflare-workers-ai",
        runtime={
            "adapters": {
                "cloudflare-workers-ai": {"api_key_env": "CF_TEAM_TOKEN", "stream": False}
            }
        },
    )
    result = adapter.generate(model="@cf/m", prompt="ping")
    assert result.text == "cf"
    assert captured["headers"]["Authorization"] == "Bearer cf-team-value"


def test_databricks_api_key_env_override(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("DATABRICKS_TOKEN", raising=False)
    monkeypatch.setenv("DATABRICKS_TEAM_TOKEN", "databricks-team-value")
    monkeypatch.setenv("DATABRICKS_HOST", "adb-1234.5.azuredatabricks.net")

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["headers"] = read_config_headers(cmd)
        return FakeProc(returncode=0, stdout=_ok_chat_payload("db"))

    monkeypatch.setattr("subprocess.Popen", fake_run)

    adapter = build_adapter(
        adapter_name="databricks",
        runtime={
            "adapters": {
                "databricks": {"api_key_env": "DATABRICKS_TEAM_TOKEN", "stream": False}
            }
        },
    )
    result = adapter.generate(model="endpoint", prompt="p")
    assert result.text == "db"
    assert captured["headers"]["Authorization"] == "Bearer databricks-team-value"


# ---------------------------------------------------------------------------
# shared validation (#666-style, checked up front)
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "value",
    ["not a name!", "1LEADING_DIGIT", 3, None, True, ["x"], {"x": 1}],
)
def test_adapter_api_key_env_errors_rejects_invalid_names(value: object) -> None:
    runtime = {"adapters": {"openai": {"api_key_env": value}}}
    errors = adapter_api_key_env_errors(runtime)
    assert errors == [
        (
            f"runtime.adapters.openai.api_key_env must be an environment "
            f"variable name, got {value!r}"
        ),
    ]


def test_adapter_api_key_env_errors_accepts_empty_string_and_valid_names() -> None:
    runtime = {
        "adapters": {
            "nvidia-nim": {"api_key_env": ""},
            "openai": {"api_key_env": "OPENAI_TEAM_KEY"},
        }
    }
    assert adapter_api_key_env_errors(runtime) == []


def test_adapter_api_key_env_errors_checked_on_a_built_in_block_too() -> None:
    """Not just aliases: a built-in adapter's own block is checked, the same
    way an alias's target is (#666)."""
    runtime = {"adapters": {"groq": {"api_key_env": "lower-case-bad"}}}
    assert adapter_api_key_env_errors(runtime) == [
        (
            "runtime.adapters.groq.api_key_env must be an environment variable "
            "name, got 'lower-case-bad'"
        ),
    ]


def test_build_adapter_raises_for_invalid_api_key_env_on_a_built_in() -> None:
    with pytest.raises(ValueError, match=r"runtime\.adapters\.openai\.api_key_env"):
        build_adapter(
            adapter_name="openai",
            runtime={"adapters": {"openai": {"api_key_env": "nope nope"}}},
        )


@pytest.mark.parametrize(
    "name", ["openai", "anthropic", "replicate", "watsonx", "azure-openai"]
)
def test_api_key_env_empty_string_is_rejected_for_an_always_keyed_adapter(name: str) -> None:
    """Unlike the OpenAI-compatible self-hosted family (vllm, llamacpp,
    lmstudio, nvidia-nim, tgi), these adapters always send a credential, so
    "" has no endpoint it could describe -- it is rejected like any other
    invalid name, both at build time and in `adapter_api_key_env_errors`."""
    with pytest.raises(ValueError, match=rf"runtime\.adapters\.{name}\.api_key_env"):
        build_adapter(
            adapter_name=name,
            runtime={"adapters": {name: {"api_key_env": ""}}},
        )
    assert adapter_api_key_env_errors({"adapters": {name: {"api_key_env": ""}}}) == [
        f"runtime.adapters.{name}.api_key_env must be an environment variable name, got ''",
    ]


@pytest.mark.parametrize(
    "name", ["openai", "anthropic", "replicate", "watsonx", "azure-openai"]
)
def test_api_key_env_empty_string_is_rejected_for_an_alias_of_an_always_keyed_adapter(
    name: str,
) -> None:
    with pytest.raises(ValueError, match=r"runtime\.adapters\.my_alias\.api_key_env"):
        build_adapter(
            adapter_name="my_alias",
            runtime={"adapters": {"my_alias": {"adapter": name, "api_key_env": ""}}},
        )
    assert adapter_api_key_env_errors(
        {"adapters": {"my_alias": {"adapter": name, "api_key_env": ""}}}
    ) == [
        "runtime.adapters.my_alias.api_key_env must be an environment variable name, got ''",
    ]


def test_adapter_api_key_env_errors_does_not_duplicate_an_alias_error() -> None:
    """An alias's own malformed `api_key_env` is reported once by
    `alias_config_errors` (building it); `adapter_api_key_env_errors` must
    not add the identical message a second time for the same entry --
    that de-duplication is `cli.runtime_shim.validate`'s job, exercised
    directly on the two error lists it combines."""
    from circuitry.adapters.factory import alias_config_errors

    runtime = {"adapters": {"openai_team": {"adapter": "openai", "api_key_env": ""}}}
    alias_errors = alias_config_errors(runtime)
    api_key_env_errors = adapter_api_key_env_errors(runtime)
    assert alias_errors == api_key_env_errors
    assert len(alias_errors) == 1


# ---------------------------------------------------------------------------
# config only (#669): a document never chooses it
# ---------------------------------------------------------------------------


def _minimal_config(runtime: dict[str, Any]) -> Any:
    from circuitry.cli.config import CircuitryConfig

    return CircuitryConfig(runtime=runtime)


def test_document_api_key_env_is_ignored_with_a_warning() -> None:
    cfg = _minimal_config({"adapters": {"openai": {}}})
    orch = {
        "runtime": {"adapters": {"openai": {"api_key_env": "ATTACKER_CHOSEN_VAR"}}},
    }
    warnings = orchestration_host_setting_warnings(
        orch, cfg, trust_document=True, document_name="evil.yml"
    )
    assert (
        "runtime.adapters.openai.api_key_env from evil.yml ignored: only config "
        "chooses which environment variable an adapter's key comes from"
    ) in warnings


def test_document_api_key_env_is_ignored_even_without_enabled_adapters() -> None:
    """Unlike the #666 alias-retarget rule, this one is not gated on
    `enabled_adapters` -- a document can never choose the variable, trusted
    or not."""
    cfg = _minimal_config({"adapters": {}})
    assert cfg.enabled_adapters is None
    effective = resolve_effective_settings(
        cfg=cfg,
        orch={
            "runtime": {
                "adapters": {"openai": {"api_key_env": "ATTACKER_CHOSEN_VAR"}}
            },
        },
        trust_document=True,
        document_name="evil.yml",
    )
    assert "api_key_env" not in effective.runtime.get("adapters", {}).get("openai", {})
    assert any("api_key_env from evil.yml ignored" in w for w in effective.warnings)


def test_document_api_key_env_does_not_reach_the_merged_runtime_at_all() -> None:
    cfg = _minimal_config(
        {"adapters": {"openai": {"api_key_env": "HOST_CHOSEN_VAR"}}}
    )
    effective = resolve_effective_settings(
        cfg=cfg,
        orch={
            "runtime": {
                "adapters": {"openai": {"api_key_env": "ATTACKER_CHOSEN_VAR"}}
            },
        },
        trust_document=True,
        document_name="evil.yml",
    )
    # The host's own choice survives; the document's attempt never applies.
    assert (
        effective.runtime["adapters"]["openai"]["api_key_env"] == "HOST_CHOSEN_VAR"
    )


def test_document_api_key_env_falls_back_to_the_adapters_own_default_variable() -> None:
    """When config names none at all (not merely a different one from the
    document's), the built adapter still reads its own default variable --
    the document's attempt never substitutes for it."""
    cfg = _minimal_config({"adapters": {}})
    effective = resolve_effective_settings(
        cfg=cfg,
        orch={
            "runtime": {
                "adapters": {"openai": {"api_key_env": "ATTACKER_CHOSEN_VAR"}}
            },
        },
        trust_document=True,
        document_name="evil.yml",
    )
    adapter = build_adapter(adapter_name="openai", runtime=effective.runtime)
    assert adapter.api_key_env == "OPENAI_API_KEY"
