"""Whose trust a `use` child has, for a prompt's `file: {from: ...}` `within` (#578).

A child named by `path:` gets its parent's trust; a child resolved through the
library is always limited. The conformance harness cannot express a library
child, so these run `UseRuntime` against a folder library.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import yaml

from circuitry.adapters.base import GenerateOptions, GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


class RecordingAdapter:
    name = "rec"

    def __init__(self) -> None:
        self.prompts: list[str] = []

    def generate(
        self,
        *,
        model: str,
        prompt: str,
        timeout_seconds: int = 120,
        options: GenerateOptions | None = None,
    ) -> GenerateResult:
        self.prompts.append(prompt)
        return GenerateResult(text="ok", raw={})


CHILD = {
    "effects": [
        {
            "type": "prompt",
            "name": "ask",
            "template": {"file": {"from": "input.prompt_file"}, "within": {"from": "input.work"}},
        }
    ]
}


def _write(path: Path, orch: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(yaml.safe_dump(orch), encoding="utf-8")


def _run_parent(
    tmp_path: Path, use: dict[str, Any], *, parent_trusted: bool, library: Path | None = None
) -> tuple[dict[str, Any], RecordingAdapter]:
    """Run a parent whose one `use` effect has *use*, in *tmp_path*.

    The child's project is its own directory (a `config.json` marks it), and the
    file it reads is in `tmp_path/elsewhere`, outside that project.
    """
    (tmp_path / "elsewhere").mkdir()
    (tmp_path / "elsewhere" / "p.txt").write_text("outside the child", encoding="utf-8")
    inputs = {
        "work": {"from": "input.work"},
        "prompt_file": {"from": "input.prompt_file"},
    }
    parent = {"effects": [{"type": "use", "name": "child", "inputs": inputs, "on_error": "continue", **use}]}
    root = compile_orchestration(
        orch=parent,
        root_name="prime",
        document_dir=tmp_path,
        confinement_root=tmp_path,
        trusted=parent_trusted,
    )
    runtime_config: dict[str, Any] = {"_orchestration_dir": str(tmp_path)}
    if library is not None:
        runtime_config["library"] = {
            "sources": [{"type": "folder", "name": "local", "path": str(library)}]
        }
    adapter = RecordingAdapter()
    store = Store({"input": {"work": str(tmp_path / "elsewhere"), "prompt_file": "p.txt"}})
    DynamicRuntime(
        root, adapter=adapter, model="m", runtime_config=runtime_config
    ).execute(store=store)
    return store.get("prime.child"), adapter


def _path_child(tmp_path: Path) -> dict[str, Any]:
    _write(tmp_path / "child" / "child.yml", CHILD)
    (tmp_path / "child" / "config.json").write_text("{}", encoding="utf-8")
    return {"path": "child/child.yml"}


def test_a_path_child_of_a_trusted_parent_reads_a_file_outside_its_own_project(
    tmp_path: Path,
) -> None:
    node, adapter = _run_parent(tmp_path, _path_child(tmp_path), parent_trusted=True)
    assert node["meta"]["error"] is None
    assert adapter.prompts == ["outside the child"]


def test_a_path_child_of_a_limited_parent_is_limited(tmp_path: Path) -> None:
    node, adapter = _run_parent(tmp_path, _path_child(tmp_path), parent_trusted=False)
    assert adapter.prompts == []
    assert "resolves outside the project" in str(node["meta"]["error"])


def test_a_library_child_is_limited_even_when_its_parent_is_trusted(tmp_path: Path) -> None:
    library = tmp_path / "library"
    _write(library / "lib_child.yml", CHILD)
    node, adapter = _run_parent(
        tmp_path, {"ref": "lib_child"}, parent_trusted=True, library=library
    )
    assert adapter.prompts == []
    assert "resolves outside the project" in str(node["meta"]["error"])


def test_the_same_child_reads_the_file_through_the_library_only_inside_its_project(
    tmp_path: Path,
) -> None:
    """Control: a library child is not broken, only confined to its own project."""
    library = tmp_path / "library"
    _write(library / "lib_child.yml", CHILD)
    (library / "p.txt").write_text("inside the library", encoding="utf-8")
    (tmp_path / "elsewhere").mkdir()
    parent = {
        "effects": [
            {
                "type": "use",
                "name": "child",
                "ref": "lib_child",
                "inputs": {"work": str(library), "prompt_file": "p.txt"},
            }
        ]
    }
    root = compile_orchestration(
        orch=parent, root_name="prime", document_dir=tmp_path, confinement_root=tmp_path, trusted=True
    )
    adapter = RecordingAdapter()
    DynamicRuntime(
        root,
        adapter=adapter,
        model="m",
        runtime_config={
            "_orchestration_dir": str(tmp_path),
            "library": {"sources": [{"type": "folder", "name": "local", "path": str(library)}]},
        },
    ).execute(store=Store({"input": {}}))
    assert adapter.prompts == ["inside the library"]


def test_a_use_definition_carries_its_documents_trust(tmp_path: Path) -> None:
    effect = {"type": "use", "name": "u", "path": "c.yml"}
    for trusted in (True, False):
        root = compile_orchestration(
            orch={"effects": [effect]},
            root_name="prime",
            document_dir=tmp_path,
            confinement_root=tmp_path,
            trusted=trusted,
        )
        assert root.effects[0].parent_trusted is trusted


def test_a_use_inline_child_is_limited_so_its_own_path_children_are(tmp_path: Path) -> None:
    child_dir = tmp_path / "child"
    _write(child_dir / "child.yml", CHILD)
    (child_dir / "config.json").write_text("{}", encoding="utf-8")
    inner = {
        "type": "use",
        "name": "inner",
        "path": "child/child.yml",
        "inputs": {
            "work": {"from": "input.work"},
            "prompt_file": {"from": "input.prompt_file"},
        },
    }
    inline = yaml.safe_dump({"effects": [inner]})
    node, adapter = _run_parent(tmp_path, {"inline": inline}, parent_trusted=True)
    assert adapter.prompts == []
    assert "resolves outside the project" in str(node["meta"]["error"])

