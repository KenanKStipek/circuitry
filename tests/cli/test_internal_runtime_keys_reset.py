"""A trusted (path-run) document's own `runtime:` block, a user/project
config's `runtime:` block, or `-e`/`--state` must never be able to inject the
internal bookkeeping keys `run()` installs into the shared `runtime_config`
for its own use — `_use_call_stack` (the `use` cycle guard),
`_decomposition_depth` (decomposition's recursion ceiling),
`_orchestration_dir` (where a relative `use: path:` resolves),
`_decomposition_planner_path` (which planner document decomposition loads)
and `_library_pins` (the recorded `use: ref:` pins). All of them start this
run's `runtime_config` straight from the merged `runtime:` block
(`cli.effective_settings.resolve_effective_settings`), so a value under any
of these names could fake or hide a `use` cycle, make decomposition skip (or
over-run) its own depth ceiling, move where a relative `use` resolves, point
decomposition at an arbitrary planner, or stop a real pin from being
recorded — before `run()`'s own one-step strip of every `_`-prefixed key
(#778) ever lets it reach any of them.
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


def test_a_documents_own_runtime_block_cannot_move_where_a_relative_use_path_resolves(
    tmp_path: Path,
) -> None:
    """A relative `use: path:` resolves against the orchestration's own
    directory (`core.use.UseRuntime`, reading `_orchestration_dir` off the
    shared `runtime_config`), never a decoy directory the document's own
    `runtime:` block names under that same key.
    """
    decoy = tmp_path / "decoy"
    decoy.mkdir()
    _write_yaml(
        tmp_path / "child.yml",
        {"effects": [{"type": "prompt", "name": "step", "template": "x"}]},
    )
    orch = _write_yaml(
        tmp_path / "root.yml",
        {
            "runtime": {"_orchestration_dir": str(decoy)},
            "effects": [
                {"type": "use", "name": "call_child", "path": "./child.yml"}
            ],
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
    assert not (decoy / "child.yml").exists()
    # `effective_settings.runtime` is the same shared dict shown back to
    # the caller -- it reflects the real resolved directory, never the
    # document's own decoy (#778's own "shows none of them").
    effective_dir = result.state["runtime"]["effective_settings"]["runtime"][
        "_orchestration_dir"
    ]
    assert effective_dir != str(decoy)
    assert Path(effective_dir) == orch.resolve().parent


def test_a_documents_own_runtime_block_cannot_point_decomposition_at_another_planner(
    tmp_path: Path,
) -> None:
    """A document's own `runtime:` block pointing `_decomposition_planner_path`
    at an arbitrary file (`core.decompose._planner_path`'s own override) must
    be ignored — decomposition always loads the bundled planner, never a path
    the document itself supplied.
    """
    bogus_planner = _write_yaml(
        tmp_path / "bogus-planner.yml",
        {
            "effects": [
                {"type": "prompt", "name": "propose_steps", "template": "BOGUS PLANNER RAN"}
            ]
        },
    )
    orch = _write_yaml(
        tmp_path / "root.yml",
        {
            "runtime": {
                "_decomposition_planner_path": str(bogus_planner),
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
    assert not any("BOGUS PLANNER RAN" in prompt for _model, prompt in adapter.calls)
    assert any("=== THIS TASK ===" in prompt for _model, prompt in adapter.calls), [
        prompt for _model, prompt in adapter.calls
    ]


def test_a_documents_own_runtime_block_cannot_fake_or_suppress_a_library_pin(
    tmp_path: Path,
) -> None:
    """A document's own `runtime:` block pre-seeding `_library_pins` with a
    fake entry must not survive into `runtime.library_refs` -- and, since
    `core.library_ref.record_pin` dedupes by identity, must not stop the
    real `use: ref:` pin from being recorded either.
    """
    folder = tmp_path / "lib"
    _write_yaml(
        folder / "helper.yml",
        {"effects": [{"type": "prompt", "name": "step", "template": "x"}]},
    )
    fake_pin = {
        "ref": "not-real",
        "source": "local",
        "path": "/nowhere",
        "sha": None,
        "cache_path": None,
        "resolved_at": "1970-01-01T00:00:00+00:00",
    }
    orch = _write_yaml(
        tmp_path / "root.yml",
        {
            "runtime": {"_library_pins": [fake_pin]},
            "effects": [{"type": "use", "name": "outer", "ref": "helper"}],
        },
    )
    cfg = CircuitryConfig(
        runtime={"library": {"sources": [{"type": "folder", "name": "local", "path": str(folder)}]}}
    )

    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=cfg,
            trust_document=True,
            skip_preflight=True,
            adapter=_StubAdapter(),
        )
    )

    assert result.ok is True, result.error
    pins = result.state["runtime"]["library_refs"]
    assert fake_pin not in pins
    assert any(p.get("ref") == "helper" and p.get("source") == "local" for p in pins), pins
