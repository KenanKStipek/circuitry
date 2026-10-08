#!/usr/bin/env python3
"""Lane D's golden corpus: the compile-time half of #406 -- declared
prompts (`core/prompt_files.py`), the composition checks (`core/
prompt_compose.py`'s `check_prompt_composition`), and the document
digest (`document_content_digest`).

Covers every message in `core/prompt_files.py` and in `core/prompt_
compose.py`'s compile-time checks, CRLF prompt files, a symlink
escaping the project, and the digest (including a `../` prompt file).

Must be run with Python 3.11 (the lane venv locally; `actions/setup-
python` 3.11 in CI). Usage: python3 generate_compiler_compose_corpus.py
[--check]
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from _compiler_corpus import render_corpus, run_case, write_or_check

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-compiler"
    / "tests"
    / "golden"
    / "compose.json"
)

_YIELD = "effects:\n  - type: yield\n    name: x\n    template: 'hi'\n"

CASES: list[dict] = [
    # -- core/prompt_files.py -------------------------------------------------
    {
        "name": "prompts_not_a_mapping",
        "files": {"doc.yml": "prompts: nope\n" + _YIELD},
        "entry": "doc.yml",
    },
    {
        "name": "prompts_key_dotted",
        "files": {"doc.yml": "prompts:\n  a.b: hi\n" + _YIELD},
        "entry": "doc.yml",
    },
    {
        "name": "prompts_key_non_string",
        "files": {"doc.yml": "prompts:\n  5: hi\n" + _YIELD},
        "entry": "doc.yml",
    },
    {
        "name": "prompts_value_wrong_shape",
        "files": {"doc.yml": "prompts:\n  voice: 5\n" + _YIELD},
        "entry": "doc.yml",
    },
    {
        "name": "prompts_file_non_string",
        "files": {"doc.yml": "prompts:\n  voice: {file: 5}\n" + _YIELD},
        "entry": "doc.yml",
    },
    {
        "name": "prompts_file_absolute_path",
        "files": {"doc.yml": "prompts:\n  voice: {file: /etc/passwd}\n" + _YIELD},
        "entry": "doc.yml",
    },
    {
        "name": "prompts_file_template_tag",
        "files": {
            "doc.yml": "prompts:\n  voice: {file: '{{input.name}}.md'}\n" + _YIELD
        },
        "entry": "doc.yml",
    },
    {
        "name": "prompts_file_missing",
        "files": {"doc.yml": "prompts:\n  voice: {file: nope.md}\n" + _YIELD},
        "entry": "doc.yml",
    },
    {
        "name": "prompts_file_not_a_regular_file",
        "files": {
            "doc.yml": "prompts:\n  voice: {file: sub}\n" + _YIELD,
            "sub/placeholder.md": "x",
        },
        "entry": "doc.yml",
    },
    {
        "name": "prompts_file_over_size_limit",
        "files": {
            "doc.yml": "prompts:\n  voice: {file: big.md}\n" + _YIELD,
            "big.md": {"repeat": "x", "count": 1024 * 1024 + 1},
        },
        "entry": "doc.yml",
    },
    {
        "name": "prompts_file_not_utf8",
        "files": {
            "doc.yml": "prompts:\n  voice: {file: bad.md}\n" + _YIELD,
            "bad.md": {"bytes_hex": "616263ff64"},
        },
        "entry": "doc.yml",
    },
    {
        "name": "prompts_file_unreadable",
        "files": {
            "doc.yml": "prompts:\n  voice: {file: secret.md}\n" + _YIELD,
            "secret.md": {"bytes_hex": "6869", "mode": "000"},
        },
        "entry": "doc.yml",
        "error_modes": {"validate_errors": ["location"], "run_error": "location"},
    },
    {
        "name": "prompts_file_crlf_translated",
        "files": {
            "doc.yml": "prompts:\n  voice: {file: voice.md}\n" + _YIELD,
            "voice.md": "line one\r\nline two\rline three\n",
        },
        "entry": "doc.yml",
    },
    {
        "name": "prompts_file_symlink_escapes_the_project",
        "files": {
            "doc.yml": "prompts:\n  voice: {file: escape.md}\n" + _YIELD,
            "escape.md": {"symlink": "/nonexistent-outside-project/x.md"},
        },
        "entry": "doc.yml",
    },
    {
        "name": "prompts_file_dotdot_escapes_the_project",
        "files": {
            "proj/doc.yml": "prompts:\n  voice: {file: ../outside.md}\n" + _YIELD,
            "outside.md": "shh",
        },
        "entry": "proj/doc.yml",
    },
    {
        "name": "prompts_file_symlink_loop_could_not_resolve",
        "files": {
            "doc.yml": "prompts:\n  voice: {file: loopA.md}\n" + _YIELD,
            "loopA.md": {"symlink": "loopB.md"},
            "loopB.md": {"symlink": "loopA.md"},
        },
        "entry": "doc.yml",
        "error_modes": {"validate_errors": ["location"], "run_error": "location"},
    },
    # -- core/prompt_compose.py: check_prompt_composition ---------------------
    {
        "name": "effect_template_file_source",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: yield\n"
                "    name: x\n"
                "    template: {file: body.md}\n"
            ),
            "body.md": "hi {{> nope}}",
        },
        "entry": "doc.yml",
    },
    {
        "name": "effect_messages_content_file_source",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: prompt\n"
                "    name: p1\n"
                "    messages:\n"
                "      - role: user\n"
                "        content: {file: msg.md}\n"
            ),
            "msg.md": "hi {{> nope}}",
        },
        "entry": "doc.yml",
    },
    {
        "name": "unknown_partial_name",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: yield\n"
                "    name: x\n"
                "    template: 'hi {{> nope}}'\n"
            )
        },
        "entry": "doc.yml",
    },
    {
        "name": "declared_prompt_and_effect_name_collide",
        "files": {
            "doc.yml": (
                "prompts:\n"
                "  x: hi\n"
                "effects:\n"
                "  - type: yield\n"
                "    name: x\n"
                "    template: 'hi'\n"
            )
        },
        "entry": "doc.yml",
    },
    {
        "name": "reference_to_a_non_text_effect",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: tool\n"
                "    name: t1\n"
                "    provider: process\n"
                "    params: {command: 'echo hi'}\n"
                "  - type: yield\n"
                "    name: x\n"
                "    template: 'hi {{> t1}}'\n"
            )
        },
        "entry": "doc.yml",
    },
    {
        "name": "reference_to_a_json_prompt_is_not_text_producing",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: prompt\n"
                "    name: p1\n"
                "    prompt_type: json\n"
                "    template: 'give json'\n"
                "    schema: {type: object}\n"
                "  - type: yield\n"
                "    name: x\n"
                "    template: 'hi {{> p1}}'\n"
            )
        },
        "entry": "doc.yml",
    },
    {
        "name": "cycle_among_declared_prompts",
        "files": {
            "doc.yml": (
                "prompts:\n"
                "  a: '{{> b}}'\n"
                "  b: '{{> a}}'\n"
            )
            + _YIELD
        },
        "entry": "doc.yml",
    },
    {
        "name": "invalid_partial_name_shape",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: yield\n"
                "    name: x\n"
                "    template: 'hi {{> 1bad}}'\n"
            )
        },
        "entry": "doc.yml",
    },
    {
        "name": "set_delimiter_inside_a_declared_prompt",
        "files": {
            "doc.yml": (
                "prompts:\n"
                "  a: '{{=<% %>=}}'\n"
            )
            + _YIELD
        },
        "entry": "doc.yml",
    },
    {
        "name": "malformed_declared_prompt",
        "files": {
            "doc.yml": (
                "prompts:\n"
                "  a: '{{#open}}'\n"
            )
            + _YIELD
        },
        "entry": "doc.yml",
    },
    {
        "name": "bare_reference_does_not_cross_a_named_if_boundary",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: if\n"
                "    name: branch\n"
                "    if: {mode: cel, expr: 'true'}\n"
                "    then:\n"
                "      - type: yield\n"
                "        name: inner\n"
                "        template: 'hi'\n"
                "  - type: yield\n"
                "    name: outer\n"
                "    template: '{{> inner}}'\n"
            )
        },
        "entry": "doc.yml",
    },
    {
        "name": "dotted_reference_reaches_into_a_named_if_from_anywhere",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: if\n"
                "    name: branch\n"
                "    if: {mode: cel, expr: 'true'}\n"
                "    then:\n"
                "      - type: yield\n"
                "        name: inner\n"
                "        template: 'hi'\n"
                "  - type: yield\n"
                "    name: outer\n"
                "    template: '{{> branch.inner}}'\n"
            )
        },
        "entry": "doc.yml",
    },
    {
        "name": "dotted_self_reference_from_inside_the_same_named_if",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: if\n"
                "    name: branch\n"
                "    if: {mode: cel, expr: 'true'}\n"
                "    then:\n"
                "      - type: yield\n"
                "        name: inner\n"
                "        template: 'hi'\n"
                "      - type: yield\n"
                "        name: inner2\n"
                "        template: '{{> branch.inner}}'\n"
            )
        },
        "entry": "doc.yml",
    },
    {
        "name": "dotted_self_reference_from_inside_a_named_dynamic",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: dynamic\n"
                "    name: loop_ns\n"
                "    effects:\n"
                "      - type: yield\n"
                "        name: inner\n"
                "        template: 'hi'\n"
                "      - type: yield\n"
                "        name: inner2\n"
                "        template: '{{> loop_ns.inner}}'\n"
            )
        },
        "entry": "doc.yml",
    },
    {
        "name": "dotted_reference_inside_a_named_if_to_a_non_root_name",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: if\n"
                "    name: branch\n"
                "    if: {mode: cel, expr: 'true'}\n"
                "    then:\n"
                "      - type: yield\n"
                "        name: inner\n"
                "        template: 'hi'\n"
                "      - type: yield\n"
                "        name: inner2\n"
                "        template: '{{> inner.x}}'\n"
            )
        },
        "entry": "doc.yml",
    },
    {
        "name": "tool_params_partial_reference_is_checked",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: tool\n"
                "    name: t1\n"
                "    provider: process\n"
                "    params: {command: 'echo {{> nope}}'}\n"
            )
        },
        "entry": "doc.yml",
    },
    {
        "name": "dotted_reference_to_a_declared_prompt",
        "files": {
            "doc.yml": (
                "prompts:\n"
                "  greeting: hi\n"
                "effects:\n"
                "  - type: yield\n"
                "    name: ref\n"
                "    template: '{{> greeting.x}}'\n"
            )
        },
        "entry": "doc.yml",
    },
    {
        "name": "dotted_reference_to_a_missing_segment_under_a_named_if",
        "files": {
            "doc.yml": (
                "effects:\n"
                "  - type: if\n"
                "    name: branch\n"
                "    if: {mode: cel, expr: 'true'}\n"
                "    then:\n"
                "      - type: yield\n"
                "        name: inner\n"
                "        template: 'hi'\n"
                "  - type: yield\n"
                "    name: outer\n"
                "    template: '{{> branch.missing}}'\n"
            )
        },
        "entry": "doc.yml",
    },
    # -- digest -----------------------------------------------------------------
    {
        "name": "digest_with_no_prompt_files",
        "files": {"doc.yml": _YIELD},
        "entry": "doc.yml",
    },
    {
        "name": "digest_includes_a_referenced_prompt_file",
        "files": {
            "doc.yml": "prompts:\n  voice: {file: voice.md}\n" + _YIELD,
            "voice.md": "be nice",
        },
        "entry": "doc.yml",
    },
    {
        "name": "digest_includes_a_parent_directory_prompt_file",
        "files": {
            # A `config.json` at the project root makes `default_project_
            # root` confine `docs/doc.yml`'s `file:` references to the
            # whole project, not just `docs/` -- otherwise `../shared/
            # voice.md` would resolve outside the (otherwise-implied)
            # project root and fail confinement instead of exercising
            # the digest's own `../` label.
            "config.json": "{}",
            "docs/doc.yml": "prompts:\n  voice: {file: ../shared/voice.md}\n"
            + _YIELD,
            "shared/voice.md": "shared voice",
        },
        "entry": "docs/doc.yml",
    },
]


def main() -> int:
    results = [run_case(case) for case in CASES]
    text = render_corpus(results)
    return write_or_check(OUTPUT, text, check="--check" in sys.argv[1:])


if __name__ == "__main__":
    raise SystemExit(main())
