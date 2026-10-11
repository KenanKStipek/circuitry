"""`Store.effect_path`/`Store.fire_concurrent_dispatch`/`Store.fire_branch_settled`
(#470): none of them ever lets a `None` name reach a path string.
`effect_path` raises if one somehow does (every remaining caller
guarantees a `str`); an unnamed `flow: tree` loop's passes have no name
of their own to append at all, so both `fire_concurrent_dispatch` and
`fire_branch_settled` fall back to the store's own `own_path` instead of
calling `effect_path` with `None`.

`Store.parallel_branches` (#475 R5) must prefix both hooks under its own
absolute path the same way it already does for `effect_start`/
`effect_complete`, so a loop (named or not) dispatching from inside
another tree loop's isolated branch reports the same absolute path its
own `start`/`end` events use, never a path relative to that branch.
"""

from __future__ import annotations

import pytest

from circuitry.core.store import Store


def test_effect_path_raises_on_a_none_name() -> None:
    store = Store(state={})
    with pytest.raises(ValueError, match="own_path"):
        store.effect_path(None)  # type: ignore[arg-type]


def test_fire_concurrent_dispatch_with_a_none_name_uses_own_path() -> None:
    seen: list[tuple[str, int, int]] = []
    store = Store(
        state={},
        concurrent_dispatch=lambda path, concurrency, total: seen.append(
            (path, concurrency, total)
        ),
    )
    child = store.child("outer")

    child.fire_concurrent_dispatch(None, 2, 3)

    assert seen == [("outer", 2, 3)]
    assert "None" not in seen[0][0]


def test_fire_concurrent_dispatch_with_a_name_still_appends_it() -> None:
    seen: list[tuple[str, int, int]] = []
    store = Store(
        state={},
        concurrent_dispatch=lambda path, concurrency, total: seen.append(
            (path, concurrency, total)
        ),
    )

    store.fire_concurrent_dispatch("each_tree", 2, 3)

    assert seen == [("each_tree", 2, 3)]


def test_fire_branch_settled_with_a_none_name_uses_own_path() -> None:
    seen: list[str] = []
    store = Store(state={}, branch_settled=seen.append)
    child = store.child("outer")

    child.fire_branch_settled(None)  # type: ignore[arg-type]

    assert seen == ["outer"]


def test_fire_branch_settled_with_a_name_still_appends_it() -> None:
    seen: list[str] = []
    store = Store(state={}, branch_settled=seen.append)

    store.fire_branch_settled("each_tree")

    assert seen == ["each_tree"]


def test_parallel_branches_prefixes_a_named_dispatch_under_the_outer_path() -> None:
    """A tree loop's own branch (`parallel_branches`) resets its path
    prefix to `""` for `effect_start`/`effect_complete` nesting (so a body
    effect's relative path composes under this store's own absolute one) —
    `concurrent_dispatch`/`branch_settled` must nest under that same
    absolute prefix, not pass the branch-relative path straight through.
    """
    dispatches: list[tuple[str, int, int]] = []
    settled: list[str] = []
    store = Store(
        state={},
        concurrent_dispatch=lambda path, concurrency, total: dispatches.append(
            (path, concurrency, total)
        ),
        branch_settled=settled.append,
    )
    outer = store.child("outer")
    (branch,) = outer.parallel_branches(1)

    branch.fire_concurrent_dispatch("inner", 2, 2)
    branch.fire_branch_settled("inner")

    assert dispatches == [("outer.inner", 2, 2)]
    assert settled == ["outer.inner"]


def test_parallel_branches_prefixes_an_unnamed_dispatch_without_a_trailing_dot() -> None:
    """An unnamed loop dispatching directly in a branch (its own relative
    path, `own_path`, is `""`) must still prefix to the branch's own
    absolute path exactly — never a trailing-dot `"outer."`.
    """
    dispatches: list[tuple[str, int, int]] = []
    settled: list[str] = []
    store = Store(
        state={},
        concurrent_dispatch=lambda path, concurrency, total: dispatches.append(
            (path, concurrency, total)
        ),
        branch_settled=settled.append,
    )
    outer = store.child("outer")
    (branch,) = outer.parallel_branches(1)

    branch.fire_concurrent_dispatch(None, 2, 2)  # type: ignore[arg-type]
    branch.fire_branch_settled(None)  # type: ignore[arg-type]

    assert dispatches == [("outer", 2, 2)]
    assert settled == ["outer"]
