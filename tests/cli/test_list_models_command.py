"""`cof list --models <adapter>` — the CLI half of the TUI model picker."""

from __future__ import annotations

import json
from typing import Any

import pytest
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


class FakeResponse:
    def __init__(self, payload: dict[str, Any]) -> None:
        self._payload = payload

    def read(self) -> bytes:
        return json.dumps(self._payload).encode("utf-8")

    def __enter__(self) -> FakeResponse:
        return self

    def __exit__(self, *args: Any) -> None:
        return None


def _patch_opener_open(monkeypatch: pytest.MonkeyPatch, payload: dict[str, Any]) -> None:
    """Replaces every opener's own ``open`` -- ``OllamaAdapter.list_models``
    now always builds an explicit opener, H1 (issue #778 step 7, H-review
    P2-15), rather than calling the bare ``urllib.request.urlopen``
    convenience function; this is the one choke point either call shape
    goes through (the same idiom ``tests/plugins/test_http.py`` uses).
    """

    def bound(self: Any, req: Any, data: Any = None, timeout: Any = None) -> Any:
        return FakeResponse(payload)

    monkeypatch.setattr("urllib.request.OpenerDirector.open", bound)


def test_models_for_an_adapter(monkeypatch: pytest.MonkeyPatch) -> None:
    payload = {"models": [{"name": "gpt-oss:20b"}, {"name": "phi3:mini"}]}
    _patch_opener_open(monkeypatch, payload)

    result = runner.invoke(app, ["list", "--models", "ollama", "--json"])
    assert result.exit_code == 0
    assert json.loads(result.output) == {
        "adapter": "ollama",
        "models": ["gpt-oss:20b", "phi3:mini"],
    }


def test_models_table_render(monkeypatch: pytest.MonkeyPatch) -> None:
    _patch_opener_open(monkeypatch, {"models": [{"name": "phi3:mini"}]})
    result = runner.invoke(app, ["list", "--models", "ollama"])
    assert result.exit_code == 0
    assert "phi3:mini" in result.output


def test_an_adapter_that_cannot_enumerate_is_not_a_failure() -> None:
    result = runner.invoke(app, ["list", "--models", "openai"])
    assert result.exit_code == 0
    assert "No models reported by openai" in result.output
    # The escape hatch is named, so the user is never stuck.
    assert "--model" in result.output


def test_unknown_adapter_is_an_error() -> None:
    result = runner.invoke(app, ["list", "--models", "nonesuch"])
    assert result.exit_code == 1
    assert "Unknown adapter" in result.output


def test_unknown_adapter_json_still_parses() -> None:
    result = runner.invoke(app, ["list", "--models", "nonesuch", "--json"])
    assert result.exit_code == 1
    assert json.loads(result.output)["models"] == []
