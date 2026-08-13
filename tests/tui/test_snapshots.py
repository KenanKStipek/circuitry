"""Text snapshots of the chrome, so layout regressions show up as a diff.

Re-record with ``CIRCUITRY_SNAPSHOT_UPDATE=1 pytest tests/tui``.
"""

from __future__ import annotations

from typing import Any

import pytest

pytest.importorskip("textual")

from circuitry.tui.screens import VIEWS

CASES: list[tuple[str, tuple[int, int], list[str]]] = [
    ("home-80x24", (80, 24), []),
    ("home-40x12", (40, 12), []),
    ("home-10x4", (10, 4), []),
    ("help-80x24", (80, 24), ["question_mark"]),
    ("help-40x12", (40, 12), ["question_mark"]),
    ("view-library-80x24", (80, 24), ["1"]),
    ("view-library-10x4", (10, 4), ["1"]),
    # The chat view opens on its seed form, which is static until it is
    # submitted — no worker runs, so the frame is deterministic.
    ("view-chat-80x24", (80, 24), ["8"]),
    ("view-chat-40x12", (40, 12), ["8"]),
]


@pytest.mark.parametrize(("name", "size", "keys"), CASES, ids=[case[0] for case in CASES])
def test_snapshot(
    render: Any, snapshot: Any, name: str, size: tuple[int, int], keys: list[str]
) -> None:
    snapshot.assert_match(render(size=size, keys=keys), name)


def test_every_view_has_a_placeholder_snapshot_worthy_body(render: Any) -> None:
    """Cheap coverage that each view renders its own name and blurb."""
    for spec in VIEWS:
        frame = render(size=(100, 30), keys=[spec.key])
        assert spec.name in frame
        assert spec.blurb.split(" ")[0] in frame
