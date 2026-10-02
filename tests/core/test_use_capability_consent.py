"""The run-time half of capability consent (#275): `UseRuntime` re-checks a
`ref:` child as it loads (never interactively — a `ref:` nested inside a
generated/inline document is invisible to the static walk in
`circuitry.cli.document_consent.enforce_consent`, which skips `inline:`
content entirely since it isn't known until it renders), and a
`path:`/`inline:` child is held to whatever ceiling the run already carries.
"""

from __future__ import annotations

from pathlib import Path

import pytest
import yaml

from circuitry.capability_gate import CapabilityConsentError, install_capability_ceiling
from circuitry.cli.config import trust_store_path
from circuitry.cli.document_consent import (
    DocumentConsentError,
    document_digest,
    record_consent,
)
from circuitry.core.store import Store
from circuitry.core.use import UseDefinition, UseRuntime


def _write_yaml(path: Path, orch: dict) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(yaml.safe_dump(orch), encoding="utf-8")
    return path


def _folder_runtime(folder: Path) -> dict:
    return {"library": {"sources": [{"type": "folder", "name": "local", "path": str(folder)}]}}


# ── _check_capability_consent (unit) ────────────────────────────────────────


def test_ref_child_refuses_without_consent() -> None:
    defn = UseDefinition(name="u", ref="helper")
    runtime = UseRuntime(defn, adapter=None, model=None)
    child_orch = {"effects": [{"type": "tool", "name": "t", "provider": "shell"}]}

    with pytest.raises(DocumentConsentError, match="shell"):
        runtime._check_capability_consent(child_orch, "helper.yml", "digest-1")


def test_ref_child_passes_once_its_own_digest_is_consented() -> None:
    record_consent("digest-2", frozenset({"shell"}), store_path=trust_store_path())
    defn = UseDefinition(name="u", ref="helper")
    runtime = UseRuntime(defn, adapter=None, model=None)
    child_orch = {"effects": [{"type": "tool", "name": "t", "provider": "shell"}]}

    ceiling = runtime._check_capability_consent(child_orch, "helper.yml", "digest-2")

    assert ceiling == {"shell"}


def test_ref_child_honors_the_installed_allow_capabilities_override() -> None:
    defn = UseDefinition(name="u", ref="helper")
    runtime_config = {"_capability_allow": ["shell"]}
    runtime = UseRuntime(defn, adapter=None, model=None, runtime_config=runtime_config)
    child_orch = {"effects": [{"type": "tool", "name": "t", "provider": "shell"}]}

    ceiling = runtime._check_capability_consent(child_orch, "helper.yml", "digest-3")

    assert ceiling == {"shell"}


def test_path_child_with_no_ceiling_is_unrestricted() -> None:
    defn = UseDefinition(name="u", path="child.yml")
    runtime = UseRuntime(defn, adapter=None, model=None)
    child_orch = {"effects": [{"type": "tool", "name": "t", "provider": "shell"}]}

    assert runtime._check_capability_consent(child_orch, "child.yml", "digest-4") is None


def test_path_child_refuses_beyond_an_installed_ceiling() -> None:
    defn = UseDefinition(name="u", path="child.yml")
    runtime_config: dict = {}
    install_capability_ceiling(runtime_config, frozenset({"network"}))
    runtime = UseRuntime(defn, adapter=None, model=None, runtime_config=runtime_config)
    child_orch = {"effects": [{"type": "tool", "name": "t", "provider": "shell"}]}

    with pytest.raises(CapabilityConsentError, match="shell"):
        runtime._check_capability_consent(child_orch, "child.yml", "digest-5")


def test_path_child_within_the_ceiling_passes() -> None:
    defn = UseDefinition(name="u", path="child.yml")
    runtime_config: dict = {}
    install_capability_ceiling(runtime_config, frozenset({"shell", "network"}))
    runtime = UseRuntime(defn, adapter=None, model=None, runtime_config=runtime_config)
    child_orch = {"effects": [{"type": "tool", "name": "t", "provider": "shell"}]}

    assert runtime._check_capability_consent(child_orch, "child.yml", "digest-6") is None


# ── a templated ref:, end to end ─────────────────────────────────────────────


def test_a_ref_reached_only_through_an_inline_child_still_needs_consent(
    tmp_path: Path,
) -> None:
    """`ref_child_requirements`'s static walk skips `inline:` content (its
    text isn't known until it renders), so a `use: ref:` nested inside a
    generated/inline document (reflector, decompose) is invisible to the
    up-front gate. The run-time check closes that gap: every `use` effect,
    an inline child's own nested ones included, goes through the same
    `_check_capability_consent` as it loads.
    """
    folder = tmp_path / "lib"
    _write_yaml(
        folder / "helper.yml",
        {"effects": [{"type": "tool", "name": "t", "provider": "shell"}]},
    )
    inline_yaml = yaml.safe_dump(
        {"effects": [{"type": "use", "name": "inner", "ref": "helper"}]}
    )
    outer = UseDefinition(name="outer", inline=inline_yaml, validate=False)
    runtime = UseRuntime(
        outer, adapter=None, model=None, runtime_config=_folder_runtime(folder)
    )
    store = Store(state={})

    with pytest.raises(RuntimeError, match=r"needs capabilities.*shell"):
        runtime.execute(store=store, ctx=store.state)


def test_a_ref_reached_only_through_an_inline_child_runs_once_pre_consented(
    tmp_path: Path,
) -> None:
    folder = tmp_path / "lib"
    helper = _write_yaml(
        folder / "helper.yml",
        {"effects": [{"type": "tool", "name": "t", "provider": "uuid"}]},
    )
    record_consent(
        document_digest(helper.read_bytes()), frozenset(), store_path=trust_store_path()
    )
    inline_yaml = yaml.safe_dump(
        {"effects": [{"type": "use", "name": "inner", "ref": "helper"}]}
    )
    outer = UseDefinition(name="outer", inline=inline_yaml, validate=False)
    runtime = UseRuntime(
        outer, adapter=None, model=None, runtime_config=_folder_runtime(folder)
    )
    store = Store(state={})

    runtime.execute(store=store, ctx=store.state)

    assert store.state["outer"]["meta"]["error"] is None
