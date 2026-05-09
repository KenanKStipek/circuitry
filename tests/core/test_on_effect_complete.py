"""Tests for Story 2 — per-effect ``on_effect_complete`` lifecycle hook."""

from __future__ import annotations

from pathlib import Path

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run


def _write(tmp_path: Path, name: str, content: str) -> Path:
    p = tmp_path / name
    p.write_text(content, encoding="utf-8")
    return p


def _effect_paths(state: dict) -> list[str]:
    return list(state["runtime"]["plugin_marker"].get("effect_paths", []))


def test_hook_fires_for_each_prompt_effect(tmp_path: Path) -> None:
    """AC 2.1: hook fires N times with correct effect_path values."""
    orch = _write(
        tmp_path,
        "orch.yml",
        """
adapter: openai
model: gpt-4o-mini
effects:
  - type: prompt
    name: greet
    template: "hi"
  - type: prompt
    name: farewell
    template: "bye"
""".strip()
        + "\n",
    )
    cfg = CircuitryConfig(plugins=["plugin_fixtures:make_recording_plugin"])
    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=False,
            initial_state={},
            config=cfg,
        )
    )
    assert result.ok is True
    paths = _effect_paths(result.state)
    # Expect at minimum: each prompt fires + the root dynamic fires.
    assert "prime.greet" in paths
    assert "prime.farewell" in paths
    assert "prime" in paths


def test_hook_fires_with_loop_iteration_paths(tmp_path: Path) -> None:
    """AC 2.3: loop iteration body effect_paths include iter_N segment."""
    orch = _write(
        tmp_path,
        "orch.yml",
        """
adapter: openai
model: gpt-4o-mini
effects:
  - type: loop
    name: my_loop
    each:
      in: prime.items.value
      as: it
    body:
      - type: prompt
        name: handle
        template: "{{it}}"
""".strip()
        + "\n",
    )
    cfg = CircuitryConfig(plugins=["plugin_fixtures:make_recording_plugin"])
    # Pre-seed state with an array since dry_run prompts return None.
    initial = {"prime": {"items": {"value": ["a", "b", "c"]}}}
    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=False,
            initial_state=initial,
            config=cfg,
        )
    )
    assert result.ok is True, result.error
    paths = _effect_paths(result.state)
    # Each iteration's body effect should fire with iter_N in its path.
    iter_paths = [p for p in paths if "iter_" in p and p.endswith(".handle")]
    assert len(iter_paths) >= 3
    assert all(p.startswith("prime.my_loop.iter_") for p in iter_paths)


def test_failing_hook_does_not_abort_run(tmp_path: Path) -> None:
    """AC 2.2: a plugin's on_effect_complete raising must NOT abort the run."""
    orch = _write(
        tmp_path,
        "orch.yml",
        """
adapter: openai
model: gpt-4o-mini
effects:
  - type: prompt
    name: greet
    template: "hi"
""".strip()
        + "\n",
    )
    cfg = CircuitryConfig(plugins=["plugin_fixtures:make_failing_plugin"])
    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=False,
            initial_state={},
            config=cfg,
        )
    )
    # FailingPlugin.on_run_start also explodes — but per existing semantics
    # plugin failures are non-fatal warnings, so the run continues to
    # success despite the hook(s) raising.
    assert result.ok is True
    failed_events = [
        e for e in result.state["runtime"]["plugins"]["events"] if not e["ok"]
    ]
    # At least the on_run_start failure shows up; on_effect_complete may
    # have fired and recorded a failure event too.
    assert any(e["hook"] == "on_run_start" for e in failed_events)


def test_hook_skipped_for_plugins_without_method(tmp_path: Path) -> None:
    """A plugin without on_effect_complete must not break the run.

    Story 2 spec: "gracefully handle plugins missing the method via hasattr
    guard"."""
    import sys
    import types

    # Inject a synthetic plugin module that implements on_run_* but not
    # on_effect_complete. This mirrors plugins written before Story 2.
    mod = types.ModuleType("legacy_plugin_fixture")

    class LegacyPlugin:
        name = "legacy"

        def on_run_start(self, *, state, context):
            pass

        def on_run_success(self, *, state, context):
            pass

        def on_run_failure(self, *, state, context, error):
            pass

    mod.plugin = LegacyPlugin()
    sys.modules["legacy_plugin_fixture"] = mod

    try:
        orch = _write(
            tmp_path,
            "orch.yml",
            """
adapter: openai
model: gpt-4o-mini
effects:
  - type: prompt
    name: greet
    template: "hi"
""".strip()
            + "\n",
        )
        cfg = CircuitryConfig(plugins=["legacy_plugin_fixture"])
        result = run(
            RunRequest(
                orchestration_path=orch,
                state_path=None,
                out_path=None,
                dry_run=True,
                validate_only=False,
                initial_state={},
                config=cfg,
            )
        )
        assert result.ok is True
    finally:
        sys.modules.pop("legacy_plugin_fixture", None)
