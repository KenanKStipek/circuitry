"""Deterministic context splitting for the size trigger (#672).

Pure functions only — no template rendering, no state, no I/O. Given a
container value (a list, a mapping, or a string) and a *room* budget in
estimated tokens, :func:`split_list`/:func:`split_mapping`/:func:`split_text`
produce whole-unit, in-order parts, each of which — rendered the same way
Mustache renders a bare ``{{{x}}}``/``{{x}}`` tag, i.e. ``str(value)`` for a
non-string value, the value itself for a string — estimates to no more than
*room* tokens. :func:`circuitry.core.complexity._estimate_tokens`'s own
ceiling-division-by-4 is duplicated here as :func:`estimate_tokens` rather
than imported, so this module has no dependency on ``core.complexity`` (or
anything else) and stays trivially unit-testable; the two must be kept in
sync (a conformance/unit test pins that they agree).

A single unit — one list item, one mapping entry, or (after every level of
text-structure splitting has been tried) one character — that alone does not
fit *room* raises :class:`NoSplitPoint`; splitting is otherwise always
possible for text (the final, structural fallback is a hard cut at
``room * 4`` characters, which always fits by construction) but is a real,
reported failure for a list/mapping whose one unit is simply too big.

Balancing: for *n* = ``ceil(total_tokens / room)`` parts, a part fills with
whole units, in order, up to a *target* of ``ceil(total_tokens / n)`` tokens
— never *room*, which ``n``'s own definition guarantees ``target`` never
exceeds — so parts come out close to even in size rather than cramming every
part to the room ceiling except the last.
"""

from __future__ import annotations

import re
from collections.abc import Callable, Mapping, Sequence
from typing import Any

__all__ = [
    "NoSplitPoint",
    "estimate_tokens",
    "split_list",
    "split_mapping",
    "split_text",
]


def estimate_tokens(text: str) -> int:
    """Token estimate for a blob of text — ceiling division by 4 characters.

    Kept identical to, and independent of,
    :func:`circuitry.core.complexity._estimate_tokens` (see the module
    docstring for why this is a deliberate duplication, not an import).
    """
    if not text:
        return 0
    return -(-len(text) // 4)


class NoSplitPoint(ValueError):
    """A single unit does not fit *room* on its own — splitting stops here.

    ``unit`` names what didn't fit: ``"item <index>"`` for a list,
    ``"key <repr>"`` for a mapping, or ``"a run of text"`` for a string (text
    splitting only reaches this for a non-positive *room*; any positive room
    always has a hard-cut fallback that fits).
    """

    def __init__(self, unit: str) -> None:
        self.unit = unit
        super().__init__(f"{unit} does not fit the room")


def _pack(
    unit_count: int,
    *,
    render_slice: Callable[[int, int], str],
    label_at: Callable[[int], str],
    room: int,
) -> list[tuple[int, int]]:
    """Partition ``range(unit_count)`` into whole-unit ``[start, end)`` ranges.

    ``render_slice(start, end)`` is the exact text a part spanning that range
    would render to (container syntax included) — measured directly, at
    every candidate boundary, rather than approximated from independently
    rendered units, so the "never over room" guarantee holds for the actual
    rendered text every time, not just on average.
    """
    if unit_count == 0:
        return []

    total_tokens = estimate_tokens(render_slice(0, unit_count))
    n = max(1, -(-total_tokens // room)) if room > 0 else unit_count
    target = -(-total_tokens // n) if n > 0 else total_tokens

    ranges: list[tuple[int, int]] = []
    start = 0
    end = 0
    while end < unit_count:
        if end == start:
            # A part must contain at least one whole unit — this is the one
            # check that bounds against `room` rather than `target`: a
            # single oversized unit cannot be shrunk to fit a smaller target.
            if estimate_tokens(render_slice(start, start + 1)) > room:
                raise NoSplitPoint(label_at(start))
            end = start + 1
            continue
        if estimate_tokens(render_slice(start, end + 1)) > target:
            ranges.append((start, end))
            start = end
            continue
        end += 1
    ranges.append((start, end))
    return ranges


def split_list(items: Sequence[Any], *, room: int) -> list[list[Any]]:
    """Cut *items* into contiguous, whole-item, balanced slices."""
    materialized = list(items)
    if not materialized:
        return []
    if room < 1:
        raise NoSplitPoint("item 0")

    def render_slice(start: int, end: int) -> str:
        return str(materialized[start:end])

    ranges = _pack(
        len(materialized),
        render_slice=render_slice,
        label_at=lambda index: f"item {index}",
        room=room,
    )
    return [materialized[start:end] for start, end in ranges]


def split_mapping(mapping: Mapping[str, Any], *, room: int) -> list[dict[str, Any]]:
    """Cut *mapping* into contiguous, whole-entry, balanced groups, in stored
    key order."""
    keys = list(mapping.keys())
    if not keys:
        return []
    if room < 1:
        raise NoSplitPoint(f"key {keys[0]!r}")

    def render_slice(start: int, end: int) -> str:
        return str({key: mapping[key] for key in keys[start:end]})

    ranges = _pack(
        len(keys),
        render_slice=render_slice,
        label_at=lambda index: f"key {keys[index]!r}",
        room=room,
    )
    return [{key: mapping[key] for key in keys[start:end]} for start, end in ranges]


#: Priority order of structural split points, each tried only on a unit the
#: level above left too big — see :func:`_atomic_units`.
_BLANK_LINE_RE = re.compile(r"\n[ \t]*\n+")
_NEWLINE_RE = re.compile(r"\n")
#: ". "/"? "/"! " followed by a capital letter or a newline — the sentence
#: end itself (punctuation + the one space) stays attached to the preceding
#: chunk, so concatenating every chunk reconstructs the original text byte
#: for byte.
_SENTENCE_RE = re.compile(r"[.?!] (?=[A-Z\n])")
_WHITESPACE_RE = re.compile(r"\s+")


def _split_keep_leading(text: str, pattern: re.Pattern[str]) -> list[str]:
    """*text* cut right after each *pattern* match; concatenating the
    returned chunks reconstructs *text* exactly — the matched separator
    itself stays attached to the chunk before it."""
    chunks: list[str] = []
    pos = 0
    for match in pattern.finditer(text):
        end = match.end()
        if end <= pos:
            continue
        chunks.append(text[pos:end])
        pos = end
    if pos < len(text):
        chunks.append(text[pos:])
    return chunks


def _atomic_units(text: str, room: int) -> list[str]:
    """*text* broken into pieces that each, standing alone, fit *room* —
    recursing through blank lines, single newlines, sentence ends,
    whitespace, and finally a hard character cut, each level applied only to
    a piece the level above left too big. Concatenating the result, in
    order, always reconstructs *text* exactly."""
    if estimate_tokens(text) <= room:
        return [text]
    return _atomic_units_at(text, room, _LEVELS)


#: `(pattern, next level)` chain; `None` is the terminal hard-cut level.
_LEVELS: tuple[re.Pattern[str] | None, ...] = (
    _BLANK_LINE_RE,
    _NEWLINE_RE,
    _SENTENCE_RE,
    _WHITESPACE_RE,
    None,
)


def _atomic_units_at(
    text: str, room: int, levels: tuple[re.Pattern[str] | None, ...]
) -> list[str]:
    if estimate_tokens(text) <= room:
        return [text]
    if not levels:
        # Should be unreachable: the hard-cut level (None) is always last
        # and always fits, by construction (room * 4 characters is exactly
        # `room` estimated tokens, rounding only ever down to that).
        raise NoSplitPoint("a run of text")

    pattern, rest = levels[0], levels[1:]
    if pattern is None:
        chunk_chars = max(1, room * 4)
        # A plain character-index slice of a Python `str` — never a byte
        # slice — so this can never land inside a multi-byte UTF-8 sequence
        # or a surrogate pair; Python's `str` has no such internal unit to
        # split (#672 R3).
        return [text[i : i + chunk_chars] for i in range(0, len(text), chunk_chars)]

    pieces = _split_keep_leading(text, pattern)
    if len(pieces) <= 1:
        return _atomic_units_at(text, room, rest)
    units: list[str] = []
    for piece in pieces:
        units.extend(_atomic_units_at(piece, room, rest))
    return units


def split_text(text: str, *, room: int) -> list[str]:
    """Cut *text* into balanced, whole-structural-unit, in-order parts."""
    if not text:
        return []
    if room < 1:
        raise NoSplitPoint("a run of text")

    units = _atomic_units(text, room)

    def render_slice(start: int, end: int) -> str:
        return "".join(units[start:end])

    ranges = _pack(
        len(units),
        render_slice=render_slice,
        label_at=lambda _index: "a run of text",
        room=room,
    )
    return ["".join(units[start:end]) for start, end in ranges]
