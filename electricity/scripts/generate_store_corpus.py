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

`"alias"` cases additionally exercise `core/saved_state.py::
compact_last_aliases` directly -- `Store` itself has no `alias` operation
of its own (`core/loop.py` does the aliasing, by plain dict assignment:
`node["last"] = node[iter_key]`), so an `"alias"` op's own *path*/*target*
is applied the same way, always under the one shared parent `Store.alias`
(electricity-vm's own addition, ready for a later milestone's loop body)
also requires.

Usage: python3 generate_store_corpus.py [--check]
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

from circuitry.core.saved_state import compact_last_aliases
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


def apply_op(store: Store, op: dict) -> None:
    """Applies one `{"op": "set"|"ensure_dict"|"alias", ...}` corpus op to
    *store*. `"alias"`'s own *path*/*target* must share the same
    ancestors -- the Rust side's `Store::alias` takes one parent plus two
    keys under it, the same shape `core/loop.py`'s own `node["last"] =
    node[iter_key]` always has (both sides of that assignment already
    share the loop body's own node)."""
    kind = op["op"]
    if kind == "set":
        store.set(op["path"], op["value"])
    elif kind == "ensure_dict":
        store.ensure_dict(op["path"])
    elif kind == "alias":
        *alias_ancestors, alias_key = op["path"].split(".")
        *target_ancestors, target_key = op["target"].split(".")
        if alias_ancestors != target_ancestors:
            raise ValueError("alias and target must share the same parent")
        parent = store.ensure_dict(".".join(alias_ancestors)) if alias_ancestors else store.state
        parent[alias_key] = parent[target_key]
    else:
        raise ValueError(f"unknown op {kind!r}")


def sequence_case(name: str, ops: list[dict]) -> dict:
    """Runs *ops* against one fresh `Store` in order, and records the
    resulting state."""
    store = Store(state={})
    for op in ops:
        apply_op(store, op)
    return {
        "name": name,
        "kind": "sequence",
        "ops": [encode_op(op) for op in ops],
        "expected_state": encode(store.state),
    }


def alias_case(name: str, ops: list[dict]) -> dict:
    """Runs *ops* (`set`/`ensure_dict`/`alias`) against one fresh `Store`,
    then records both the plain state and `compact_last_aliases`'s own
    saved form of it -- the ground truth for `Store::snapshot` and
    `Store::saved` respectively."""
    store = Store(state={})
    for op in ops:
        apply_op(store, op)
    saved = compact_last_aliases(store.state)
    return {
        "name": name,
        "kind": "alias",
        "ops": [encode_op(op) for op in ops],
        "expected_state": encode(store.state),
        "expected_saved": encode(saved),
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
            apply_op(branch, op)
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
    if "target" in op:
        encoded["target"] = op["target"]
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
        merge_case(
            "branch_overwrites_a_parent_tracked_container_key",
            initial={"cfg": {"a": 1}},
            branch_ops=[[{"op": "set", "path": "cfg.b", "value": 2}]],
        ),
        sequence_case(
            "non_string_keys_inside_a_plain_dict_value",
            [
                {"op": "set", "path": "lookup", "value": {1: "one", 2: "two", "three": 3}},
            ],
        ),
        sequence_case(
            "ensure_dict_descends_into_a_plain_dict_value",
            [
                {"op": "set", "path": "tool_output", "value": {"nested": {"x": 1}}},
                {"op": "ensure_dict", "path": "tool_output.nested"},
                {"op": "set", "path": "tool_output.nested.y", "value": 2},
            ],
        ),
        alias_case(
            "last_aliased_to_an_iter_sibling_is_compacted_to_a_ref",
            [
                {"op": "set", "path": "prime.lp.iter_2.value", "value": "done"},
                {"op": "alias", "path": "prime.lp.last", "target": "prime.lp.iter_2"},
            ],
        ),
        alias_case(
            "an_unaliased_last_is_left_untouched",
            [
                {"op": "set", "path": "prime.lp.iter_0.value", "value": 1},
                {"op": "set", "path": "prime.lp.last", "value": "not an alias"},
            ],
        ),
        alias_case(
            "several_iter_siblings_alias_the_same_node_the_last_one_wins",
            [
                {"op": "set", "path": "prime.lp.iter_2.value", "value": 1},
                # `iter_5` deliberately aliased to the exact same node as
                # `iter_2` -- not a shape `core/loop.py` itself ever
                # produces, but the generic tie-break
                # `_aliased_iter_key`'s own reverse scan has to resolve.
                {"op": "alias", "path": "prime.lp.iter_5", "target": "prime.lp.iter_2"},
                {"op": "alias", "path": "prime.lp.last", "target": "prime.lp.iter_2"},
            ],
        ),
        alias_case(
            "a_last_that_is_its_own_tracked_dict_matching_no_sibling",
            [
                {"op": "set", "path": "prime.lp.last.value", "value": 1},
            ],
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
