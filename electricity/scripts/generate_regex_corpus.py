#!/usr/bin/env python3
"""Generate the golden corpus for lane D2 of issue #448 (#479):
`electricity-tools::regex`'s translation layer and `RegexPlugin` port,
by actually running Circuitry's own real plugin
(`plugins/regex.py::RegexPlugin`).

Priority order (per the issue's own test plan and rulings): `sub` with
a `replacement` (including backreferences and a bad template's bare
`re.error` text); `\\w`/`\\s`/`\\d` with Python 3.11's exact Unicode
membership; anchors (`^`/`$`/`\\A`/`\\Z`) with and without MULTILINE;
`findall` with zero/one/several groups (the tuple shapes); `search`/
`match` result shapes. Lookarounds, VERBOSE and IGNORECASE corners come
after these, with lighter coverage.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI, same as every other `generate_*_corpus.py`).

Usage: python3 generate_regex_corpus.py [--check]
Writes: crates/electricity-tools/tests/golden/regex_corpus.json
"""

from __future__ import annotations

import json
import re
import struct
import sys
import unicodedata
from pathlib import Path
from typing import Any

from circuitry.plugins.regex import RegexPlugin

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-tools"
    / "tests"
    / "golden"
    / "regex_corpus.json"
)


# ---------------------------------------------------------------------
# Tagged value encoding (matches generate_stdlib_tools_corpus.py's)
# ---------------------------------------------------------------------


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
    if isinstance(value, tuple):
        return {"t": "list", "v": [encode(item) for item in value]}
    if isinstance(value, dict):
        return {"t": "dict", "v": [[encode(k), encode(v)] for k, v in value.items()]}
    raise TypeError(f"no tagged encoding for {type(value)!r}")


def regex_case(name: str, params: dict[str, Any]) -> dict:
    plugin = RegexPlugin()
    try:
        result = plugin.execute(params=params, timeout_seconds=30)
    except Exception as exc:  # recording whatever the plugin raises
        # A third-party-library-shaped message (CPython's own `re.error`,
        # wrapped with electricity's `regex: invalid pattern: ` prefix or
        # not -- a `sub` template error) is compared only by *location*
        # (issue #479's ruling 3), never byte for byte; `exc.__cause__`
        # covers `raise ... from exc` (the plugin's own pattern-compile
        # wrapping), since `isinstance` on the wrapping `ValueError`
        # itself would otherwise miss it.
        is_re_error = isinstance(exc, re.error) or isinstance(exc.__cause__, re.error)
        return {
            "case": name,
            "params": encode(params),
            "ok": None,
            "raw": None,
            "err": str(exc),
            "is_re_error": is_re_error,
            "is_invalid_pattern": str(exc).startswith("regex: invalid pattern: "),
        }
    else:
        return {
            "case": name,
            "params": encode(params),
            "ok": encode(result.value),
            "raw": encode(result.raw),
            "err": None,
            "is_re_error": False,
            "is_invalid_pattern": False,
        }


def build_cases() -> list[dict]:
    cases: list[dict] = []

    def add(name: str, params: dict[str, Any]) -> None:
        cases.append(regex_case(name, params))

    # --- Priority 1: sub with replacement -----------------------------
    add("sub_backreference_swap", {"pattern": r"(\w+)@(\w+)", "input": "a@b", "mode": "sub", "replacement": r"\2@\1"})
    add("sub_named_group_g_angle", {"pattern": r"(?P<word>\w+)", "input": "hi", "mode": "sub", "replacement": r"\g<word>!"})
    add("sub_g0_whole_match", {"pattern": r"\w+", "input": "hi", "mode": "sub", "replacement": r"[\g<0>]"})
    add("sub_empty_match_iteration", {"pattern": "x*", "input": "abxd", "mode": "sub", "replacement": "-"})
    add("sub_count_one", {"pattern": "a", "input": "aaaa", "mode": "sub", "replacement": "X", "count": 1})
    add("sub_count_string_coercion", {"pattern": "a", "input": "aaaa", "mode": "sub", "replacement": "X", "count": "2"})
    add("sub_count_float_coercion", {"pattern": "a", "input": "aaaa", "mode": "sub", "replacement": "X", "count": 2.7})
    add("sub_count_bool_coercion", {"pattern": "a", "input": "aaaa", "mode": "sub", "replacement": "X", "count": True})
    add("sub_count_negative", {"pattern": "a", "input": "aaaa", "mode": "sub", "replacement": "X", "count": -1})
    add("sub_count_explicit_null", {"pattern": "a", "input": "aaaa", "mode": "sub", "replacement": "X", "count": None})
    add("sub_count_bad_string", {"pattern": "a", "input": "aaaa", "mode": "sub", "replacement": "X", "count": "x"})
    add("sub_bad_escape_in_template", {"pattern": "a", "input": "a", "mode": "sub", "replacement": r"\q"})
    add("sub_invalid_group_reference_in_template", {"pattern": "(a)", "input": "a", "mode": "sub", "replacement": r"\5"})
    add("sub_unknown_group_name_in_template", {"pattern": "(a)", "input": "a", "mode": "sub", "replacement": r"\g<bogus>"})
    add("sub_unterminated_name_in_template", {"pattern": "(a)", "input": "a", "mode": "sub", "replacement": r"\g<1"})
    add("sub_missing_replacement", {"pattern": "a", "input": "a", "mode": "sub"})
    add("sub_non_participating_group_is_empty", {"pattern": "(a)|(b)", "input": "b", "mode": "sub", "replacement": r"[\1][\2]"})
    add("sub_non_letter_escape_keeps_backslash", {"pattern": "a", "input": "a", "mode": "sub", "replacement": r"\@"})
    add("sub_backslash_b_is_backspace", {"pattern": "a", "input": "a", "mode": "sub", "replacement": r"\b"})
    add("sub_octal_escape_in_template", {"pattern": "a", "input": "a", "mode": "sub", "replacement": r"\101"})
    add("sub_g_angle_numeric_reference", {"pattern": "(a)", "input": "a", "mode": "sub", "replacement": r"\g<1>\g<1>"})

    # --- Priority 2: \w \s \d Unicode membership -----------------------
    add("word_class_excludes_combining_mark", {"pattern": r"\w", "input": "\u0345", "mode": "findall"})
    add("word_class_includes_other_numeric", {"pattern": r"\w", "input": "\u00b2", "mode": "findall"})  # SUPERSCRIPT TWO
    add("word_class_underscore", {"pattern": r"\w+", "input": "a_b", "mode": "findall"})
    add("space_class_includes_file_separator", {"pattern": r"\s", "input": "\x1c", "mode": "findall"})
    add("digit_class_is_nd_only", {"pattern": r"\d", "input": "\u00b2", "mode": "findall"})  # superscript two is No, not Nd
    add("word_class_ascii_flag_excludes_accented", {"pattern": r"\w+", "input": "café", "mode": "findall", "flags": ["ASCII"]})
    add("negated_word_class", {"pattern": r"\W+", "input": "a b", "mode": "findall"})
    add("word_boundary", {"pattern": r"\bcat\b", "input": "the cat sat", "mode": "search"})
    add("non_word_boundary", {"pattern": r"\Bcat", "input": "concatenate", "mode": "search"})
    add("word_boundary_empty_matches", {"pattern": r"\b", "input": "ab cd", "mode": "findall"})
    add("non_word_boundary_on_empty_input", {"pattern": r"\B", "input": "", "mode": "findall"})
    add("lookahead_empty_matches", {"pattern": r"(?=a)", "input": "aabaa", "mode": "findall"})

    # --- Character-class ranges, nested-set escapes, octal/backspace --
    add("class_range_lowercase", {"pattern": "[a-z]+", "input": "ABmnZ", "mode": "findall"})
    add("class_range_negated_digits", {"pattern": "[^0-9]+", "input": "a1b2c", "mode": "findall"})
    add("class_range_trailing_dash_is_literal", {"pattern": "[a-]+", "input": "a-a", "mode": "findall"})
    add("class_range_backwards_is_an_error", {"pattern": "[z-a]", "input": "a"})
    add("class_range_with_ignorecase", {"pattern": "[a-z]+", "input": "AbC", "mode": "findall", "flags": ["IGNORECASE"]})
    add("class_negated_word_shortcut", {"pattern": r"[\W]+", "input": "a b_c", "mode": "findall"})
    add("class_whitespace_or_non_whitespace_is_everything", {"pattern": r"[\s\S]+", "input": "a b", "mode": "findall"})
    add("class_backspace_escape", {"pattern": r"[\b]", "input": "\b", "mode": "search"})
    add("class_octal_escape_zero", {"pattern": r"[\0]", "input": "\0", "mode": "search"})
    add("class_octal_escape_three_digit", {"pattern": r"[\101]", "input": "A", "mode": "search"})
    add("class_octal_escape_one_digit_no_groups", {"pattern": r"[\1]", "input": "\x01", "mode": "search"})
    add("class_bad_escape_digit_eight", {"pattern": r"[\8]", "input": "8"})
    add("class_posix_lookalike_is_literal", {"pattern": r"[[:alpha:]]", "input": "a]", "mode": "search"})
    add("class_set_intersection_lookalike_is_literal", {"pattern": r"[a&&b]", "input": "a&b", "mode": "findall"})

    # --- IGNORECASE + ASCII, Turkish I --------------------------------
    add("ignorecase_ascii_folds_plain_letters", {"pattern": "abc", "input": "ABC", "mode": "search", "flags": ["IGNORECASE", "ASCII"]})
    add("ignorecase_turkish_i_dotless", {"pattern": "i", "input": "\u0131", "mode": "search", "flags": ["IGNORECASE"]})
    add("ignorecase_turkish_i_dotted_capital", {"pattern": "i", "input": "\u0130", "mode": "search", "flags": ["IGNORECASE"]})

    # --- Python 3.7+ empty-match scan rule (must_advance) --------------
    add("must_advance_alternation_caret_or_word", {"pattern": r"^|\w+", "input": "two words", "mode": "findall"})
    add("must_advance_bare_alternation_empty_first", {"pattern": "|a", "input": "aaa", "mode": "findall"})
    add("must_advance_lazy_optional", {"pattern": "a??", "input": "aaa", "mode": "findall"})

    # --- Literal brace, {,} shorthand, backwards brace quantifier -----
    add("literal_open_brace_not_a_quantifier", {"pattern": "a{", "input": "a{", "mode": "search"})
    add("brace_bare_comma_means_star", {"pattern": "a{,}", "input": "aaa", "mode": "findall"})
    add("brace_backwards_quantifier_is_an_error", {"pattern": "a{3,2}", "input": "aaa"})

    # --- Comment transparency, duplicate group names -------------------
    add("comment_transparent_to_following_quantifier", {"pattern": "a(?#c)*", "input": "aaa", "mode": "match"})
    add("duplicate_group_name_is_an_error", {"pattern": "(?P<a>x)(?P<a>y)", "input": "xy"})

    # --- ^ after a trailing newline, under MULTILINE --------------------
    add("caret_multiline_after_trailing_newline", {"pattern": "^", "input": "ab\n", "mode": "findall", "flags": ["MULTILINE"]})

    # --- Priority 3: anchors ($ ^ \A \Z) with/without MULTILINE --------
    add("dollar_before_trailing_newline_no_multiline", {"pattern": "a$", "input": "a\n", "mode": "search"})
    add("dollar_no_multiline_does_not_match_mid_string_newline", {"pattern": "a$", "input": "a\nb", "mode": "search"})
    add("dollar_multiline_matches_before_every_newline", {"pattern": "a$", "input": "a\nb", "mode": "search", "flags": ["MULTILINE"]})
    add("caret_multiline_matches_after_newline", {"pattern": "^b", "input": "a\nb", "mode": "search", "flags": ["MULTILINE"]})
    add("caret_no_multiline_only_matches_start", {"pattern": "^b", "input": "a\nb", "mode": "search"})
    add("backslash_A_literal", {"pattern": r"\Aabc", "input": "abc", "mode": "match"})
    add("backslash_Z_literal", {"pattern": r"abc\Z", "input": "abc", "mode": "search"})
    add("backslash_Z_not_before_trailing_newline", {"pattern": r"abc\Z", "input": "abc\n", "mode": "search"})

    # --- Priority 4: findall group shapes --------------------------------
    add("findall_no_groups", {"pattern": r"\d+", "input": "a1 b22", "mode": "findall"})
    add("findall_one_group", {"pattern": r"(\d+)", "input": "a1 b22", "mode": "findall"})
    add("findall_two_groups_tuples", {"pattern": r"(a)(b)?", "input": "ab a", "mode": "findall"})
    add("findall_non_participating_is_empty_string", {"pattern": r"(a)|(b)", "input": "ab", "mode": "findall"})
    add("findall_no_match", {"pattern": r"z+", "input": "abc", "mode": "findall"})

    # --- Priority 5: search/match result shapes --------------------------
    add("match_anchored_not_full", {"pattern": "ab", "input": "abc", "mode": "match"})
    add("match_no_match", {"pattern": "bc", "input": "abc", "mode": "match"})
    add("match_with_groups", {"pattern": r"(a)(b)", "input": "ab", "mode": "match"})
    add("match_non_participating_group_is_none", {"pattern": r"(a)(b)?", "input": "a", "mode": "match"})
    add("search_no_groups", {"pattern": r"\d+", "input": "abc 123", "mode": "search"})
    add("search_no_match", {"pattern": r"z+", "input": "abc", "mode": "search"})
    add("search_with_groups", {"pattern": r"(\w+)@(\w+)", "input": "a@b", "mode": "search"})

    # --- Parameter checks (exact text) -----------------------------------
    add("missing_pattern", {"input": "a"})
    add("empty_pattern", {"pattern": "", "input": "a"})
    add("non_string_pattern", {"pattern": 5, "input": "a"})
    add("missing_input", {"pattern": "a"})
    add("non_string_input", {"pattern": "a", "input": 5})
    add("unknown_mode", {"pattern": "a", "input": "a", "mode": "bogus"})
    add("mode_explicit_none", {"pattern": "a", "input": "a", "mode": None})
    add("mode_non_string", {"pattern": "a", "input": "a", "mode": 5})
    add("flags_not_a_list", {"pattern": "a", "input": "a", "flags": "IGNORECASE"})
    add("flags_falsy_empty_list", {"pattern": "a", "input": "a", "flags": []})
    add("flags_falsy_false", {"pattern": "a", "input": "a", "flags": False})
    add("flags_lowercase_name", {"pattern": "ABC", "input": "abc", "mode": "search", "flags": ["ignorecase"]})
    add("flags_unknown_name", {"pattern": "a", "input": "a", "flags": ["BOGUS"]})
    add("flags_unknown_non_string_item", {"pattern": "a", "input": "a", "flags": [1]})
    add("flags_unknown_none_item", {"pattern": "a", "input": "a", "flags": [None]})

    # --- Invalid patterns (location-compared) -----------------------------
    add("invalid_pattern_unterminated_group", {"pattern": "(", "input": "a"})
    add("invalid_pattern_unbalanced_close", {"pattern": "a)", "input": "a"})
    add("invalid_pattern_nothing_to_repeat", {"pattern": "*a", "input": "a"})
    add("invalid_pattern_multiple_repeat", {"pattern": "a**", "input": "a"})
    add("invalid_pattern_bad_escape", {"pattern": r"\q", "input": "a"})
    add("invalid_pattern_unterminated_class", {"pattern": "[abc", "input": "a"})
    add("invalid_pattern_variable_width_lookbehind", {"pattern": "(?<=a*)b", "input": "ab"})
    add("invalid_pattern_global_flag_not_at_start", {"pattern": "a(?i)b", "input": "ab"})
    add("invalid_pattern_bad_group_name", {"pattern": "(?P<1a>x)", "input": "a"})
    add("invalid_pattern_forward_backreference", {"pattern": r"\1(a)", "input": "aa"})
    add("invalid_pattern_bad_octal_range", {"pattern": r"\777", "input": "a"})
    add("invalid_pattern_L_flag", {"pattern": "(?L)a", "input": "a"})

    # --- VERBOSE / lookarounds / IGNORECASE (lower priority) --------------
    add("verbose_whitespace_and_comment", {"pattern": "a b # comment\n c", "input": "abc", "mode": "search", "flags": ["VERBOSE"]})
    add("verbose_space_inside_class", {"pattern": "[a b]+", "input": "a b", "mode": "search", "flags": ["VERBOSE"]})
    add("verbose_comma_n_quantifier_literal_vs_rust", {"pattern": "a{,3}", "input": "aaa", "mode": "findall", "flags": ["VERBOSE"]})
    add("lookbehind_and_backreference_case8", {"pattern": r"(?<=foo)bar", "input": "foobar", "mode": "search"})
    add("backreference_case8", {"pattern": r"(\w+)\1", "input": "hahaha", "mode": "search"})
    add("lookahead_assertion", {"pattern": r"foo(?=bar)", "input": "foobar", "mode": "search"})
    add("negative_lookahead", {"pattern": r"foo(?!bar)", "input": "foobaz", "mode": "search"})
    add("negative_lookbehind", {"pattern": r"(?<!foo)bar", "input": "xxxbar", "mode": "search"})
    add("atomic_group", {"pattern": r"(?>a+)b", "input": "aaab", "mode": "search"})
    add("possessive_quantifier", {"pattern": r"a++b", "input": "aaab", "mode": "search"})
    add("conditional_group", {"pattern": r"(a)?(?(1)b|c)", "input": "ac", "mode": "search"})
    add("named_backreference_p_equals", {"pattern": r"(?P<x>a)(?P=x)", "input": "aa", "mode": "search"})
    add("ignorecase_ascii_letters", {"pattern": "abc", "input": "ABC", "mode": "search", "flags": ["IGNORECASE"]})
    add("ignorecase_accented_letter", {"pattern": "é", "input": "É", "mode": "search", "flags": ["IGNORECASE"]})
    add("ignorecase_ascii_flag_restricts_folding", {"pattern": "k", "input": "\u212a", "mode": "search", "flags": ["IGNORECASE", "ASCII"]})  # KELVIN SIGN

    # --- raw/meta shape ----------------------------------------------------
    add("raw_carries_flags_as_given", {"pattern": "a", "input": "a", "mode": "search", "flags": ["IGNORECASE"]})
    add("raw_carries_none_flags_when_absent", {"pattern": "a", "input": "a", "mode": "search"})

    return cases


def unicode_metadata() -> dict:
    return {"unidata_version": unicodedata.unidata_version}


def render(corpus: dict) -> str:
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
    corpus = {"meta": unicode_metadata(), "cases": build_cases()}
    text = render(corpus)
    return 0 if write_or_check(OUTPUT, text, check=check) else 1


if __name__ == "__main__":
    raise SystemExit(main())
