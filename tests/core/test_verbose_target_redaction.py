"""#819: the verbose progress labels built by `core/prompt.py::_adapter_target`
and `core/tool.py::_plugin_target` fall back to the raw `base_url` when it
has no hostname `urlparse` can find (no scheme, userinfo hidden behind the
path, ...). That fallback must redact, never echo the configured value raw.
"""

from __future__ import annotations

from dataclasses import dataclass

from circuitry.core.prompt import _adapter_target
from circuitry.core.tool import _plugin_target


@dataclass
class _FakeAdapter:
    name: str
    base_url: str


@dataclass
class _FakePlugin:
    name: str
    base_url: str


def test_adapter_target_redacts_a_hostless_base_url() -> None:
    password = "pw_value_pw_value"
    adapter = _FakeAdapter(name="ollama", base_url=f"alice:{password}@host:1234")

    label = _adapter_target(adapter, "llama3")

    assert password not in label
    assert "alice" not in label
    assert label == "ollama · llama3 @ host:1234"


def test_adapter_target_uses_the_real_hostname_when_present() -> None:
    adapter = _FakeAdapter(name="ollama", base_url="http://example.test:11434")

    assert _adapter_target(adapter, "llama3") == "ollama · llama3 @ example.test"


def test_plugin_target_redacts_a_hostless_base_url() -> None:
    password = "pw_value_pw_value"
    plugin = _FakePlugin(name="comfyui", base_url=f"alice:{password}@host:8188")

    label = _plugin_target(plugin)

    assert password not in label
    assert "alice" not in label
    assert label == "comfyui @ host:8188"


def test_plugin_target_uses_the_real_hostname_when_present() -> None:
    plugin = _FakePlugin(name="comfyui", base_url="http://localhost:8188")

    assert _plugin_target(plugin) == "comfyui @ localhost"
