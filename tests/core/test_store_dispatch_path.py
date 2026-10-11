"""`Store.effect_path`/`Store.fire_concurrent_dispatch` (#470): neither
ever lets a `None` name reach a path string. `effect_path` raises if one
somehow does (every real caller already guarantees a `str`); an unnamed
`flow: tree` loop's `dispatch` has no name of its own to append at all,
so `fire_concurrent_dispatch` falls back to the store's own `own_path`
instead of calling `effect_path` with `None`.
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
