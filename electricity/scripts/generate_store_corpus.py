#!/usr/bin/env python3
"""Generate electricity-vm's store golden corpus from Circuitry's real
``core.store.store.Store`` (DESIGN.md §6.7, issue #431's Lane B section).

Each case is a sequence of operations run against a real ``Store`` (or, for
the tree-merge cases, several isolated branch stores) and the resulting
state, so electricity-vm's own `Store` can be driven through the identical
sequence and its materialized result compared byte-for-byte against
Circuitry's own -- including dict key *order*, which is why every value is
written through ``encode`` below (the same tagged scheme ``generate_value_
corpus.py``'s own ``encode`` uses, restricted to what this corpus needs: no
bytes/float/date) rather than a plain JSON object: a plain JSON object
would round-trip through whatever `serde_json::Map` representation
electricity-vm's own test happens to be built with, which isn't this
corpus's concern to depend on (and must never be, by adding a `preserve_
order`-style feature flag to a workspace-shared dependency like `serde_
json` -- Cargo's feature unification would turn that on for *every* crate
in the workspace that also depends on it, changing unrelated code's own
compiled shape; see this PR's own notes).

The tree-merge cases reproduce `core/dynamic.py`'s own merge-back loop
exactly (`for idx in range(n): for key, value in isolated_stores[idx].
state.items(): child_store.state[key] = value`) rather than
`Store.parallel_branches`'s own live-publish helper, which is a different,
coalesced-for-`--live-state` mechanism `core/dynamic.py` never calls to
produce its own final merge.

`last`-alias compaction (`core/saved_state.py::compact_last_aliases`) is
deliberately not in this corpus: that function isn't part of `Store`
itself (`core/loop.py` does the aliasing; `core/saved_state.py` does the
compaction), and `loop` is out of scope for M0-H's interpreter. electricity
-vm's own `Store::alias`/`Store::materialize` tests cover that mechanism
directly against DESIGN.md §6.7's documented shape instead.

Usage: python3 generate_store_corpus.py [--check]
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

from circuitry.core.store.store import Store

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-vm"
    / "tests"
    / "golden"
    / "store_corpus.json"
)


def encode(value: object) -> dict:
    """Same tagged scheme as ``generate_value_corpus.py``'s ``encode``,
    restricted to what this corpus ever contains (no bytes/float/date)."""
    if value is None:
        return {"t": "none"}
    if isinstance(value, bool):
        return {"t": "bool", "v": value}
    if isinstance(value, int):
        return {"t": "int", "v": str(value)}
    if isinstance(value, str):
        return {"t": "str", "v": value}
    if isinstance(value, list):
        return {"t": "list", "v": [encode(item) for item in value]}
    if isinstance(value, dict):
        return {"t": "dict", "v": [[encode(k), encode(v)] for k, v in value.items()]}
    raise TypeError(f"no tagged encoding for {type(value)!r}")


def sequence_case(name: str, ops: list[dict]) -> dict:
    """Runs *ops* (`{"op": "set", "path": ..., "value": ...}` or
    `{"op": "ensure_dict", "path": ...}`) against one fresh `Store` in
    order, and records the resulting state."""
    store = Store(state={})
    for op in ops:
        if op["op"] == "set":
            store.set(op["path"], op["value"])
        elif op["op"] == "ensure_dict":
            store.ensure_dict(op["path"])
        else:
            raise ValueError(f"unknown op {op['op']!r}")
    return {
        "name": name,
        "kind": "sequence",
        "ops": [encode_op(op) for op in ops],
        "expected_state": encode(store.state),
    }


def merge_case(name: str, initial: dict, branch_ops: list[list[dict]]) -> dict:
    """Builds a `Store` seeded with *initial*, `parallel_branches(len(
    branch_ops))`, runs each branch's own op sequence against its isolated
    store, then merges them back exactly as `core/dynamic.py`'s own tree
    dispatch does (its own doc comment above)."""
    store = Store(state=dict(initial))
    branches = store.parallel_branches(len(branch_ops))
    for branch, ops in zip(branches, branch_ops, strict=True):
        for op in ops:
            if op["op"] == "set":
                branch.set(op["path"], op["value"])
            else:
                raise ValueError(f"unknown op {op['op']!r}")
    for branch in branches:
        for key, value in branch.state.items():
            store.state[key] = value
    return {
        "name": name,
        "kind": "merge",
        "initial": encode(initial),
        "branch_ops": [[encode_op(op) for op in ops] for ops in branch_ops],
        "expected_state": encode(store.state),
    }


def encode_op(op: dict) -> dict:
    encoded = {"op": op["op"], "path": op["path"]}
    if "value" in op:
        encoded["value"] = encode(op["value"])
    return encoded


def build_corpus() -> list[dict]:
    return [
        sequence_case(
            "nested-ensure-dict-and-set",
            [
                {"op": "set", "path": "a.b.c", "value": 1},
                {"op": "set", "path": "a.b.d", "value": 2},
                {"op": "set", "path": "x", "value": "hello"},
            ],
        ),
        sequence_case(
            "ensure_dict_coerces_a_non_dict_value",
            [
                {"op": "set", "path": "count", "value": 5},
                {"op": "ensure_dict", "path": "count"},
                {"op": "set", "path": "count.y", "value": 1},
            ],
        ),
        sequence_case(
            "set_overwrites_a_dict_value_with_a_scalar",
            [
                {"op": "set", "path": "cfg.a", "value": 1},
                {"op": "set", "path": "cfg", "value": "now a string"},
            ],
        ),
        sequence_case(
            "non_string_feeling_but_still_json_safe_leaf_values",
            [
                {"op": "set", "path": "flags.on", "value": True},
                {"op": "set", "path": "flags.off", "value": False},
                {"op": "set", "path": "nothing", "value": None},
                {"op": "set", "path": "items", "value": [1, 2, 3]},
            ],
        ),
        merge_case(
            "two_branches_collide_on_one_key_last_index_wins",
            initial={"existing": 1},
            branch_ops=[
                [
                    {"op": "set", "path": "x", "value": "from branch 0"},
                    {"op": "set", "path": "existing", "value": "overwritten by 0"},
                ],
                [
                    {"op": "set", "path": "y", "value": "from branch 1"},
                ],
                [
                    {"op": "set", "path": "x", "value": "from branch 2"},
                    {"op": "set", "path": "existing", "value": "overwritten by 2"},
                ],
            ],
        ),
        merge_case(
            "branch_writes_a_nested_container_of_its_own",
            initial={},
            branch_ops=[
                [
                    {"op": "set", "path": "sub.a", "value": 1},
                    {"op": "set", "path": "sub.b", "value": 2},
                ],
            ],
        ),
        merge_case(
            "an_empty_branch_contributes_nothing",
            initial={"already": "here"},
            branch_ops=[[], [{"op": "set", "path": "new", "value": "value"}]],
        ),
    ]


def render(cases: list[dict]) -> str:
    return json.dumps(cases, indent=2, ensure_ascii=False, sort_keys=False) + "\n"


def main() -> int:
    text = render(build_corpus())
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    if "--check" in sys.argv[1:]:
        current = OUTPUT.read_text() if OUTPUT.exists() else ""
        if current != text:
            print(f"{OUTPUT} is stale; run without --check to regenerate", file=sys.stderr)
            return 1
        return 0
    OUTPUT.write_text(text)
    print(f"wrote {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
