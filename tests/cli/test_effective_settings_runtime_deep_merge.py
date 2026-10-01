"""A trusted document's `runtime:` block merges into the host config key by
key for `plugins.<name>` and `adapters.<name>`, not whole-section replace.
Regression suite for #316.

Before this, `_merge_runtime` merged `runtime` one level deep only: a
document setting `runtime.plugins.sqlite.*` dropped every other
`runtime.plugins.*` entry the host configured, including the shell allowlist
pin `runtime.plugins.shell.allowed_commands` (#308); a document setting one
adapter's timeout dropped every other adapter's config the same way.

`plugins.shell.allowed_commands` is also a ceiling: the host pin intersects
with whatever a trusted document sets there rather than being replaced
outright (the shell half of #264) -- unlike every other key, which a
document's own value simply overrides.
"""

from __future__ import annotations

from circuitry.adapters import build_adapter
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.effective_settings import resolve_effective_settings
from circuitry.plugins.factory import build_plugin


def _trusted_cfg(**overrides: object) -> CircuitryConfig:
    fields: dict[str, object] = {"trust_orchestration_runtime": True}
    fields.update(overrides)
    return CircuitryConfig(**fields)  # type: ignore[arg-type]


def test_document_setting_another_plugin_keeps_the_host_shell_pin() -> None:
    cfg = _trusted_cfg(
        runtime={
            "plugins": {
                "shell": {"allowed_commands": ["ls", "cat"]},
            }
        }
    )
    orch = {"runtime": {"plugins": {"sqlite": {"path": "./doc.db"}}}}

    effective = resolve_effective_settings(cfg=cfg, orch=orch)

    assert effective.runtime["plugins"]["sqlite"] == {"path": "./doc.db"}
    assert effective.runtime["plugins"]["shell"]["allowed_commands"] == ["ls", "cat"]

    plugin = build_plugin(plugin_name="shell", runtime=effective.runtime)
    assert plugin.pinned_allowed_commands == ("ls", "cat")  # type: ignore[attr-defined]


def test_document_setting_one_adapters_timeout_keeps_the_others() -> None:
    cfg = _trusted_cfg(
        runtime={
            "adapters": {
                "openai": {"base_url": "https://cfg.example", "timeout_seconds": 60},
                "anthropic": {"timeout_seconds": 90},
            }
        }
    )
    orch = {"runtime": {"adapters": {"ollama": {"timeout_seconds": 1800}}}}

    effective = resolve_effective_settings(cfg=cfg, orch=orch, cli_adapter="openai")

    assert effective.runtime["adapters"]["ollama"] == {"timeout_seconds": 1800}
    assert effective.runtime["adapters"]["openai"] == {
        "base_url": "https://cfg.example",
        "timeout_seconds": 60,
    }
    assert effective.runtime["adapters"]["anthropic"]["timeout_seconds"] == 90
    # The selected adapter's timeout came from config, not this document --
    # the document only touched a different adapter's block.
    assert effective.sources["adapters.openai.timeout_seconds"] == "config"

    adapter = build_adapter(adapter_name="openai", runtime=effective.runtime)
    assert adapter.base_url == "https://cfg.example"  # type: ignore[attr-defined]


def test_adapter_timeout_source_is_orchestration_only_for_that_adapters_own_key() -> None:
    cfg = _trusted_cfg(
        runtime={"adapters": {"ollama": {"timeout_seconds": 60}}},
    )
    orch = {"runtime": {"adapters": {"ollama": {"timeout_seconds": 1800}}}}

    effective = resolve_effective_settings(cfg=cfg, orch=orch, cli_adapter="ollama")

    assert effective.runtime["adapters"]["ollama"]["timeout_seconds"] == 1800
    assert effective.sources["adapters.ollama.timeout_seconds"] == "orchestration"


def test_document_only_narrows_the_shell_allowlist_pin_never_widens_it() -> None:
    """The host pin is a ceiling even for a trusted document (#316): the
    effective allowlist is the intersection, never the document's list
    outright."""
    cfg = _trusted_cfg(
        runtime={"plugins": {"shell": {"allowed_commands": ["ls", "cat", "echo"]}}}
    )
    orch = {
        "runtime": {"plugins": {"shell": {"allowed_commands": ["cat", "echo", "rm"]}}}
    }

    effective = resolve_effective_settings(cfg=cfg, orch=orch)

    assert effective.runtime["plugins"]["shell"]["allowed_commands"] == ["cat", "echo"]

    plugin = build_plugin(plugin_name="shell", runtime=effective.runtime)
    assert plugin.pinned_allowed_commands == ("cat", "echo")  # type: ignore[attr-defined]


def test_document_shell_allowlist_applies_outright_when_host_has_no_pin() -> None:
    """With no host pin to intersect against, a trusted document's own
    allowlist still applies as before."""
    cfg = _trusted_cfg()
    orch = {"runtime": {"plugins": {"shell": {"allowed_commands": ["rm"]}}}}

    effective = resolve_effective_settings(cfg=cfg, orch=orch)

    assert effective.runtime["plugins"]["shell"]["allowed_commands"] == ["rm"]


def test_document_shell_other_key_untouched_by_the_ceiling() -> None:
    """A document that sets a shell key other than `allowed_commands` keeps
    the host's pin unchanged -- the ceiling only narrows that one leaf."""
    cfg = _trusted_cfg(
        runtime={"plugins": {"shell": {"allowed_commands": ["ls", "cat"]}}}
    )
    orch = {"runtime": {"plugins": {"shell": {"env": {"FOO": "bar"}}}}}

    effective = resolve_effective_settings(cfg=cfg, orch=orch)

    assert effective.runtime["plugins"]["shell"]["allowed_commands"] == ["ls", "cat"]
    assert effective.runtime["plugins"]["shell"]["env"] == {"FOO": "bar"}
