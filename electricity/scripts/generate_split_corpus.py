#!/usr/bin/env python3
"""Generate the golden corpus for `electricity-vm::split` (issue #682):
`core/split.py`'s own `split_list`/`split_mapping`/`split_text`, run for
real and recorded against the Rust port.

Every case gives the room budget and the exact resulting parts (or, for a
`NoSplitPoint`, the unit text that didn't fit) -- ported by hand as a JSON
corpus rather than copied source, since the two engines encode
lists/mappings differently (`electricity_value::Value` vs. Python's own
list/dict).

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI, `-c electricity/scripts/generator-constraints.txt`): `str()`/
`repr()` on a container drives every measurement here, and that text's
own shape is part of what this corpus pins.

Usage: python3 generate_split_corpus.py [--check]
"""

from __future__ import annotations

import json
import struct
import sys
from pathlib import Path
from typing import Any

from circuitry.core.split import NoSplitPoint, split_list, split_mapping, split_text

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-vm"
    / "tests"
    / "golden"
    / "split_corpus.json"
)


def encode(value: object) -> dict:
    if value is None:
        return {"t": "none"}
    if isinstance(value, bool):
        return {"t": "bool", "v": value}
    if isinstance(value, int):
        return {"t": "int", "v": str(value)}
    if isinstance(value, float):
        bits = struct.unpack("<Q", struct.pack("<d", value))[0]
        return {"t": "float", "bits": format(bits, "016x")}
    if isinstance(value, str):
        return {"t": "str", "v": value}
    if isinstance(value, list):
        return {"t": "list", "v": [encode(item) for item in value]}
    if isinstance(value, dict):
        return {"t": "dict", "v": [[encode(k), encode(v)] for k, v in value.items()]}
    raise TypeError(f"no tagged encoding for {type(value)!r}")


def list_case(name: str, items: list[Any], room: int) -> dict[str, Any]:
    try:
        parts = split_list(items, room=room)
    except NoSplitPoint as exc:
        return {"name": name, "kind": "list", "items": encode(items), "room": room, "parts": None, "error": exc.unit}
    return {
        "name": name,
        "kind": "list",
        "items": encode(items),
        "room": room,
        "parts": [encode(p) for p in parts],
        "error": None,
    }


def mapping_case(name: str, mapping: dict[str, Any], room: int) -> dict[str, Any]:
    try:
        parts = split_mapping(mapping, room=room)
    except NoSplitPoint as exc:
        return {
            "name": name,
            "kind": "mapping",
            "mapping": encode(mapping),
            "room": room,
            "parts": None,
            "error": exc.unit,
        }
    return {
        "name": name,
        "kind": "mapping",
        "mapping": encode(mapping),
        "room": room,
        "parts": [encode(p) for p in parts],
        "error": None,
    }


def text_case(name: str, text: str, room: int) -> dict[str, Any]:
    try:
        parts = split_text(text, room=room)
    except NoSplitPoint as exc:
        return {"name": name, "kind": "text", "text": text, "room": room, "parts": None, "error": exc.unit}
    return {
        "name": name,
        "kind": "text",
        "text": text,
        "room": room,
        "parts": [encode(p) for p in parts],
        "error": None,
    }


def build_cases() -> list[dict[str, Any]]:
    cases: list[dict[str, Any]] = []

    # --- lists ---------------------------------------------------------
    cases.append(list_case("list_empty", [], 10))
    cases.append(list_case("list_room_zero_refuses", [1], 0))
    cases.append(list_case("list_single_item_fits", [1, 2, 3], 50))
    cases.append(list_case("list_balanced_two_parts", list(range(10)), 5))
    cases.append(list_case("list_many_small_parts", list(range(30)), 4))
    cases.append(list_case("list_mixed_types", ["a", 1, 2.5, True, None, [1, 2], {"k": "v"}], 4))
    cases.append(list_case("list_one_item_too_big", ["x", "y" * 400], 2))
    cases.append(list_case("list_strings_unicode", ["café", "日本語", "a" * 20], 4))

    # --- mappings --------------------------------------------------------
    cases.append(mapping_case("mapping_empty", {}, 10))
    cases.append(mapping_case("mapping_room_zero_refuses", {"a": 1}, 0))
    cases.append(mapping_case("mapping_single_entry_fits", {"a": 1, "b": 2}, 50))
    cases.append(
        mapping_case(
            "mapping_balanced_two_parts",
            {f"k{i}": i for i in range(10)},
            6,
        )
    )
    cases.append(
        mapping_case(
            "mapping_one_entry_too_big",
            {"small": "x", "huge": "y" * 400},
            2,
        )
    )
    cases.append(
        mapping_case(
            "mapping_nested_values",
            {"a": {"x": 1, "y": 2}, "b": [1, 2, 3], "c": "plain"},
            6,
        )
    )

    # --- text: each level of the structural cascade ---------------------
    cases.append(text_case("text_empty", "", 10))
    cases.append(text_case("text_room_zero_refuses", "x", 0))
    cases.append(text_case("text_fits_whole", "short text", 50))
    cases.append(
        text_case(
            "text_blank_line_level",
            "Paragraph one here.\n\nParagraph two here.\n\nParagraph three here.",
            6,
        )
    )
    cases.append(
        text_case(
            "text_single_newline_level",
            "line one\nline two\nline three\nline four\nline five\nline six",
            4,
        )
    )
    cases.append(
        text_case(
            "text_sentence_level",
            "First sentence here. Second sentence here. Third sentence here. Fourth one.",
            6,
        )
    )
    cases.append(
        text_case(
            "text_whitespace_level",
            "one two three four five six seven eight nine ten eleven twelve",
            3,
        )
    )
    cases.append(
        text_case(
            # Python's own `str.isspace()` (and so `re`'s `\s`, which
            # `core/split.py::_WHITESPACE_RE` uses) treats U+001C-U+001F as
            # whitespace even though Unicode's `White_Space` property
            # doesn't -- a text with no higher-level structure (no blank
            # lines, newlines, or sentence ends) falls to this level and
            # still has to split at one.
            "text_whitespace_level_extra_control_whitespace",
            "aaaa\x1cbbbb\x1ccccc\x1cdddd\x1deeee\x1effff\x1fgggg",
            2,
        )
    )
    cases.append(
        text_case(
            # Unicode `White_Space` (not one of Python's own extra four
            # control characters) -- already matched correctly by a
            # plain Unicode `\s`, pinned here for symmetry with the
            # control-whitespace case above.
            "text_whitespace_level_unicode_whitespace",
            chr(0x2003).join(["aaaa", "bbbb", "cccc", "dddd"]),
            2,
        )
    )
    cases.append(
        text_case(
            "text_hard_cut_level",
            "x" * 60,
            3,
        )
    )
    cases.append(
        text_case(
            "text_hard_cut_multibyte",
            "日" * 40,
            2,
        )
    )
    cases.append(
        text_case(
            "text_mixed_cascade",
            (
                "Intro paragraph with a sentence. And another sentence here.\n\n"
                "Second paragraph,\nwith an embedded newline and more words after it "
                "to push past the room on this level too."
            ),
            5,
        )
    )
    cases.append(
        text_case(
            "text_long_single_word_hard_cut",
            "supercalifragilisticexpialidocious" * 3,
            2,
        )
    )

    return cases


def render(corpus: list[dict[str, Any]]) -> str:
    return json.dumps(corpus, indent=2, ensure_ascii=False, sort_keys=True) + "\n"


def write_or_check(path: Path, text: str, *, check: bool) -> bool:
    path.parent.mkdir(parents=True, exist_ok=True)
    if check:
        current = path.read_text() if path.exists() else ""
        if current != text:
            print(f"{path} is stale; run without --check to regenerate", file=sys.stderr)
            return False
        return True
    path.write_text(text)
    print(f"wrote {path}")
    return True


def main() -> int:
    check = "--check" in sys.argv[1:]
    text = render(build_cases())
    return 0 if write_or_check(OUTPUT, text, check=check) else 1


if __name__ == "__main__":
    raise SystemExit(main())
