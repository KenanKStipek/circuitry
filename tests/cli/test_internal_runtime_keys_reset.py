"""A trusted (path-run) document's own `runtime:` block, a user/project
config's `runtime:` block, or `-e`/`--state` must never be able to inject the
internal bookkeeping keys `run()` installs into the shared `runtime_config`
for its own use — `_use_call_stack` (the `use` cycle guard) and
`_decomposition_depth` (decomposition's recursion ceiling). Both start this
run's `runtime_config` straight from the merged `runtime:` block
(`cli.effective_settings.resolve_effective_settings`), so a value under
either name could fake or hide a `use` cycle, or make decomposition skip (or
over-run) its own depth ceiling, before `run()` ever gets a chance to install
its own (#778).
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run


@dataclass(frozen=True)
class _StubAdapter:
    name: str = "stub"
    calls: list = field(default_factory=list)

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        self.calls.append((model, prompt))
        return GenerateResult(text="ok", raw={})


def _write_yaml(path: Path, orch: dict) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(yaml.safe_dump(orch), encoding="utf-8")
    return path


def test_a_documents_own_runtime_block_cannot_fake_a_use_cycle(tmp_path: Path) -> None:
    """A non-cyclic `use: path:` must succeed even when the document's own
    `runtime:` block pre-seeds `_use_call_stack` with the exact identity
    (its resolved path) the `use` is about to call — otherwise a document
    could wall off any child it names just by quoting its own path back at
    the cycle guard.
    """
    child = _write_yaml(
        tmp_path / "child.yml",
        {"effects": [{"type": "prompt", "name": "step", "template": "x"}]},
    )
    orch = _write_yaml(
        tmp_path / "root.yml",
        {
            "runtime": {"_use_call_stack": [str(child.resolve())]},
            "effects": [{"type": "use", "name": "call_child", "path": str(child)}],
        },
    )

    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(),
            trust_document=True,
            skip_preflight=True,
            adapter=_StubAdapter(),
        )
    )

    assert result.ok is True, result.error
    assert result.state["prime"]["call_child"]["step"]["value"] == "ok"


def test_a_documents_own_runtime_block_cannot_skip_decomposition(tmp_path: Path) -> None:
    """A document's own `runtime:` block pre-seeding `_decomposition_depth`
    at (or past) `max_depth` must not make decomposition skip its planner —
    a run always starts at depth 0, whatever the merged runtime carried.
    """
    orch = _write_yaml(
        tmp_path / "root.yml",
        {
            "runtime": {
                "_decomposition_depth": 5,
                "complexity": {
                    "scoring": {"enabled": True},
                    "decomposition": {
                        "enabled": True,
                        "threshold": 0,
                        "max_depth": 1,
                        "max_chunks": 8,
                        "on_failure": "route_up",
                    },
                },
            },
            "effects": [
                {
                    "type": "prompt",
                    "name": "task",
                    "template": "TASK analyze {{topic}} and cross-reference it against {{source}}.",
                }
            ],
        },
    )

    adapter = _StubAdapter()
    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(),
            trust_document=True,
            skip_preflight=True,
            adapter=adapter,
            initial_state={"topic": "volcanoes", "source": "the archive"},
        )
    )

    assert result.ok is True, result.error
    # The bundled planner's own task-section marker, present in its prompt
    # verbatim (it is not interpolated) — seeing it at all means the planner
    # ran, i.e. decomposition was attempted rather than skipped as though
    # the depth ceiling were already reached.
    assert any("=== THIS TASK ===" in prompt for _model, prompt in adapter.calls), [
        prompt for _model, prompt in adapter.calls
    ]
