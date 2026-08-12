"""Resize-safe layout primitives shared by every Circuitry screen.

Terminals get resized to absurd sizes — a 10x4 pane is a real thing people
do. Nothing here may raise on a degenerate size; the contract is "render
something, never blow up". Screens opt in by mixing in
:class:`ResponsiveLayout`, which stamps CSS classes onto the screen so the
stylesheet can drop chrome (padding, borders, header/footer) as the box
shrinks.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:  # pragma: no cover - typing only
    from textual.events import Resize
    from textual.geometry import Size

#: Below this width the layout drops padding, borders and secondary copy.
COMPACT_WIDTH = 48

#: Below either of these the screen is treated as "tiny": header, footer and
#: every non-essential row are hidden so the body still gets a line to draw on.
TINY_WIDTH = 24
TINY_HEIGHT = 8

#: Every class this module manages, so a screen can clear them in one pass.
SIZE_CLASSES = ("-compact", "-tiny")

#: Appended when text has to be cut short.
ELLIPSIS = "…"


def size_classes(width: int, height: int) -> frozenset[str]:
    """Return the CSS classes describing a viewport of ``width`` x ``height``.

    ``-compact`` and ``-tiny`` stack: a tiny screen is always compact too, so
    compact styling does not have to be repeated in the tiny rules.
    """
    classes: set[str] = set()
    if width < COMPACT_WIDTH:
        classes.add("-compact")
    if width < TINY_WIDTH or height < TINY_HEIGHT:
        classes.update(("-compact", "-tiny"))
    return frozenset(classes)


def fit(text: str, width: int) -> str:
    """Truncate ``text`` to ``width`` cells, marking the cut with an ellipsis.

    Returns an empty string for a non-positive width rather than raising, so
    callers can hand it a computed size without guarding first.
    """
    if width <= 0:
        return ""
    if len(text) <= width:
        return text
    if width == 1:
        return ELLIPSIS
    return text[: width - 1] + ELLIPSIS


def key_column_width(available: int) -> int:
    """Width to reserve for the key column of a two-column key/description list.

    Roughly a third of the space, clamped so the column never starves the
    description or collapses to nothing on a narrow terminal.
    """
    if available <= 0:
        return 0
    return max(1, min(12, available // 3))


class ResponsiveLayout:
    """Mixin that keeps ``-compact`` / ``-tiny`` classes in sync with size.

    Mix into a ``Screen`` (before the Textual base class) and the classes are
    applied on mount and on every resize. Stylesheets then react with plain
    CSS instead of every screen hand-rolling its own breakpoint logic.
    """

    if TYPE_CHECKING:  # pragma: no cover - provided by the DOMNode we mix into
        size: Size

        def add_class(self, *class_names: str) -> object: ...

        def remove_class(self, *class_names: str) -> object: ...

    def apply_size_classes(self, size: Size) -> None:
        """Stamp the classes matching ``size`` onto this node.

        A zero size means "not laid out yet"; the Resize event that follows
        the first layout pass carries the real numbers.
        """
        if size.width <= 0 and size.height <= 0:
            return
        wanted = size_classes(size.width, size.height)
        stale = [name for name in SIZE_CLASSES if name not in wanted]
        if stale:
            self.remove_class(*stale)
        if wanted:
            self.add_class(*sorted(wanted))

    def on_mount(self) -> None:
        """Textual message handler: stamp classes before the first paint."""
        self.apply_size_classes(self.size)

    def on_resize(self, event: Resize) -> None:
        """Textual message handler: re-stamp classes for the new size."""
        self.apply_size_classes(event.size)
