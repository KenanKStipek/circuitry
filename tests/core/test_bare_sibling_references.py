"""Bare sibling references (#778): pinned regressions the redesign must
never reintroduce.

The full behaviour (root, chain/tree dynamic, ``use`` child root,
``finally:``, nearest-wins, loop bindings, sections) is covered by the
conformance suite's ``c778-*`` cases, which exercise the real CLI/engine
path. This file pins three things only a Python-level unit test can check
cheaply: ``{{prime}}``'s own key order under ``PYTHONHASHSEED``, a
``resume: restart`` dynamic's own child read live (never a stale snapshot),
and a built-in context key kept from being shadowed by an effect of the
same name.
"""

from __future__ import annotations

import json
import subprocess
import sys
from dataclasses import dataclass
from typing import Any

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass
class EchoAdapter:
    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=prompt, raw={"model": model})


def _prompt(name: str, template: str) -> dict[str, Any]:
    return {"type": "prompt", "name": name, "template": template}


def test_resume_restart_dynamic_child_is_read_live_not_a_stale_snapshot() -> None:
    """A sibling that reads a ``resume: restart`` dynamic's own child, bare
    or dotted, sees what that dynamic actually ran this time -- never a
    snapshot taken before the restart dropped and replaced its state
    (core.dynamic.DynamicRuntime.execute's own restart branch).
    """
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "d",
                "resume": "restart",
                "effects": [_prompt("x", "new")],
            },
            _prompt("bare_reader", "{{d.x.value}}"),
            _prompt("dotted_reader", "{{prime.d.x.value}}"),
        ],
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    # Seeded as if an earlier run already wrote `d.x` -- `resume: restart`
    # must drop this whole node and run `d` fresh, not leave a reference to
    # it anywhere a later sibling's bare/dotted read could still reach.
    store = Store({"prime": {"d": {"x": {"value": "stale", "meta": {}}}}})
    DynamicRuntime(root, adapter=EchoAdapter(), model="m", resume=True).execute(
        store=store
    )
    assert store.state["prime"]["d"]["x"]["value"] == "new"
    assert store.state["prime"]["bare_reader"]["value"] == "new"
    assert store.state["prime"]["dotted_reader"]["value"] == "new"


def test_dynamic_second_child_reads_its_own_first_child_through_its_self_alias() -> None:
    """A dynamic ``d``'s own second child can read its first child through
    ``d``'s own self-alias (``{{d.x.value}}``), not only a name bare of any
    prefix -- ``d`` is itself a live top-level key in the context it
    renders its own children against (#778).
    """
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "d",
                "effects": [
                    _prompt("x", "x-val"),
                    _prompt("reader", "{{d.x.value}}"),
                ],
            },
        ],
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})
    DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=store)
    assert store.state["prime"]["d"]["reader"]["value"] == "x-val"


def test_dynamic_second_child_reads_its_restarted_first_child_through_its_self_alias() -> None:
    """The same read, from inside a ``resume: restart`` dynamic -- its
    second child's ``{{d.x.value}}`` sees the node ``d`` just rewrote this
    run, never the dropped, stale one (#778).
    """
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "d",
                "resume": "restart",
                "effects": [
                    _prompt("x", "new-x"),
                    _prompt("reader", "{{d.x.value}}"),
                ],
            },
        ],
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store(
        {
            "prime": {
                "d": {
                    "x": {"value": "stale", "meta": {}},
                    "reader": {"value": "stale-reader", "meta": {}},
                }
            }
        }
    )
    DynamicRuntime(root, adapter=EchoAdapter(), model="m", resume=True).execute(
        store=store
    )
    assert store.state["prime"]["d"]["reader"]["value"] == "new-x"


def test_tree_branch_sibling_read_renders_empty_bare_and_dotted() -> None:
    """A ``flow: tree`` branch's own read of a sibling branch, bare or
    dotted, renders empty -- each branch gets its own context copy, taken
    before any branch's write lands (``cancellation.submit_with_context``),
    so neither spelling can see a concurrent sibling's write (#778).
    """
    orch = {
        "flow": "tree",
        "effects": [
            _prompt("a", "a-val"),
            _prompt("bare_reader", "v={{a.value}}"),
            _prompt("dotted_reader", "v={{prime.a.value}}"),
        ],
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})
    DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=store)
    assert store.state["prime"]["bare_reader"]["value"] == "v="
    assert store.state["prime"]["dotted_reader"]["value"] == "v="


def test_builtin_run_id_is_not_hidden_by_an_effect_of_the_same_name() -> None:
    """``_run_id`` (and any other built-in context key) keeps its meaning
    even when an effect happens to share its name -- the bare-reference
    fallback is only ever consulted once every normal scope, including the
    run's own top-level keys, has already missed.
    """
    orch = {
        "effects": [
            {"type": "tool", "name": "_run_id", "provider": "env_vars", "params": {"mode": "get", "name": "PATH"}},
            _prompt("reader", "{{_run_id}}"),
        ],
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"_run_id": "the-real-run-id"})
    DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=store)
    assert store.state["prime"]["reader"]["value"] == "the-real-run-id"


_HASH_SEED_SCRIPT = """
import json
from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


class EchoAdapter:
    name = "echo"

    def generate(self, *, model, prompt, timeout_seconds=120):
        return GenerateResult(text=prompt, raw={})


orch = {
    "effects": [
        {"type": "prompt", "name": "a", "template": "a"},
        {"type": "prompt", "name": "b", "template": "b"},
        {"type": "prompt", "name": "c", "template": "c"},
        {"type": "prompt", "name": "whole", "template": "{{{prime}}}"},
    ],
}
root = compile_orchestration(orch=orch, root_name="prime")
store = Store({})
DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=store)
print(store.state["prime"]["whole"]["value"])
"""


def _key_order(obj: Any) -> Any:
    """*obj*'s own key order, recursively -- what ``PYTHONHASHSEED`` could
    disturb (a ``dict``'s own insertion order never depends on it; a
    ``set``'s iteration order does). Values are dropped: this run's own
    timestamps differ on every invocation regardless of hash seed.
    """
    if isinstance(obj, dict):
        return [(key, _key_order(value)) for key, value in obj.items()]
    if isinstance(obj, list):
        return [_key_order(item) for item in obj]
    return None


def test_nested_dynamic_in_loop_body_reads_its_own_child_not_a_root_sibling() -> None:
    """A dynamic ``d`` running inside a loop body resolves a bare sibling
    read through its own self-alias, always a live top-level key in the
    context it renders against -- not the dotted path ``push_bare_effect_
    scope`` would compute from its place in the chain, which isn't live
    while a loop body, an ``if`` branch or a tree branch is running
    (#778 finding 7). A root effect sharing the same name must not win.
    """
    orch = {
        "effects": [
            _prompt("x", "root-x"),
            {
                "type": "loop",
                "name": "lp",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {
                        "type": "dynamic",
                        "name": "d",
                        "flow": "chain",
                        "effects": [
                            _prompt("x", "loop-x"),
                            _prompt("reader", "{{x.value}}"),
                        ],
                    },
                ],
            },
        ],
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": [1]}})
    DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=store)
    reader = store.state["prime"]["lp"]["iter_0"]["d"]["reader"]
    assert reader["value"] == "loop-x"


def test_whole_prime_render_is_byte_identical_across_pythonhashseed() -> None:
    """A whole ``{{{prime}}}`` render's own key order must not depend on
    ``PYTHONHASHSEED`` -- the regression a dict-subclass view whose
    ``keys()`` returned a ``set`` would reintroduce (#778).
    """
    import os

    orders = []
    for seed in ("0", "1", "2"):
        proc = subprocess.run(
            [sys.executable, "-c", _HASH_SEED_SCRIPT],
            capture_output=True,
            text=True,
            env={**os.environ, "PYTHONHASHSEED": seed},
            check=True,
        )
        orders.append(_key_order(json.loads(proc.stdout)))
    assert orders[0] == orders[1] == orders[2]
    # Sanity: the render actually produced JSON with the three siblings,
    # not an empty/degenerate dict that would trivially "match".
    assert {"a", "b", "c"} <= {key for key, _ in orders[0]}
