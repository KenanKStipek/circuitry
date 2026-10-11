"""A `use` child's `concurrent_dispatch`/`branch_settled` (#483): a tree
loop's dispatch and branch-settled events must carry the same absolute path
its own `start`/`end` events use, the rule #470/#475 already settled for a
tree loop nested inside another tree loop's branch. Before this fix,
`core/use.py` wrapped `effect_start`/`effect_complete` with
`_namespaced_effect_cb` but passed `concurrent_dispatch`/`branch_settled`
through unwrapped, so a child loop's dispatch carried the child store's own
unnamespaced `prime.<loop>` path instead.
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import yaml

from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store
from circuitry.core.use import _namespaced_branch_settled_cb, _namespaced_dispatch_cb

# ── Helpers ──────────────────────────────────────────────────────────────────


def _write_orch(tmp_path: Path, name: str, content: dict) -> Path:
    path = tmp_path / name
    path.write_text(yaml.dump(content), encoding="utf-8")
    return path


def _mock_adapter(response: str = "mock response") -> MagicMock:
    adapter = MagicMock()
    adapter.name = "mock"
    result = MagicMock()
    result.text = response
    result.raw = {}
    result.tokens_sent = 10
    result.tokens_received = 5
    adapter.generate.return_value = result
    return adapter


def _tree_loop_child(loop_name: str = "fan") -> dict:
    return {
        "effects": [
            {
                "type": "loop",
                "name": loop_name,
                "flow": "tree",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "step", "template": "{{item}}"},
                ],
            },
        ]
    }


def _recording_store() -> tuple[Store, list[str], list[tuple[str, int, int]], list[str]]:
    """A root Store whose `effect_start`/`concurrent_dispatch`/`branch_settled`
    callbacks log the paths they are given."""
    started: list[str] = []
    dispatches: list[tuple[str, int, int]] = []
    settled: list[str] = []
    store = Store(
        state={},
        effect_start=lambda path, node: started.append(path),
        concurrent_dispatch=lambda path, concurrency, total: dispatches.append(
            (path, concurrency, total)
        ),
        branch_settled=settled.append,
    )
    return store, started, dispatches, settled


def _run_use(orch: dict, store: Store) -> None:
    DynamicRuntime(
        compile_orchestration(orch=orch), adapter=_mock_adapter(), model="test-model"
    ).execute(store=store)


# ── Unit tests: the wrapper functions themselves ─────────────────────────────


def test_namespaced_dispatch_cb_is_none_when_callback_is_none() -> None:
    assert _namespaced_dispatch_cb(None, "prime.sub") is None


def test_namespaced_branch_settled_cb_is_none_when_callback_is_none() -> None:
    assert _namespaced_branch_settled_cb(None, "prime.sub") is None


def test_namespaced_dispatch_cb_nests_under_node_path() -> None:
    seen: list[tuple[str, int, int]] = []
    wrapped = _namespaced_dispatch_cb(
        lambda path, concurrency, total: seen.append((path, concurrency, total)),
        "prime.sub",
    )
    assert wrapped is not None

    wrapped("prime.fan", 2, 2)

    assert seen == [("prime.sub.fan", 2, 2)]


def test_namespaced_branch_settled_cb_nests_under_node_path() -> None:
    seen: list[str] = []
    wrapped = _namespaced_branch_settled_cb(seen.append, "prime.sub")
    assert wrapped is not None

    wrapped("prime.fan")

    assert seen == ["prime.sub.fan"]


def test_namespaced_dispatch_cb_forwards_the_childs_own_root_path_at_node_path() -> None:
    """`prime` itself (an unnamed tree loop that is the entire child body)
    is the use node's own path already, unlike `effect_start`/
    `effect_complete` the `use` node never dispatches on its own behalf, so
    this dispatch is the only one that loop gets -- at `node_path`, not
    dropped."""
    seen: list[tuple[str, int, int]] = []
    wrapped = _namespaced_dispatch_cb(
        lambda path, concurrency, total: seen.append((path, concurrency, total)),
        "prime.sub",
    )
    assert wrapped is not None

    wrapped("prime", 2, 2)

    assert seen == [("prime.sub", 2, 2)]


def test_namespaced_branch_settled_cb_forwards_the_childs_own_root_path_at_node_path() -> None:
    seen: list[str] = []
    wrapped = _namespaced_branch_settled_cb(seen.append, "prime.sub")
    assert wrapped is not None

    wrapped("prime")

    assert seen == ["prime.sub"]


# ── Integration: dispatch/branch_settled through a real use child ───────────


def test_use_child_tree_loop_dispatch_matches_its_own_start_path(tmp_path: Path) -> None:
    child = _write_orch(tmp_path, "child.yml", _tree_loop_child())
    store, started, dispatches, settled = _recording_store()

    _run_use(
        {
            "effects": [
                {
                    "type": "use",
                    "name": "sub",
                    "path": str(child),
                    "inputs": {"items": ["a", "b"]},
                }
            ]
        },
        store,
    )

    assert "prime.sub.fan" in started
    assert [path for path, _, _ in dispatches] == ["prime.sub.fan"]
    assert settled == ["prime.sub.fan", "prime.sub.fan"]


def test_use_child_tree_loop_dispatch_namespaces_through_a_grandchild(
    tmp_path: Path,
) -> None:
    """A `use` inside a `use`'s dispatch composes to the grandchild's own
    depth, not just the immediate parent's."""
    grandchild = _write_orch(tmp_path, "grandchild.yml", _tree_loop_child())
    child = _write_orch(
        tmp_path,
        "child.yml",
        {
            "effects": [
                {
                    "type": "use",
                    "name": "inner",
                    "path": str(grandchild),
                    "inputs": {"items": {"from": "input.items"}},
                }
            ]
        },
    )
    store, started, dispatches, settled = _recording_store()

    _run_use(
        {
            "effects": [
                {
                    "type": "use",
                    "name": "outer",
                    "path": str(child),
                    "inputs": {"items": ["a", "b"]},
                }
            ]
        },
        store,
    )

    assert "prime.outer.inner.fan" in started
    assert [path for path, _, _ in dispatches] == ["prime.outer.inner.fan"]
    assert settled == ["prime.outer.inner.fan", "prime.outer.inner.fan"]


def test_use_child_tree_loop_nested_inside_another_tree_loop_namespaces_correctly(
    tmp_path: Path,
) -> None:
    """A tree loop nested inside another tree loop, both inside the child,
    must compose the child's own branch-prefixing (#475) with this use
    node's own namespacing (#483)."""
    child = _write_orch(
        tmp_path,
        "child.yml",
        {
            "effects": [
                {
                    "type": "loop",
                    "name": "outer",
                    "flow": "tree",
                    "each": {"in": "input.items", "as": "o"},
                    "body": [
                        {
                            "type": "loop",
                            "name": "inner",
                            "flow": "tree",
                            "each": {"in": "input.items", "as": "i"},
                            "body": [
                                {"type": "prompt", "name": "leaf", "template": "{{i}}"},
                            ],
                        },
                    ],
                },
            ]
        },
    )
    store, started, dispatches, settled = _recording_store()

    _run_use(
        {
            "effects": [
                {
                    "type": "use",
                    "name": "sub",
                    "path": str(child),
                    "inputs": {"items": ["a", "b"]},
                }
            ]
        },
        store,
    )

    dispatch_paths = {path for path, _, _ in dispatches}
    assert dispatch_paths == {
        "prime.sub.outer",
        "prime.sub.outer.iter_0.inner",
        "prime.sub.outer.iter_1.inner",
    }
    assert all(path in started for path in dispatch_paths)
    assert set(settled) == dispatch_paths
