"""The headless driving example must stay runnable and stay true.

`docs/wizard.md` and `scripts/wizard-chat` both hard-code state paths into the
wizard's output namespace. Those paths are the one part of the contract a reader
copies verbatim, so they are checked against the orchestration's own interface
rather than trusted.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import re
from pathlib import Path
from types import ModuleType
from typing import Any

import pytest
import yaml

DOC_PATH = Path("docs/wizard.md")
SCRIPT_PATH = Path("scripts/wizard-chat")
WIZARD_PATH = Path("src/circuitry/curation/agents/wizard.yml")


@pytest.fixture(scope="module")
def declared_paths() -> dict[str, str]:
    interface = yaml.safe_load(WIZARD_PATH.read_text(encoding="utf-8"))["interface"]
    return {name: spec["path"] for name, spec in interface["outputs"].items()}


@pytest.fixture(scope="module")
def driver() -> ModuleType:
    """Import scripts/wizard-chat, which has no .py extension."""
    spec = importlib.util.spec_from_loader(
        "wizard_chat",
        importlib.machinery.SourceFileLoader("wizard_chat", str(SCRIPT_PATH)),
    )
    assert spec is not None
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def _python_blocks(markdown: str) -> list[str]:
    return re.findall(r"```python\n(.*?)```", markdown, flags=re.DOTALL)


# ── The doc example ──────────────────────────────────────────────────────────


def test_doc_example_is_valid_python() -> None:
    (example,) = _python_blocks(DOC_PATH.read_text(encoding="utf-8"))
    compile(example, "docs/wizard.md", "exec")


def test_doc_example_reads_the_paths_the_wizard_declares(
    declared_paths: dict[str, str],
) -> None:
    (example,) = _python_blocks(DOC_PATH.read_text(encoding="utf-8"))
    for output in ("say", "yaml", "done"):
        assert declared_paths[output] in example, (
            f"docs/wizard.md does not read the declared path for {output!r}"
        )


def test_doc_contract_table_matches_the_interface(
    declared_paths: dict[str, str],
) -> None:
    doc = DOC_PATH.read_text(encoding="utf-8")
    for output, path in declared_paths.items():
        assert f"`{path}`" in doc, f"contract table is missing {output!r}"


def test_doc_and_script_agree_on_the_orchestration_path() -> None:
    doc = DOC_PATH.read_text(encoding="utf-8")
    assert str(WIZARD_PATH) in doc
    assert WIZARD_PATH.exists()


# ── The driver script ────────────────────────────────────────────────────────


def test_script_reads_the_paths_the_wizard_declares(
    driver: ModuleType, declared_paths: dict[str, str]
) -> None:
    source = SCRIPT_PATH.read_text(encoding="utf-8")
    for output in ("say", "yaml", "done", "errors"):
        assert declared_paths[output] in source


def test_script_wizard_path_resolves(driver: ModuleType) -> None:
    assert driver.WIZARD.exists()


def test_dig_walks_and_misses_cleanly(driver: ModuleType) -> None:
    state = {"prime": {"turn": {"decide": {"done": {"value": True}}}}}
    assert driver._dig(state, "prime.turn.decide.done.value") is True
    assert driver._dig(state, "prime.turn.decide.check.value.ok") is None
    assert driver._dig(state, "nope") is None


def test_run_turn_passes_the_host_state_through(
    driver: ModuleType, monkeypatch: pytest.MonkeyPatch
) -> None:
    seen: dict[str, Any] = {}

    class _Result:
        state = {
            "prime": {
                "turn": {
                    "decide": {
                        "respond": {"value": {"say": "hi"}},
                        "check": {"value": {"yaml": "effects: []", "errors": []}},
                        "done": {"value": True},
                    }
                }
            }
        }

    def fake_run(**kwargs: Any) -> Any:
        seen.update(kwargs)
        return _Result()

    monkeypatch.setattr(driver, "run_orchestration", fake_run)

    say, yaml_text, done, errors = driver.run_turn(
        goal="do a thing",
        conversation=[{"role": "user", "content": "hello"}],
        draft="effects: []",
        config=None,
    )

    assert seen["state"] == {
        "goal": "do a thing",
        "conversation": [{"role": "user", "content": "hello"}],
        "draft": "effects: []",
    }
    assert (say, yaml_text, done, errors) == ("hi", "effects: []", True, [])


def test_scripted_conversation_accumulates_state_and_stops_on_done(
    driver: ModuleType, monkeypatch: pytest.MonkeyPatch, tmp_path: Path, capsys: Any
) -> None:
    """The outer loop is the host's whole job — this is it, exercised."""
    turns = iter(
        [
            ("What should it output?", None, False),
            ("Here is a draft.", "effects: []\n", False),
            ("Done.", "effects: []\n", True),
        ]
    )
    calls: list[dict[str, Any]] = []

    def fake_run_turn(*, goal: str, conversation: list, draft: str, **_: Any):
        calls.append({"conversation": list(conversation), "draft": draft})
        say, yaml_text, done = next(turns)
        return say, yaml_text, done, []

    monkeypatch.setattr(driver, "run_turn", fake_run_turn)
    monkeypatch.setattr(driver, "load_config", lambda *a, **k: None)
    monkeypatch.setattr(driver, "find_config_path", lambda *a, **k: None)

    replies = tmp_path / "answers.txt"
    replies.write_text("A summary.\nShip it.\n", encoding="utf-8")
    out = tmp_path / "built.yml"

    monkeypatch.setattr(
        "sys.argv",
        [
            "wizard-chat",
            "--goal",
            "Summarize an article",
            "--reply",
            str(replies),
            "--out",
            str(out),
        ],
    )
    assert driver.main() == 0

    # Turn 1 starts empty; each later turn carries the accumulated transcript
    # and the last validated draft.
    assert calls[0]["conversation"] == []
    assert calls[0]["draft"] == ""
    assert calls[1]["conversation"] == [
        {"role": "wizard", "content": "What should it output?"},
        {"role": "user", "content": "A summary."},
    ]
    assert calls[2]["draft"] == "effects: []\n"
    assert len(calls) == 3  # stopped on done, did not consume a fourth turn

    assert out.read_text(encoding="utf-8") == "effects: []\n"
    assert "wizard is done" in capsys.readouterr().out


def test_manifest_documents_the_same_contract(declared_paths: dict[str, str]) -> None:
    manifest = json.loads(
        Path("src/circuitry/curation/manifest.json").read_text(encoding="utf-8")
    )
    entry = next(e for e in manifest["entries"] if e["name"] == "agents/wizard")
    assert {k: v["path"] for k, v in entry["outputs"].items()} == declared_paths
