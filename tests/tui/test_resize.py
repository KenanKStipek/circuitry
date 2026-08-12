"""Resize safety: every screen renders at absurd sizes without exploding."""

from __future__ import annotations

from typing import Any

import pytest

pytest.importorskip("textual")

from textual.geometry import Size
from textual.pilot import Pilot

from circuitry.tui.layout import (
    COMPACT_WIDTH,
    SIZE_CLASSES,
    TINY_HEIGHT,
    TINY_WIDTH,
    ResponsiveLayout,
    fit,
    key_column_width,
    size_classes,
)
from circuitry.tui.screens import VIEWS

#: Down to the 10x4 the story calls for, plus a couple of degenerate sizes.
SIZES = [(80, 24), (60, 20), (40, 12), (24, 8), (16, 6), (10, 4), (4, 2), (1, 1)]

#: Key sequences that put the app on each distinct kind of screen.
SCREEN_KEYS: dict[str, tuple[str, ...]] = {
    "home": (),
    "help": ("question_mark",),
    **{spec.slug: (spec.key,) for spec in VIEWS},
    "help-on-view": ("2", "question_mark"),
}


@pytest.mark.parametrize("screen", list(SCREEN_KEYS), ids=list(SCREEN_KEYS))
def test_every_screen_renders_at_every_size(
    run_app: Any, capture_frame: Any, screen: str
) -> None:
    """One app per screen, resized through the whole ladder down to 10x4."""

    async def scenario(pilot: Pilot[Any]) -> list[tuple[tuple[int, int], list[str]]]:
        for key in SCREEN_KEYS[screen]:
            await pilot.press(key)
            await pilot.pause()
        frames: list[tuple[tuple[int, int], list[str]]] = []
        for size in SIZES:
            await pilot.resize_terminal(*size)
            await pilot.pause()
            frames.append((size, capture_frame(pilot.app).split("\n")))
        return frames

    for (width, height), lines in run_app(scenario):
        assert len(lines) == height, f"{width}x{height} rendered {len(lines)} rows"
        assert {len(line) for line in lines} == {width}, f"ragged frame at {width}x{height}"


@pytest.mark.parametrize("screen", list(SCREEN_KEYS), ids=list(SCREEN_KEYS))
def test_resize_torture(render: Any, screen: str) -> None:
    """Shrink and grow a live screen repeatedly; the frame must stay sane."""
    torture = [(80, 24), (10, 4), (120, 40), (12, 5), (1, 1), (40, 12), (10, 4), (80, 24)]
    frame = render(size=(80, 24), keys=list(SCREEN_KEYS[screen]), resizes=torture)
    lines = frame.split("\n")
    assert len(lines) == 24
    assert {len(line) for line in lines} == {80}
    assert frame.strip()


def test_chrome_is_dropped_on_a_tiny_screen(render: Any) -> None:
    """A 4-row terminal spends every row on content, not header/footer."""
    tiny = render(size=(20, 4))
    assert "1  Library" in tiny
    roomy = render(size=(80, 24))
    assert "Back / Quit" in roomy  # footer present when there is room
    assert "Back / Quit" not in tiny


def test_size_classes_are_applied_to_the_live_screen(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> list[set[str]]:
        seen: list[set[str]] = [set(pilot.app.screen.classes)]
        for width, height in [(30, 20), (10, 4), (100, 40)]:
            await pilot.resize_terminal(width, height)
            await pilot.pause()
            seen.append({c for c in pilot.app.screen.classes if c in SIZE_CLASSES})
        return seen

    wide, compact, tiny, wide_again = run_app(scenario, size=(80, 24))
    assert not wide & set(SIZE_CLASSES)
    assert compact == {"-compact"}
    assert tiny == {"-compact", "-tiny"}
    assert not wide_again


@pytest.mark.parametrize(
    ("width", "height", "expected"),
    [
        (80, 24, set()),
        (COMPACT_WIDTH, 24, set()),
        (COMPACT_WIDTH - 1, 24, {"-compact"}),
        (80, TINY_HEIGHT - 1, {"-compact", "-tiny"}),
        (TINY_WIDTH - 1, 24, {"-compact", "-tiny"}),
        (0, 0, {"-compact", "-tiny"}),
    ],
)
def test_size_classes(width: int, height: int, expected: set[str]) -> None:
    assert size_classes(width, height) == expected


@pytest.mark.parametrize(
    ("text", "width", "expected"),
    [
        ("hello", 10, "hello"),
        ("hello", 5, "hello"),
        ("hello", 4, "hel…"),
        ("hello", 1, "…"),
        ("hello", 0, ""),
        ("hello", -3, ""),
        ("", 4, ""),
    ],
)
def test_fit(text: str, width: int, expected: str) -> None:
    assert fit(text, width) == expected


@pytest.mark.parametrize(
    ("available", "expected"),
    [(0, 0), (-1, 0), (1, 1), (3, 1), (24, 8), (80, 12), (600, 12)],
)
def test_key_column_width(available: int, expected: int) -> None:
    assert key_column_width(available) == expected


class _Node(ResponsiveLayout):
    """Bare mixin host, to test the stamping without a whole Textual app."""

    def __init__(self) -> None:
        self.classes: set[str] = set()
        self.size = Size(80, 24)

    def add_class(self, *class_names: str) -> object:
        self.classes.update(class_names)
        return self

    def remove_class(self, *class_names: str) -> object:
        self.classes.difference_update(class_names)
        return self


def test_unknown_size_leaves_classes_alone() -> None:
    node = _Node()
    node.apply_size_classes(Size(20, 6))
    assert node.classes == {"-compact", "-tiny"}
    node.apply_size_classes(Size(0, 0))
    assert node.classes == {"-compact", "-tiny"}


def test_classes_are_replaced_not_accumulated() -> None:
    node = _Node()
    node.apply_size_classes(Size(10, 4))
    assert node.classes == {"-compact", "-tiny"}
    node.apply_size_classes(Size(30, 20))
    assert node.classes == {"-compact"}
    node.apply_size_classes(Size(100, 40))
    assert node.classes == set()
