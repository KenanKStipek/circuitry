#!/usr/bin/env python3
"""electricity-config's dotenv golden corpus (issue #468): every case
is parsed by python-dotenv's own ``dotenv_values`` -- the pinned version
in `generator-constraints.txt` -- not a re-implementation, so the Rust
port in ``electricity-config/src/dotenv.rs`` is checked against the
library it has to match: quoting, backslash escapes, the ``export ``
prefix, comments, multi-line (quoted) values, and ``${VAR}``/
``${VAR:-default}`` interpolation (``dotenv_values``'s own
``interpolate=True`` default, always on for `load_user_env`'s call).

Every ``.env`` text here is synthetic -- written for this issue, nothing
copied from a real file.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI). Usage: python3 generate_dotenv_corpus.py [--check]
"""

from __future__ import annotations

import io
import json
import os
import sys
from pathlib import Path
from typing import Any

from dotenv import dotenv_values

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-config"
    / "tests"
    / "golden"
    / "dotenv_corpus.json"
)


def case(name: str, text: str, *, real_env: dict[str, str] | None = None) -> dict[str, Any]:
    """One `dotenv_values` case. `dotenv_values` interpolates `${VAR}`
    against the *real* `os.environ` -- reproduced here by temporarily
    overlaying *real_env* onto it so interpolation resolves the same
    way `load_user_env`'s own call would, without ever touching this
    process's actual environment beyond the scope of one case."""
    saved = dict(os.environ)
    try:
        os.environ.clear()
        os.environ.update(real_env or {})
        values = dotenv_values(stream=io.StringIO(text))
    finally:
        os.environ.clear()
        os.environ.update(saved)
    return {
        "name": name,
        "text": text,
        "real_env": real_env or {},
        "expect": values,
    }


def build_cases() -> list[dict[str, Any]]:
    cases: list[dict[str, Any]] = []

    cases.append(case("simple_key_value", "FOO=bar\n"))
    cases.append(case("bare_key_has_no_value", "FOO\n"))
    cases.append(case("empty_value", "FOO=\n"))
    cases.append(case("whitespace_around_key_and_value_is_trimmed", "  FOO  =  bar  \n"))
    cases.append(
        case(
            "blank_lines_and_full_line_comments_are_skipped",
            "\n# a comment\n\nFOO=bar\n# another\n",
        )
    )
    cases.append(case("export_prefix_is_stripped", "export FOO=bar\n"))
    cases.append(case("inline_comment_after_unquoted_value", "FOO=bar # trailing comment\n"))
    cases.append(case("hash_with_no_leading_space_is_part_of_the_value", "FOO=bar#not_a_comment\n"))
    cases.append(
        case(
            "value_is_empty_when_hash_follows_whitespace_after_equals",
            "FOO= # just a comment\n",
        )
    )
    cases.append(case("single_quoted_value", "FOO='bar baz'\n"))
    cases.append(case("double_quoted_value", 'FOO="bar baz"\n'))
    cases.append(case("single_quoted_value_keeps_a_hash_literal", "FOO='bar # not a comment'\n"))
    cases.append(case("double_quoted_value_keeps_a_hash_literal", 'FOO="bar # not a comment"\n'))
    cases.append(case("double_quoted_escapes_decode", 'FOO="line1\\nline2\\tend"\n'))
    cases.append(
        case(
            "single_quoted_escapes_only_backslash_and_quote",
            "FOO='a\\nb\\'c\\\\d'\n",
        )
    )
    cases.append(case("multiline_double_quoted_value", 'FOO="line one\nline two\nline three"\n'))
    cases.append(case("multiline_single_quoted_value", "FOO='line one\nline two'\n"))
    cases.append(case("single_quoted_value_is_also_interpolated", "A=hello\nB='${A} world'\n"))
    cases.append(case("double_quoted_value_is_interpolated", 'A=hello\nB="${A} world"\n'))
    cases.append(case("unquoted_value_is_interpolated", "A=hello\nB=${A}_suffix\n"))
    cases.append(
        case(
            "interpolation_falls_back_to_the_real_environment",
            "B=${REAL_ONLY}\n",
            real_env={"REAL_ONLY": "from_env"},
        )
    )
    cases.append(case("interpolation_default_when_unset_anywhere", "B=${TOTALLY_UNSET:-fallback}\n"))
    cases.append(case("interpolation_default_is_empty_string", "B=${TOTALLY_UNSET:-}\n"))
    cases.append(
        case(
            "bare_key_resolves_to_empty_not_the_default",
            "A\nB=${A:-fallback}\n",
        )
    )
    cases.append(
        case(
            "a_file_value_outranks_the_real_environment_for_interpolation",
            "A=file_value\nB=${A}\n",
            real_env={"A": "env_value"},
        )
    )
    cases.append(case("repeated_key_keeps_first_position_takes_last_value", "A=1\nB=2\nA=3\n"))
    cases.append(case("crlf_line_endings", "FOO=bar\r\nBAZ=qux\r\n"))
    cases.append(case("single_quoted_key", "'FOO BAR'=baz\n"))
    cases.append(case("unterminated_single_quote_line_is_dropped", "FOO='unterminated\nBAR=baz\n"))
    cases.append(case("unterminated_double_quote_line_is_dropped", 'FOO="unterminated\nBAR=baz\n'))
    cases.append(case("stray_text_after_quoted_value_drops_the_line", 'FOO="bar"stray\nBAZ=qux\n'))
    cases.append(case("empty_file", ""))
    cases.append(case("only_whitespace", "   \n\n\t\n"))
    cases.append(case("no_trailing_newline_on_last_line", "FOO=bar"))
    cases.append(case("no_trailing_newline_quoted_value", 'FOO="bar"'))
    cases.append(case("nested_interpolation_through_two_names", "A=1\nB=${A}\nC=${B}2\n"))
    cases.append(case("unicode_value", "FOO=héllo wörld 🎉\n"))
    cases.append(case("key_with_underscores_and_digits", "FOO_BAR_2=baz\n"))
    cases.append(case("value_containing_equals_sign", "FOO=bar=baz=qux\n"))
    cases.append(case("multiple_variables_in_one_value", "A=1\nB=2\nC=${A}-${B}\n"))
    cases.append(case("unset_variable_in_middle_of_text", "A=${B}x${C:-y}\n"))

    return cases


def render(cases: list[dict[str, Any]]) -> str:
    return json.dumps(cases, indent=2, ensure_ascii=False, sort_keys=False) + "\n"


def main() -> int:
    check = "--check" in sys.argv[1:]
    cases = build_cases()
    text = render(cases)

    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    if check:
        current = OUTPUT.read_text(encoding="utf-8") if OUTPUT.exists() else ""
        if current != text:
            print(f"{OUTPUT} is stale; run without --check to regenerate", file=sys.stderr)
            return 1
        return 0
    OUTPUT.write_text(text, encoding="utf-8")
    print(f"wrote {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
