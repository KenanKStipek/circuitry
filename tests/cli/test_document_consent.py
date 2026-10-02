"""Tests for capability consent (#275): the static requirement walk, the
consent store (shared with `cof trust`'s project-config store), and the
prompt/raise/allow-capabilities resolution logic.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.cli.config import trust_store_path
from circuitry.cli.config_trust import record_trust
from circuitry.cli.document_consent import (
    DocumentConsentError,
    consented_capabilities,
    document_digest,
    enforce_consent,
    record_consent,
    ref_child_requirements,
    refusal_message,
    required_capabilities,
    resolve_consent,
)


def _write_yaml(path: Path, orch: dict[str, Any]) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(yaml.safe_dump(orch), encoding="utf-8")
    return path


def _tool(provider: str, name: str = "t") -> dict[str, Any]:
    return {"type": "tool", "name": name, "provider": provider}


# ── required_capabilities ────────────────────────────────────────────────────


def test_required_capabilities_is_empty_for_a_benign_document() -> None:
    orch = {"effects": [_tool("math")]}
    assert required_capabilities(orch, root_path=None, runtime=None) == frozenset()


def test_required_capabilities_unions_every_tool_effect() -> None:
    orch = {"effects": [_tool("shell"), _tool("web_fetch", "w")]}
    assert required_capabilities(orch, root_path=None, runtime=None) == {"shell", "network"}


def test_required_capabilities_follows_a_path_child(tmp_path: Path) -> None:
    child = _write_yaml(tmp_path / "child.yml", {"effects": [_tool("shell")]})
    root = _write_yaml(
        tmp_path / "root.yml",
        {"effects": [{"type": "use", "name": "u", "path": str(child)}]},
    )
    orch = yaml.safe_load(root.read_text())
    assert required_capabilities(orch, root_path=root, runtime=None) == {"shell"}


# ── ref_child_requirements ───────────────────────────────────────────────────


def _folder_runtime(folder: Path) -> dict[str, Any]:
    return {"library": {"sources": [{"type": "folder", "name": "local", "path": str(folder)}]}}


def test_ref_child_requirements_finds_a_direct_ref(tmp_path: Path) -> None:
    folder = tmp_path / "lib"
    _write_yaml(folder / "helper.yml", {"effects": [_tool("python_eval")]})
    root = {"effects": [{"type": "use", "name": "u", "ref": "helper"}]}
    runtime = _folder_runtime(folder)

    reqs = ref_child_requirements(root, root_path=tmp_path / "root.yml", runtime=runtime)

    assert len(reqs) == 1
    assert reqs[0].required == {"python_eval"}
    assert reqs[0].path == (folder / "helper.yml").resolve()


def test_ref_child_requirements_walks_through_a_path_edge_to_a_nested_ref(
    tmp_path: Path,
) -> None:
    folder = tmp_path / "lib"
    _write_yaml(folder / "inner.yml", {"effects": [_tool("shell")]})
    child = _write_yaml(
        tmp_path / "child.yml",
        {"effects": [{"type": "use", "name": "u2", "ref": "inner"}]},
    )
    root = {"effects": [{"type": "use", "name": "u1", "path": str(child)}]}
    runtime = _folder_runtime(folder)

    reqs = ref_child_requirements(root, root_path=tmp_path / "root.yml", runtime=runtime)

    assert [r.required for r in reqs] == [{"shell"}]


def test_ref_child_requirements_ignores_inline_children(tmp_path: Path) -> None:
    root = {
        "effects": [
            {"type": "use", "name": "u", "inline": "effects:\n  - {type: tool, name: t, provider: shell}\n"}
        ]
    }
    assert ref_child_requirements(root, root_path=tmp_path / "root.yml", runtime=None) == []


# ── the consent store ─────────────────────────────────────────────────────


def test_a_digest_never_asked_about_reads_as_none() -> None:
    assert consented_capabilities("deadbeef", store_path=trust_store_path()) is None


def test_record_then_read_round_trips() -> None:
    store = trust_store_path()
    record_consent("abc123", frozenset({"shell", "network"}), store_path=store)
    assert consented_capabilities("abc123", store_path=store) == {"shell", "network"}


def test_recording_again_replaces_the_prior_entry() -> None:
    store = trust_store_path()
    record_consent("digest", frozenset({"shell"}), store_path=store)
    record_consent("digest", frozenset({"shell", "network"}), store_path=store)
    assert consented_capabilities("digest", store_path=store) == {"shell", "network"}


def test_document_consent_coexists_with_a_trusted_project_config(tmp_path: Path) -> None:
    """Writing one section never clobbers the other \u2014 same file, `cof trust`."""
    store = trust_store_path()
    config_path = tmp_path / "config.json"
    config_path.write_text("{}", encoding="utf-8")

    record_trust(config_path, b"{}", store_path=store)
    record_consent("digest", frozenset({"shell"}), store_path=store)

    from circuitry.cli.config_trust import read_trust_entries

    assert [e.path for e in read_trust_entries(store)] == [str(config_path.resolve())]
    assert consented_capabilities("digest", store_path=store) == {"shell"}

    # Writing the trust side again still preserves the document side.
    record_trust(config_path, b'{"a": 1}', store_path=store)
    assert consented_capabilities("digest", store_path=store) == {"shell"}


def test_document_digest_is_sha256_of_the_bytes() -> None:
    import hashlib

    assert document_digest(b"hello") == hashlib.sha256(b"hello").hexdigest()


# ── resolve_consent ──────────────────────────────────────────────────────


def test_resolve_consent_passes_silently_when_nothing_is_required() -> None:
    store = trust_store_path()
    result = resolve_consent(
        "doc", "digest", frozenset(), store_path=store, allow_capabilities=None, prompt=None
    )
    assert result == frozenset()


def test_resolve_consent_raises_non_interactively_when_unconsented() -> None:
    store = trust_store_path()
    with pytest.raises(DocumentConsentError, match="shell"):
        resolve_consent(
            "doc", "digest", frozenset({"shell"}),
            store_path=store, allow_capabilities=None, prompt=None,
        )


def test_resolve_consent_is_satisfied_by_allow_capabilities_without_persisting() -> None:
    store = trust_store_path()
    result = resolve_consent(
        "doc", "digest", frozenset({"shell"}),
        store_path=store, allow_capabilities=frozenset({"shell"}), prompt=None,
    )
    assert result == {"shell"}
    assert consented_capabilities("digest", store_path=store) is None


def test_resolve_consent_prompts_and_records_on_yes() -> None:
    store = trust_store_path()
    seen = []

    def prompt(label: str, caps: frozenset[str]) -> bool:
        seen.append((label, caps))
        return True

    result = resolve_consent(
        "doc", "digest", frozenset({"network"}),
        store_path=store, allow_capabilities=None, prompt=prompt,
    )
    assert result == {"network"}
    assert seen == [("doc", frozenset({"network"}))]
    assert consented_capabilities("digest", store_path=store) == {"network"}


def test_resolve_consent_raises_on_a_declined_prompt() -> None:
    store = trust_store_path()
    with pytest.raises(DocumentConsentError, match="Not consented"):
        resolve_consent(
            "doc", "digest", frozenset({"network"}),
            store_path=store, allow_capabilities=None, prompt=lambda *_: False,
        )
    assert consented_capabilities("digest", store_path=store) is None


def test_resolve_consent_does_not_reprompt_once_consented() -> None:
    store = trust_store_path()
    record_consent("digest", frozenset({"shell"}), store_path=store)

    def fail_if_called(*_: object) -> bool:
        raise AssertionError("should not prompt: already consented")

    result = resolve_consent(
        "doc", "digest", frozenset({"shell"}),
        store_path=store, allow_capabilities=None, prompt=fail_if_called,
    )
    assert result == {"shell"}


def test_refusal_message_names_the_cof_trust_and_allow_capabilities_hints() -> None:
    message = refusal_message("my doc.yml", frozenset({"shell", "network"}))
    assert "cof trust" in message
    assert "--allow-capabilities" in message
    assert "network" in message and "shell" in message


# ── enforce_consent ──────────────────────────────────────────────────────


def test_enforce_consent_skips_the_whole_document_when_not_gated(tmp_path: Path) -> None:
    """A path-run or plain library-name document: gate_whole_document=False
    means its own effects are never asked about \u2014 only `ref:` children are.
    """
    orch_path = _write_yaml(tmp_path / "orch.yml", {"effects": [_tool("shell")]})
    ceiling = enforce_consent(
        orch={"effects": [_tool("shell")]},
        orchestration_path=orch_path,
        gate_whole_document=False,
        runtime=None,
        store_path=trust_store_path(),
        allow_capabilities=None,
        prompt=None,
    )
    assert ceiling is None


def test_enforce_consent_gates_the_whole_document_when_fetched(tmp_path: Path) -> None:
    orch_path = _write_yaml(tmp_path / "orch.yml", {"effects": [_tool("shell")]})
    with pytest.raises(DocumentConsentError):
        enforce_consent(
            orch={"effects": [_tool("shell")]},
            orchestration_path=orch_path,
            gate_whole_document=True,
            runtime=None,
            store_path=trust_store_path(),
            allow_capabilities=None,
            prompt=None,
        )


def test_enforce_consent_returns_the_consented_set_as_ceiling(tmp_path: Path) -> None:
    orch_path = _write_yaml(tmp_path / "orch.yml", {"effects": [_tool("shell")]})
    ceiling = enforce_consent(
        orch={"effects": [_tool("shell")]},
        orchestration_path=orch_path,
        gate_whole_document=True,
        runtime=None,
        store_path=trust_store_path(),
        allow_capabilities=frozenset({"shell"}),
        prompt=None,
    )
    assert ceiling == {"shell"}


def test_enforce_consent_also_gates_a_ref_child_of_a_trusted_document(
    tmp_path: Path,
) -> None:
    """The owner-check scenario: a path-run (gate_whole_document=False)
    document that pulls in a `use: ref:` child still needs that child
    consented \u2014 only the top-level document itself is exempt.
    """
    folder = tmp_path / "lib"
    _write_yaml(folder / "helper.yml", {"effects": [_tool("web_fetch")]})
    orch_path = _write_yaml(
        tmp_path / "root.yml",
        {"effects": [{"type": "use", "name": "u", "ref": "helper"}]},
    )
    with pytest.raises(DocumentConsentError, match="helper"):
        enforce_consent(
            orch=yaml.safe_load(orch_path.read_text()),
            orchestration_path=orch_path,
            gate_whole_document=False,
            runtime=_folder_runtime(folder),
            store_path=trust_store_path(),
            allow_capabilities=None,
            prompt=None,
        )


def test_enforce_consent_passes_a_tool_only_document_run_by_path_with_no_ref(
    tmp_path: Path,
) -> None:
    """#275's owner check: a plain tool-only document run by path, no
    `use: ref:`, never asks \u2014 nothing changes for it.
    """
    orch_path = _write_yaml(tmp_path / "orch.yml", {"effects": [_tool("uuid")]})
    ceiling = enforce_consent(
        orch={"effects": [_tool("uuid")]},
        orchestration_path=orch_path,
        gate_whole_document=False,
        runtime=None,
        store_path=trust_store_path(),
        allow_capabilities=None,
        prompt=None,
    )
    assert ceiling is None
