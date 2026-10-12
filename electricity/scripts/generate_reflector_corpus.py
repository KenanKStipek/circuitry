#!/usr/bin/env python3
"""Generate the golden corpus for lane L1 of issue #448 (electricity's
`reflector` effect, issue #506) from Circuitry's own real
`core/reflector.py` functions.

Four of that module's own helpers are pure enough to call directly
against synthetic input, with no `Store`/run context needed:

- `_clean_yaml_fences`/`_extract_done_flag`: fences, `---`, every
  `str.splitlines()` separator, `steps`/`plan` dict/list, a bare list,
  a scalar, invalid YAML, mixed key types, `done` truthiness.
- `_render_reflector_prime`: format specs, `!r`, `{{ }}`, unknown
  fields, lone braces -- records the rendered text plus whether the
  `.format()` call itself raised (`fell_back`), independent of the
  function's own `logger.warning` side effect.
- the redact/`json.dumps`/truncate transform `_best_effort_context`
  wraps around a `Store` this script has none of: `_context_from_settings`
  below is that same transform's own body, called directly against a
  synthetic `effective_settings` dict (missing keys, non-ASCII,
  secrets, more than 2000 code points) -- not `_best_effort_context`
  itself, which needs a live `Store`.

Must be run with Python 3.11 and the pinned dependencies
(`generator-constraints.txt`): PyYAML's own `safe_load`/`dump` output,
and `json.dumps`'s own escaping, are what several of these cases pin.

Usage: python3 generate_reflector_corpus.py [--check]
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

from circuitry.cli.redaction import redact
from circuitry.core.reflector import (
    _CONTEXT_MAX_CHARS,
    _clean_yaml_fences,
    _extract_done_flag,
    _render_reflector_prime,
)

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-vm"
    / "tests"
    / "golden"
    / "reflector_corpus.json"
)


# ---------------------------------------------------------------------
# clean_yaml_fences / extract_done_flag
# ---------------------------------------------------------------------


def clean_fences_case(text: str) -> dict:
    return {"kind": "clean_fences", "input": text, "cleaned": _clean_yaml_fences(text)}


def extract_done_case(text: str) -> dict:
    try:
        done, plan_yaml = _extract_done_flag(text)
        return {
            "kind": "extract_done",
            "input": text,
            "ok": True,
            "done": done,
            "plan_yaml": plan_yaml,
        }
    except Exception as exc:  # recording the failure itself
        return {
            "kind": "extract_done",
            "input": text,
            "ok": False,
            "error": str(exc),
        }


def clean_fences_cases() -> list[dict]:
    texts = [
        "```yaml\neffects: []\n---\n```",
        "effects:\n  - type: tool\n```\n",
        "a\x0cb",
        "a\rb",
        "a\u2028b",
        "a\x85b",
        "a\u2029b",
        "```\x0ceffects: []\x0c---\x0c```",
        "  \n```\n  \n",
        "no fences at all\njust plain text",
    ]
    return [clean_fences_case(t) for t in texts]


def extract_done_cases() -> list[dict]:
    texts = [
        "",
        "   \n\n",
        "done: false\neffects:\n  - type: prompt\n    name: a\n    template: hi\n",
        "done: true\neffects: []\n",
        "steps:\n  - type: prompt\n    name: a\n    template: hi\ndone: false\n",
        "plan:\n  effects:\n    - type: prompt\n      name: a\n      template: hi\ndone: false\n",
        "plan:\n  - type: prompt\n    name: a\n    template: hi\ndone: false\n",
        "- type: prompt\n  name: a\n  template: hi\n",
        "I cannot plan this.\n",
        "done: false\nnotes: just a mapping with no effects/steps/plan\n",
        "done: 'false'\neffects: []\n",
        "done: 'no'\neffects: []\n",
        "done: no\neffects: []\n",
        "done: off\neffects: []\n",
        "effects: [\n",  # unparseable YAML
        "1: a\nb: c\neffects: []\n",  # mixed key types, but no sort needed (no done re-dump path hits sort)
        "```yaml\ndone: false\neffects:\n  - type: tool\n    name: x\n    provider: json\n```\n",
        "done: false\nsteps: not-a-list\neffects: []\n",
        "done: false\nplan: not-a-dict-or-list\n",
    ]
    return [extract_done_case(t) for t in texts]


# ---------------------------------------------------------------------
# _render_reflector_prime
# ---------------------------------------------------------------------


def prime_case(
    template: str,
    *,
    max_effects: int = 8,
    goal: str = "g",
    context: str = "c",
    divergence: bool = False,
) -> dict:
    """*divergence* marks a case electricity's own `py_str_format`
    deliberately does not reproduce (the settled "attribute/index
    access falls back" divergence) -- the Rust replay test skips the
    exact-text comparison for one of these, checking only that its own
    implementation took the fallback branch instead.
    """
    rendered = _render_reflector_prime(
        prime_template=template, max_effects=max_effects, goal=goal, context=context
    )
    try:
        (template or "").format(max_effects=max_effects, goal=goal, context=context)
        fell_back = False
    except Exception:  # detecting the fallback branch itself
        fell_back = True
    return {
        "kind": "prime",
        "template": template,
        "max_effects": max_effects,
        "goal": goal,
        "context": context,
        "fell_back": fell_back,
        "divergence": divergence,
        "rendered": rendered,
    }


def prime_cases() -> list[dict]:
    # `{goal.upper}` (attribute access) is deliberately excluded: Python's
    # `str.format` actually succeeds there, printing the *bound method's*
    # own `repr` (`<built-in method upper of str object at 0x...>`, a
    # memory address) -- non-deterministic even run to run over the same
    # input, so no golden file could ever pin it. `py_str_format` chooses
    # the documented divergence instead (DESIGN.md §13): fall back to the
    # raw template rather than try to reproduce an address.
    return [
        prime_case("Goal: {goal}\nContext: {context}\nMax: {max_effects}"),
        prime_case("Max (padded): {max_effects:03d}"),
        prime_case("Context repr: {context!r}"),
        prime_case("Goal str: {goal!s}"),
        prime_case("Literal braces: {{not a field}}"),
        prime_case("Unknown field: {nope}"),
        prime_case("Lone brace: {"),
        prime_case("Lone close: }"),
        prime_case("Positional: {}"),
        # Python succeeds here (`"g"[0]`), deterministically -- but this
        # is the settled "index access falls back" divergence (DESIGN.md
        # §13): `py_str_format` never does attribute/index access at all.
        prime_case("Index access: {goal[0]}", divergence=True),
        prime_case("Bad conversion: {goal!x}"),
        prime_case("", max_effects=3, goal="", context=""),
        prime_case("{max_effects:<6d}|end"),
        prime_case("Quoted: {context!r}", context="has 'single' and \"double\""),
        prime_case("Non-ascii: {goal}", goal="caf\u00e9 \U0001f600"),
    ]


# ---------------------------------------------------------------------
# _best_effort_context's own redact/json.dumps/truncate transform
# ---------------------------------------------------------------------


def _context_from_settings(effective_settings: dict) -> str:
    """`_best_effort_context`'s own body, given *effective_settings*
    directly instead of reading it off a live `Store` -- see this
    module's own doc comment.
    """
    summary = {
        "model": effective_settings.get("model"),
        "adapter": effective_settings.get("adapter"),
        "plugins": effective_settings.get("plugins"),
    }
    text = json.dumps(redact(summary), indent=2, sort_keys=True)
    if len(text) > _CONTEXT_MAX_CHARS:
        text = text[:_CONTEXT_MAX_CHARS].rstrip() + "\n... (truncated)"
    return text


def context_case(effective_settings: dict) -> dict:
    return {
        "kind": "context",
        "effective_settings": effective_settings,
        "context": _context_from_settings(effective_settings),
    }


def context_cases() -> list[dict]:
    return [
        context_case({}),
        context_case({"model": "gpt-4", "adapter": "openai", "plugins": ["json", "shell"]}),
        context_case({"model": None, "adapter": None, "plugins": None}),
        context_case({"model": "caf\u00e9-\U0001f600", "adapter": "openai", "plugins": []}),
        context_case(
            {
                "model": "gpt-4",
                "adapter": "openai",
                "plugins": ["json"],
                "api_key": "sk-should-be-redacted",
            }
        ),
        context_case({"model": "m" * 3000, "adapter": "a", "plugins": []}),
    ]


def build_corpus() -> list[dict]:
    return clean_fences_cases() + extract_done_cases() + prime_cases() + context_cases()


def render(cases: list[dict]) -> str:
    return json.dumps(cases, indent=2, ensure_ascii=False) + "\n"


def main() -> int:
    text = render(build_corpus())
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    if "--check" in sys.argv[1:]:
        current = OUTPUT.read_text() if OUTPUT.exists() else ""
        if current != text:
            print(f"{OUTPUT} is stale; run without --check to regenerate", file=sys.stderr)
            return 1
        return 0
    OUTPUT.write_text(text)
    print(f"wrote {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
