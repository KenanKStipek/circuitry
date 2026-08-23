"""Text snapshots of the chrome, so layout regressions show up as a diff.

Re-record with ``CIRCUITRY_SNAPSHOT_UPDATE=1 pytest tests/tui``.
"""

from __future__ import annotations

from typing import Any

import pytest

pytest.importorskip("textual")

from circuitry.tui.execution import (
    PlanNode,
    build_tree,
    render_lines,
    render_text,
    score_column_width,
)
from circuitry.tui.layout import fit
from circuitry.tui.screens import HOME_ROW_BUDGET, VIEWS, home_row

CASES: list[tuple[str, tuple[int, int], list[str]]] = [
    ("home-80x24", (80, 24), []),
    ("home-40x12", (40, 12), []),
    ("home-10x4", (10, 4), []),
    ("help-80x24", (80, 24), ["question_mark"]),
    ("help-40x12", (40, 12), ["question_mark"]),
    ("view-library-80x24", (80, 24), ["1"]),
    ("view-library-40x12", (40, 12), ["1"]),
    ("view-library-10x4", (10, 4), ["1"]),
    ("library-search-80x24", (80, 24), ["1", "slash", "h", "e", "l", "l", "o"]),
    ("library-empty-80x24", (80, 24), ["1", "slash", "z", "z", "z", "z"]),
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


def test_every_home_row_fits_an_eighty_column_terminal() -> None:
    """Copy is written to the width it will be read at.

    The home list is the first thing anyone sees, and a blurb that runs off
    the edge stops mid-word. ``ViewRow`` ellipsises what does not fit, which
    is the right behaviour at 40 columns and an admission of defeat at 80.
    """
    too_long = {
        spec.slug: len(home_row(spec))
        for spec in VIEWS
        if len(home_row(spec)) > HOME_ROW_BUDGET
    }
    assert not too_long, f"blurbs over {HOME_ROW_BUDGET} cells: {too_long}"


def test_a_narrow_row_is_ellipsised_rather_than_cut() -> None:
    """Below the budget the cut is marked, so nobody misreads it as the end."""
    line = home_row(VIEWS[0])
    assert fit(line, 20).endswith("…")
    assert len(fit(line, 20)) == 20


def test_an_unscored_tree_draws_no_complexity_column() -> None:
    """The property that keeps the score column out of every future diff.

    A snapshot only earns its keep if it changes when the app does. The
    complexity column arrived after most of these files were recorded, and a
    column that drew *something* for an unscored effect — a dash, a blank
    gutter — would have made every one of them a permanent diff on every TUI
    PR from here on. It does not, and this is where that stays true.

    Asserted on the model rather than a frame so it holds at every width, and
    so a new snapshot that happens to include a scored run cannot mask it.
    """
    unscored = build_tree(
        (
            PlanNode(name="draft", kind="prompt"),
            PlanNode(name="polish", kind="prompt"),
        ),
        {},
    )
    lines = render_lines(unscored)
    assert lines, "the fixture stopped producing a tree"
    assert all(line.gutter == "" for line in lines)
    assert render_text(unscored) == "├─ · draft\n└─ · polish"
    assert score_column_width(unscored) == 0
    # Narrow enough that a column would have had to be dropped anyway; the
    # claim is that there was never one to drop.
    assert score_column_width(unscored, 30) == 0
