"""The optional ``list_models()`` hook and its forgiving shim.

The contract under test is *degradation*: a picker asking an adapter what
it offers must get an answer or an empty list, never an exception — no
matter how absent, unreachable, or badly behaved the adapter is.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any

import pytest

from circuitry.adapters import AnthropicAdapter, CyberdinerAdapter, OllamaAdapter
from circuitry.adapters.cyberdiner import SEED_TIERS
from circuitry.adapters.models import (
    MAX_MODELS,
    ModelLister,
    call_list_models,
    list_adapter_models,
)


@dataclass
class FakeResponse:
    payload: bytes

    def read(self) -> bytes:
        return self.payload

    def __enter__(self) -> FakeResponse:
        return self

    def __exit__(self, *args: Any) -> None:
        return None


def fake_urlopen(payload: Any, *, seen: list[Any] | None = None) -> Any:
    def _open(url: Any, timeout: float | None = None) -> FakeResponse:
        if seen is not None:
            seen.append((url, timeout))
        return FakeResponse(json.dumps(payload).encode("utf-8"))

    return _open


# -- the shim ----------------------------------------------------------------


def test_adapter_without_list_models_reports_nothing() -> None:
    class Bare:
        name = "bare"

    assert call_list_models(Bare()) == []


def test_a_raising_list_models_is_not_an_error() -> None:
    class Angry:
        def list_models(self) -> list[str]:
            raise RuntimeError("backend is on fire")

    assert call_list_models(Angry()) == []


def test_junk_return_values_are_discarded() -> None:
    class Junk:
        def list_models(self) -> Any:
            return ["  a  ", "", None, 7, "a", "b"]

    # Stripped, de-duplicated, non-strings dropped, order preserved.
    assert call_list_models(Junk()) == ["a", "b"]

    class NotEvenAList:
        def list_models(self) -> Any:
            return {"models": ["a"]}

    assert call_list_models(NotEvenAList()) == []


def test_a_flood_of_models_is_capped() -> None:
    class Flood:
        def list_models(self) -> list[str]:
            return [f"m{i}" for i in range(MAX_MODELS + 50)]

    assert len(call_list_models(Flood())) == MAX_MODELS


def test_model_lister_is_structural() -> None:
    assert isinstance(OllamaAdapter(), ModelLister)

    class Bare:
        name = "bare"

    assert not isinstance(Bare(), ModelLister)


# -- ollama ------------------------------------------------------------------


def test_ollama_lists_installed_tags(monkeypatch: pytest.MonkeyPatch) -> None:
    seen: list[Any] = []
    payload = {
        "models": [
            {"name": "gpt-oss:20b"},
            {"name": "phi3:mini"},
            {"name": "llama3.2:latest"},
        ]
    }
    monkeypatch.setattr(
        "circuitry.adapters.ollama.urllib.request.urlopen",
        fake_urlopen(payload, seen=seen),
    )

    adapter = OllamaAdapter(base_url="http://box:11434/")
    assert adapter.list_models() == ["gpt-oss:20b", "llama3.2:latest", "phi3:mini"]

    url, timeout = seen[0]
    assert url == "http://box:11434/api/tags"
    # Short enough that a dead daemon does not hold a picker hostage.
    assert timeout is not None and timeout <= 5


def test_ollama_degrades_to_empty_when_the_daemon_is_down(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def boom(url: Any, timeout: float | None = None) -> Any:
        raise OSError("connection refused")

    monkeypatch.setattr("circuitry.adapters.ollama.urllib.request.urlopen", boom)
    assert OllamaAdapter().list_models() == []


@pytest.mark.parametrize(
    "payload", [{}, {"models": "nope"}, {"models": [{"size": 1}, "x"]}, []]
)
def test_ollama_survives_a_surprising_payload(
    monkeypatch: pytest.MonkeyPatch, payload: Any
) -> None:
    monkeypatch.setattr(
        "circuitry.adapters.ollama.urllib.request.urlopen", fake_urlopen(payload)
    )
    assert OllamaAdapter().list_models() == []


# -- cyberdiner --------------------------------------------------------------


def test_cyberdiner_offers_seed_tiers_by_default() -> None:
    assert CyberdinerAdapter().list_models() == list(SEED_TIERS)


def test_configured_valid_tiers_win() -> None:
    adapter = CyberdinerAdapter(valid_tiers=("cheap", "house-special"))
    assert adapter.list_models() == ["cheap", "house-special"]


# -- anthropic ---------------------------------------------------------------


def test_anthropic_lists_current_models_without_a_network_call() -> None:
    models = AnthropicAdapter().list_models()
    assert "claude-sonnet-5" in models
    assert "claude-opus-5" in models
    assert "claude-haiku-4-5" in models


def test_anthropics_default_model_is_one_it_offers() -> None:
    adapter = AnthropicAdapter()
    assert adapter.default_model in adapter.list_models()


# -- building from config ----------------------------------------------------


def test_list_adapter_models_builds_from_runtime_config(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    seen: list[Any] = []
    monkeypatch.setattr(
        "circuitry.adapters.ollama.urllib.request.urlopen",
        fake_urlopen({"models": [{"name": "phi3:mini"}]}, seen=seen),
    )
    models = list_adapter_models(
        adapter_name="ollama",
        runtime={"adapters": {"ollama": {"base_url": "http://elsewhere:11434"}}},
    )
    assert models == ["phi3:mini"]
    assert seen[0][0] == "http://elsewhere:11434/api/tags"


def test_unbuildable_and_unknown_adapters_report_nothing() -> None:
    assert list_adapter_models(adapter_name="nonesuch", runtime={}) == []
    # host_claude needs an injected handler, so it cannot be built here.
    assert list_adapter_models(adapter_name="host_claude", runtime={}) == []


def test_an_adapter_without_the_hook_is_unaffected() -> None:
    assert list_adapter_models(adapter_name="openai", runtime={}) == []
