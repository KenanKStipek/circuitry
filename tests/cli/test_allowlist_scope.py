"""The allowlists guard everything a run builds, not just the top document (#245).

`enabled_tools` / `enabled_adapters` used to be checked against the top-level
document's text only. These tests pin each path that stepped around that:
`use` children (path, ref, inline — static and templated), generated plans,
`--adapter`, the config's `default_adapter`, profile provider overrides, and
`--service-profile` dropping the allowlists altogether.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.allowlist_gate import AllowlistError, install_allowlists, require_tool
from circuitry.cli import runtime_shim
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run, validate
from circuitry.cli.shared_library import ServiceProfile, apply_service_profile
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.prompt import PromptDefinition, PromptRuntime
from circuitry.core.store import Store
from circuitry.core.tool import ToolDefinition, ToolRuntime

TOOLS_JSON_ONLY = CircuitryConfig(enabled_tools=["json"])
ADAPTERS_OLLAMA_ONLY = CircuitryConfig(enabled_adapters=["ollama"])

#: A harmless tool the allowlist leaves out — if it ever ran, it would only
#: print a UUID, but these tests assert it never gets that far.
DENIED_TOOL_CHILD = """\
effects:
  - type: tool
    name: t
    provider: uuid
    params: {}
"""


def _write(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def _inline_parent(tmp_path: Path, inline: str, *, before: str = "") -> Path:
    doc = {"effects": []}
    if before:
        doc["effects"].append(
            {"type": "tool", "name": before, "provider": "json",
             "params": {"op": "parse", "input": "{}"}}
        )
    doc["effects"].append({"type": "use", "name": "child", "inline": inline})
    return _write(tmp_path / "parent.yml", yaml.safe_dump(doc, sort_keys=False))


def _run(path: Path, config: CircuitryConfig, **kwargs: Any) -> runtime_shim.RunResult:
    # A `use` makes the run build an adapter; inject one unless the test is
    # about which adapter the run resolves (then it passes `adapter=None`).
    kwargs.setdefault("adapter", RecordingAdapter(name="stub"))
    return run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=config,
            skip_preflight=True,
            **kwargs,
        )
    )


@dataclass(frozen=True)
class RecordingAdapter:
    name: str
    calls: list = field(default_factory=list)

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        self.calls.append((model, prompt))
        return GenerateResult(text=f"{model}:{prompt}", raw={})


# ---------- static: cof check / validate walks `use` children ----------


def test_check_rejects_inline_child_tool(tmp_path: Path) -> None:
    """The issue's repro: an inline child's tool fails `cof check`."""
    parent = _inline_parent(tmp_path, DENIED_TOOL_CHILD)

    result = validate(parent, config=TOOLS_JSON_ONLY, skip_preflight=True)

    assert result["ok"] is False
    assert result["errors"] == [
        (
            "use child inline 'child': tool 'uuid' not in enabled_tools allowlist "
            "(enabled: ['json'])"
        )
    ]


def test_check_rejects_path_grandchild_tool(tmp_path: Path) -> None:
    """`path:` children are followed transitively, relative to each file."""
    _write(tmp_path / "sub" / "grandchild.yml", DENIED_TOOL_CHILD)
    _write(
        tmp_path / "sub" / "child.yml",
        "effects:\n  - type: use\n    name: deeper\n    path: grandchild.yml\n",
    )
    parent = _write(
        tmp_path / "parent.yml",
        "effects:\n  - type: use\n    name: child\n    path: sub/child.yml\n",
    )

    result = validate(parent, config=TOOLS_JSON_ONLY, skip_preflight=True)

    assert result["ok"] is False
    grandchild = (tmp_path / "sub" / "grandchild.yml").resolve()
    assert result["errors"] == [
        (
            f"use child {grandchild}: tool 'uuid' not in enabled_tools allowlist "
            "(enabled: ['json'])"
        )
    ]


def test_check_rejects_ref_child_adapter(tmp_path: Path) -> None:
    """`ref:` children resolve through the configured library sources."""
    library = tmp_path / "library"
    _write(
        library / "summarize.yml",
        "effects:\n  - type: prompt\n    name: p\n    provider: openai\n"
        "    template: hi\n",
    )
    parent = _write(
        tmp_path / "parent.yml",
        "effects:\n  - type: use\n    name: child\n    ref: summarize\n",
    )
    config = CircuitryConfig(
        enabled_adapters=["ollama"],
        runtime={
            "library": {
                "sources": [{"type": "folder", "name": "mine", "path": str(library)}]
            }
        },
    )

    result = validate(parent, config=config, skip_preflight=True)

    assert result["ok"] is False
    assert result["errors"] == [
        (
            f"use child {(library / 'summarize.yml').resolve()}: adapter 'openai' "
            "not in enabled_adapters allowlist (enabled: ['ollama'])"
        )
    ]


def test_check_accepts_children_inside_the_allowlist(tmp_path: Path) -> None:
    parent = _inline_parent(
        tmp_path,
        "effects:\n  - type: tool\n    name: t\n    provider: json\n"
        "    params: {op: parse, input: '{}'}\n",
    )

    result = validate(parent, config=TOOLS_JSON_ONLY, skip_preflight=True)

    assert result["ok"] is True, result["errors"]


def test_check_does_not_judge_a_use_childs_own_adapter_field(tmp_path: Path) -> None:
    """A child's `adapter:` is dead text — it runs on the adapter its parent
    already built (core/use.py), never on its own declared adapter."""
    parent = _inline_parent(
        tmp_path,
        "adapter: ollama\neffects:\n  - type: tool\n    name: t\n    provider: json\n"
        "    params: {op: parse, input: '{}'}\n",
    )
    config = CircuitryConfig(enabled_adapters=["openai"], enabled_tools=["json"])

    result = validate(parent, config=config, skip_preflight=True)

    assert result["ok"] is True, result["errors"]


def test_check_leaves_templated_names_to_the_run(tmp_path: Path) -> None:
    """`provider: "{{input.tool}}"` is not a name until the child renders."""
    parent = _inline_parent(
        tmp_path,
        'effects:\n  - type: tool\n    name: t\n    provider: "{{input.tool}}"\n',
    )

    result = validate(parent, config=TOOLS_JSON_ONLY, skip_preflight=True)

    assert result["ok"] is True, result["errors"]


# ---------- run: children, generated documents ----------


def test_run_rejects_inline_child_before_anything_runs(tmp_path: Path) -> None:
    parent = _inline_parent(tmp_path, DENIED_TOOL_CHILD, before="first")

    result = _run(parent, TOOLS_JSON_ONLY)

    assert result.ok is False
    assert result.error is not None
    assert result.error.startswith("Allowlist enforcement failed: use child")
    assert "tool 'uuid' not in enabled_tools allowlist" in result.error
    # Refused at the gate: not even the allowed sibling ran.
    assert "prime" not in result.state


def test_run_rejects_templated_inline_child_as_it_loads(tmp_path: Path) -> None:
    """A templated child passes the static check and is refused once rendered."""
    parent = _inline_parent(
        tmp_path,
        'effects:\n  - type: tool\n    name: t\n    provider: "{{input.tool}}"\n'
        "    params: {}\n",
    )

    result = _run(parent, TOOLS_JSON_ONLY, initial_state={"input": {"tool": "uuid"}})

    assert result.ok is False
    assert result.error is not None
    assert "use 'child': child inline failed allowlist enforcement" in result.error
    assert "tool 'uuid' not in enabled_tools allowlist" in result.error
    child = result.state["prime"]["child"]
    assert "t" not in child


def test_document_runtime_block_cannot_reopen_the_allowlists(tmp_path: Path) -> None:
    """The run's allowlists overwrite whatever a `runtime:` block tried to set."""
    doc = {
        "runtime": {"_allowlists": {"adapters": None, "tools": None}},
        "effects": [
            {
                "type": "use",
                "name": "child",
                "inline": 'effects:\n  - type: tool\n    name: t\n'
                '    provider: "{{input.tool}}"\n    params: {}\n',
            }
        ],
    }
    parent = _write(tmp_path / "parent.yml", yaml.safe_dump(doc, sort_keys=False))

    result = _run(parent, TOOLS_JSON_ONLY, initial_state={"input": {"tool": "uuid"}})

    assert result.ok is False
    assert "tool 'uuid' not in enabled_tools allowlist" in (result.error or "")


def test_run_ignores_a_use_childs_own_adapter_field(tmp_path: Path) -> None:
    """The child runs on the parent's already-built adapter, never on the
    build its own `adapter:` field would name — UseRuntime._check_allowlists
    must not judge it."""
    parent = _inline_parent(
        tmp_path,
        "adapter: ollama\neffects:\n  - type: prompt\n    name: greet\n    template: hi\n",
    )
    config = CircuitryConfig(enabled_adapters=["openai"], default_model="m")
    adapter = RecordingAdapter(name="openai")

    result = _run(parent, config, adapter=adapter)

    assert result.ok is True, result.error
    assert adapter.calls == [("m", "hi")]


def test_run_allows_children_when_no_allowlist_is_set(tmp_path: Path) -> None:
    parent = _inline_parent(tmp_path, DENIED_TOOL_CHILD)

    result = _run(parent, CircuitryConfig())

    assert result.ok is True, result.error


def test_require_tool_matches_an_uppercase_config_entry() -> None:
    """`enabled_tools: ["JSON"]` in config.json normalizes to lowercase
    (config.py's ``_normalize_allowlist``), so it matches the lowercase
    provider name the build-time gate canonicalises to."""
    cfg = CircuitryConfig.from_dict({"enabled_tools": ["JSON"]})

    require_tool("json", cfg.enabled_tools)  # must not raise


def test_tool_runtime_refuses_a_tool_outside_the_installed_allowlist(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The backstop: whatever reaches a tool effect is gated before the build."""
    built: list[str] = []
    monkeypatch.setattr(
        "circuitry.plugins.factory.build_plugin",
        lambda **kwargs: built.append(kwargs["plugin_name"]),
    )
    runtime_config: dict[str, Any] = {}
    install_allowlists(runtime_config, enabled_adapters=None, enabled_tools=["json"])
    store = Store({})
    runtime = ToolRuntime(
        ToolDefinition(name="t", provider="UUID", params={}),
        runtime_config=runtime_config,
    )

    with pytest.raises(AllowlistError, match="tool 'uuid' not in enabled_tools"):
        runtime.execute(store=store, ctx={})

    assert built == []


def test_prompt_runtime_refuses_an_adapter_outside_the_installed_allowlist(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    built: list[str] = []
    monkeypatch.setattr(
        "circuitry.core.prompt.build_adapter",
        lambda **kwargs: built.append(kwargs["adapter_name"]),
    )
    runtime_config: dict[str, Any] = {}
    install_allowlists(runtime_config, enabled_adapters=["primary"], enabled_tools=None)
    adapter = RecordingAdapter(name="primary")
    runtime = PromptRuntime(
        PromptDefinition(name="p", template="hi", provider="openai:gpt-4o"),
        adapter=adapter,
        model="m",
        runtime_config=runtime_config,
    )

    with pytest.raises(Exception, match="adapter 'openai' not in enabled_adapters"):
        runtime.execute(store=Store({}), ctx={})

    assert built == []
    assert adapter.calls == []


def test_prompt_runtime_refuses_a_denied_fallback_without_falling_through(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A ``provider_fallbacks`` entry outside the allowlist must fail the
    effect outright, the same as a denied ``provider:`` — not be logged as a
    merely-failed attempt and silently fallen through past to a later
    fallback that *is* allowed (#263's fallback-chain attempt-resolve fix
    must not reopen the allowlist gate it closed)."""
    allowed_fallback = RecordingAdapter(name="also-allowed")
    monkeypatch.setattr(
        "circuitry.core.prompt.build_adapter",
        lambda *, adapter_name, runtime: allowed_fallback,
    )
    runtime_config: dict[str, Any] = {}
    install_allowlists(
        runtime_config, enabled_adapters=["primary", "also-allowed"], enabled_tools=None
    )
    adapter = RecordingAdapter(name="primary")
    runtime = PromptRuntime(
        PromptDefinition(
            name="p",
            template="hi",
            provider="openai:gpt-4o",
            provider_fallbacks=["also-allowed:m2"],
        ),
        adapter=adapter,
        model="m",
        runtime_config=runtime_config,
    )

    with pytest.raises(AllowlistError, match="adapter 'openai' not in enabled_adapters"):
        runtime.execute(store=Store({}), ctx={})

    assert adapter.calls == []
    assert allowed_fallback.calls == []


def test_reflector_generated_plan_is_refused(tmp_path: Path) -> None:
    """A reflector's plan runs as an inline `use` child — same gate."""
    plan = yaml.safe_dump(
        {
            "done": False,
            "effects": [{"type": "tool", "name": "t", "provider": "uuid", "params": {}}],
        }
    )
    adapter = RecordingAdapter(name="primary")
    object.__setattr__(adapter, "generate", lambda **_: GenerateResult(text=plan, raw={}))
    orch = {
        "effects": [
            {
                "type": "reflector",
                "name": "planner",
                "effects": [{"type": "prompt", "name": "propose_steps", "template": "Plan."}],
            }
        ]
    }
    runtime_config: dict[str, Any] = {}
    install_allowlists(runtime_config, enabled_adapters=None, enabled_tools=["json"])

    with pytest.raises(Exception, match="tool 'uuid' not in enabled_tools allowlist"):
        DynamicRuntime(
            compile_orchestration(orch=orch),
            adapter=adapter,
            model="m",
            runtime_config=runtime_config,
        ).execute(store=Store({}))


# ---------- run: adapters resolved after the document check ----------

PROMPT_DOC = "effects:\n  - type: prompt\n    name: greet\n    template: hello\n"


@pytest.fixture()
def no_adapter_builds(monkeypatch: pytest.MonkeyPatch) -> list[str]:
    built: list[str] = []

    def _build(**kwargs: Any) -> Any:
        built.append(kwargs["adapter_name"])
        raise AssertionError("a denied adapter must not be built")

    monkeypatch.setattr(runtime_shim, "build_adapter", _build)
    return built


def test_run_rejects_adapter_override(tmp_path: Path, no_adapter_builds: list[str]) -> None:
    """`cof run --adapter openai` under `enabled_adapters: [ollama]`."""
    doc = _write(tmp_path / "doc.yml", PROMPT_DOC)
    config = CircuitryConfig(
        default_adapter="ollama", default_model="m", enabled_adapters=["ollama"]
    )

    result = _run(doc, config, adapter=None, adapter_override="openai")

    assert result.ok is False
    assert result.error == (
        "adapter 'openai' not in enabled_adapters allowlist (enabled: ['ollama'])"
    )
    assert no_adapter_builds == []


def test_run_rejects_config_default_adapter(
    tmp_path: Path, no_adapter_builds: list[str]
) -> None:
    doc = _write(tmp_path / "doc.yml", PROMPT_DOC)
    config = CircuitryConfig(
        default_adapter="openai", default_model="m", enabled_adapters=["ollama"]
    )

    result = _run(doc, config, adapter=None)

    assert result.ok is False
    assert "adapter 'openai' not in enabled_adapters allowlist" in (result.error or "")
    assert no_adapter_builds == []


def test_run_rejects_profile_provider_override(tmp_path: Path) -> None:
    doc = _write(tmp_path / "doc.yml", PROMPT_DOC)
    _write(
        tmp_path / "profiles" / "cloud.yml",
        "effects:\n  greet:\n    provider: openai\n",
    )
    adapter = RecordingAdapter(name="ollama")

    result = _run(doc, ADAPTERS_OLLAMA_ONLY, adapter=adapter, profile_name="cloud")

    assert result.ok is False
    assert result.error == (
        "Allowlist enforcement failed: profile override for 'greet': adapter "
        "'openai' not in enabled_adapters allowlist (enabled: ['ollama'])"
    )
    assert adapter.calls == []


def test_run_accepts_an_allowed_adapter_override(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`--adapter` naming a listed adapter still overrides a denied default."""
    adapter = RecordingAdapter(name="ollama")
    monkeypatch.setattr(runtime_shim, "build_adapter", lambda **_: adapter)
    doc = _write(tmp_path / "doc.yml", PROMPT_DOC)
    config = CircuitryConfig(
        default_adapter="openai", default_model="m", enabled_adapters=["ollama"]
    )

    result = _run(doc, config, adapter=None, adapter_override="ollama")

    assert result.ok is True, result.error
    assert adapter.calls == [("m", "hello")]


# ---------- --service-profile ----------


def test_service_profile_keeps_allowlists_and_environment() -> None:
    cfg = CircuitryConfig(
        default_adapter="ollama",
        enabled_adapters=["ollama"],
        enabled_plugins=[],
        enabled_tools=["json"],
        environment="prod",
    )
    profile = ServiceProfile(
        name="fetched",
        default_adapter="openai",
        default_model="gpt",
        plugins=["x"],
        runtime_overrides={"k": 1},
    )

    effective = apply_service_profile(cfg=cfg, profile=profile)

    assert effective.enabled_adapters == ["ollama"]
    assert effective.enabled_plugins == []
    assert effective.enabled_tools == ["json"]
    assert effective.environment == "prod"
    assert effective.default_adapter == "openai"
    assert effective.runtime == {"k": 1}


def test_recorded_state_names_the_run_allowlists(tmp_path: Path) -> None:
    parent = _inline_parent(
        tmp_path,
        "effects:\n  - type: tool\n    name: t\n    provider: json\n"
        "    params: {op: parse, input: '{}'}\n",
    )

    result = _run(parent, TOOLS_JSON_ONLY)

    assert result.ok is True, result.error
    recorded = result.state["runtime"]["effective_settings"]["runtime"]
    assert json.loads(json.dumps(recorded["_allowlists"])) == {
        "adapters": None,
        "tools": ["json"],
    }
