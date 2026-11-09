from __future__ import annotations

import pytest

from circuitry.core.complexity import _estimate_tokens as scorer_estimate_tokens
from circuitry.core.split import (
    NoSplitPoint,
    estimate_tokens,
    split_list,
    split_mapping,
    split_text,
)


def test_estimate_tokens_matches_the_scorers_own_ceiling_division() -> None:
    for text in ("", "a", "ab", "abc", "abcd", "abcde", "x" * 97):
        assert estimate_tokens(text) == scorer_estimate_tokens(text)


def test_split_list_exact_parts() -> None:
    items = list(range(20))
    parts = split_list(items, room=5)

    assert parts == [[0, 1, 2, 3, 4, 5], [6, 7, 8, 9, 10, 11], [12, 13, 14, 15, 16], [17, 18, 19]]
    assert sum(len(p) for p in parts) == len(items)
    assert [x for p in parts for x in p] == items


def test_split_list_every_part_fits_the_room() -> None:
    items = list(range(37))
    room = 7
    parts = split_list(items, room=room)

    for part in parts:
        assert estimate_tokens(str(part)) <= room


def test_split_list_is_balanced_not_just_room_filled() -> None:
    # 20 single-digit ints: str(list) is "[0, 1, ..., 19]" — picking room=10
    # must not cram as many items as fit (room) into every part but the
    # last; it should come out close to even.
    items = list(range(20))
    parts = split_list(items, room=10)
    sizes = [estimate_tokens(str(p)) for p in parts]

    assert max(sizes) - min(sizes) <= 1 or len(parts) == 1


def test_split_list_single_item_too_large_is_no_split_point() -> None:
    with pytest.raises(NoSplitPoint) as excinfo:
        split_list([list(range(100))], room=5)

    assert excinfo.value.unit == "item 0"


def test_split_list_empty_is_empty() -> None:
    assert split_list([], room=5) == []


def test_split_mapping_exact_parts_whole_entries_in_order() -> None:
    mapping = {f"k{i}": i for i in range(10)}
    parts = split_mapping(mapping, room=15)

    assert all(isinstance(p, dict) for p in parts)
    # Order preserved, no entry duplicated or dropped.
    merged: dict[str, int] = {}
    for part in parts:
        merged.update(part)
    assert merged == mapping
    assert list(merged.keys()) == list(mapping.keys())


def test_split_mapping_single_entry_too_large_is_no_split_point() -> None:
    with pytest.raises(NoSplitPoint) as excinfo:
        split_mapping({"only": "x" * 1000}, room=5)

    assert excinfo.value.unit == "key 'only'"


def test_split_mapping_empty_is_empty() -> None:
    assert split_mapping({}, room=5) == []


def test_split_text_paragraphs_reconstruct_exactly() -> None:
    text = "Paragraph one.\n\nParagraph two.\n\nParagraph three."
    parts = split_text(text, room=4)

    assert "".join(parts) == text


def test_split_text_every_part_fits_the_room() -> None:
    text = "Hello world. This is a test sentence. Another one follows. " * 10
    room = 10
    parts = split_text(text, room=room)

    for part in parts:
        assert estimate_tokens(part) <= room
    assert "".join(parts) == text


def test_split_text_long_single_line_splits_on_sentences() -> None:
    text = "First sentence here. Second sentence here. Third sentence here."
    parts = split_text(text, room=8)

    assert "".join(parts) == text
    assert len(parts) > 1


def test_split_text_long_word_run_falls_back_to_whitespace_then_hard_cut() -> None:
    # No sentence ends, no newlines — forces the whitespace, then hard-cut,
    # fallback levels.
    text = " ".join(f"word{i}" for i in range(50))
    parts = split_text(text, room=3)

    assert "".join(parts) == text
    for part in parts:
        assert estimate_tokens(part) <= 3


def test_split_text_unbreakable_run_falls_back_to_a_hard_cut() -> None:
    text = "x" * 1000
    parts = split_text(text, room=10)

    assert "".join(parts) == text
    for part in parts:
        assert estimate_tokens(part) <= 10


def test_split_text_never_splits_inside_a_multi_byte_character() -> None:
    # Each "word" below is itself a single Python `str` code point (an
    # emoji) — a hard cut can only ever happen between code points in a
    # Python `str`, never inside one, so no emoji is ever corrupted.
    text = "🎉" * 50
    parts = split_text(text, room=2)

    assert "".join(parts) == text
    for part in parts:
        for char in part:
            assert char == "🎉"


def test_split_text_empty_is_empty() -> None:
    assert split_text("", room=5) == []


def test_split_text_non_positive_room_is_no_split_point() -> None:
    with pytest.raises(NoSplitPoint):
        split_text("hello world", room=0)


@pytest.mark.parametrize(
    "splitter_args",
    [
        ("list", list(range(30)), 6),
        ("mapping", {f"k{i}": i for i in range(15)}, 8),
        ("text", "one two three four five six seven eight nine ten. " * 5, 9),
    ],
)
def test_splitting_is_deterministic_across_calls(splitter_args: tuple) -> None:
    kind, value, room = splitter_args
    if kind == "list":
        first, second = split_list(value, room=room), split_list(value, room=room)
    elif kind == "mapping":
        first, second = split_mapping(value, room=room), split_mapping(value, room=room)
    else:
        first, second = split_text(value, room=room), split_text(value, room=room)

    assert first == second
