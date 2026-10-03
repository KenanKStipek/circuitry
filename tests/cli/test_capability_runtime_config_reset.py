"""A trusted (path-run) document's own `runtime:` block must never be able
to inject or widen the internal capability-consent bookkeeping keys
(`_capability_ceiling`, `_capability_allow`) that a run installs into the
shared `runtime_config` — those decide what a `use: ref:` child or a
generated plan may do, and are meant to come only from this run's own
consent resolution and `--allow-capabilities`, never from document text,
however trusted the document itself is (#275).
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
    """A `use` effect always builds an adapter, even for a tool-only child."""

    name: str = "stub"
    calls: list = field(default_factory=list)

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        self.calls.append((model, prompt))
        return GenerateResult(text=f"{model}:{prompt}", raw={})


def _write_yaml(path: Path, orch: dict) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(yaml.safe_dump(orch), encoding="utf-8")
    return path


def test_a_documents_own_runtime_block_cannot_inject_capability_allow(
    tmp_path: Path,
) -> None:
    """The static gate (`ref_child_requirements`) only ever sees `ref:`
    reachable through `path:`/plain text — never through `inline:` content,
    whose text isn't known until it renders. A `ref:` nested inside an
    inline child therefore only gets checked at run time
    (`UseRuntime._check_capability_consent`), which is exactly where a
    document-injected `_capability_allow` could widen what passes without
    real consent — so this has to go the inline route to actually exercise
    that check, not the static one (which never reads runtime_config at
    all, only `RunRequest.allow_capabilities`).
    """
    folder = tmp_path / "lib"
    helper = _write_yaml(
        folder / "helper.yml",
        {
            "effects": [
                {
                    "type": "tool",
                    "name": "t",
                    "provider": "shell",
                    "params": {"command": "echo", "args": ["hi"]},
                }
            ]
        },
    )
    inline_yaml = yaml.safe_dump(
        {"effects": [{"type": "use", "name": "inner", "ref": "helper"}]}
    )
    orch = _write_yaml(
        tmp_path / "root.yml",
        {
            # A trusted document's own runtime: block is applied — but not
            # into the keys this module reserves for its own bookkeeping.
            "runtime": {"_capability_allow": ["shell", "network", "fs-write", "python_eval"]},
            "effects": [{"type": "use", "name": "outer", "inline": inline_yaml}],
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
            allow_capabilities=None,
            capability_prompt=None,
        )
    )

    assert result.ok is False
    assert "needs capabilities nobody has consented to: shell" in (result.error or "")
    assert helper.exists()  # sanity: the child is the one that needed consent


def test_a_documents_own_runtime_block_cannot_inject_a_capability_ceiling(
    tmp_path: Path,
) -> None:
    """A plain path run with no `use: ref:` installs no ceiling at all — its
    own `use: path:` children stay unrestricted (#284). A document that
    pre-set `runtime._capability_ceiling` to something narrower must not get
    to override that and wrongly restrict its own path child instead.
    """
    child = _write_yaml(
        tmp_path / "child.yml",
        {
            "effects": [
                {
                    "type": "tool",
                    "name": "t",
                    "provider": "shell",
                    "params": {"command": "echo", "args": ["hi"]},
                }
            ]
        },
    )
    orch = _write_yaml(
        tmp_path / "root.yml",
        {
            "runtime": {"_capability_ceiling": ["network"]},
            "effects": [{"type": "use", "name": "u", "path": str(child)}],
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
            allow_capabilities=None,
            capability_prompt=None,
        )
    )

    assert result.ok is True, result.error
