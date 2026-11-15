#!/usr/bin/env python3
"""Generate the golden corpus for lane L1 of issue #448 (electricity's
`reflector` effect, issue #506) from Circuitry's own real
`core/reflector.py` functions.

These helpers are pure enough to call directly against synthetic input,
with no real `Store` needed:

- `_clean_yaml_fences`/`_extract_done_flag`: fences, `---`, every
  `str.splitlines()` separator, `steps`/`plan` dict/list, a bare list,
  a scalar, invalid YAML (syntax and a calendar-invalid timestamp
  alike), `done` truthiness. `_extract_done_flag` hands back a parsed
  document, not YAML text (issue #778) -- recorded here as JSON.
- `_render_reflector_prime`: Mustache tags (`{{goal}}`/`{{context}}`/
  `{{max_effects}}`), no HTML escaping, a literal single brace left
  untouched, an unknown field, a malformed template and a partial tag
  (both refused -- there is no fallback to the raw template).
- `_best_effort_context`/`_best_effort_goal`, called directly (PR
  #510's first review: an earlier draft of this script
  re-implemented `_best_effort_context`'s own redact/`json.dumps`/
  truncate body instead of calling it) -- both only ever read
  `_run_root_state(store)`, and that function's own `getattr(store,
  "true_root_state", None)` check is satisfied by a bare
  `types.SimpleNamespace(true_root_state={...})`, with no real `Store`
  construction needed at all.

Must be run with Python 3.11 and the pinned dependencies
(`generator-constraints.txt`): PyYAML's own `safe_load` output, and
`json.dumps`'s own escaping, are what several of these cases pin.

Usage: python3 generate_reflector_corpus.py [--check]
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from types import SimpleNamespace

from circuitry.core.reflector import (
    _best_effort_context,
    _best_effort_goal,
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
        done, parsed = _extract_done_flag(text)
        return {
            "kind": "extract_done",
            "input": text,
            "ok": True,
            "done": done,
            "parsed": parsed,
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
        "a\x0bb",
        "a\x0cb",
        "a\rb",
        "a\r\nb",
        "a\x1cb",
        "a\x1db",
        "a\x1eb",
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
        "effects: [\n",  # unparseable YAML (syntax)
        "done: false\nsteps: not-a-list\neffects: []\n",
        "done: false\nplan: not-a-dict-or-list\n",
        # Mixed key types -- `_extract_done_flag` never sorts (#778: no
        # `yaml.dump` round trip left to hit a sort at all), so this
        # parses and normalizes exactly like any other plan; mixed-type
        # key sorting itself is `_canonical_json_digest`'s own concern,
        # exercised by the reflector conformance cases instead.
        "1: a\nb: c\neffects: []\n",
        # A plan whose own top-level keys are not in sorted order --
        # issue #778: no round trip through `yaml.dump` means the
        # parsed document keeps exactly this order, `done` included
        # (dropped only, never used to resort the rest).
        "zeta: 1\neffects:\n  - type: prompt\n    name: a\n    template: hi\nalpha: 2\ndone: false\n",
        "```yaml\ndone: false\neffects:\n  - type: tool\n    name: x\n    provider: json\n```\n",
        # Issue #528's own comment: a calendar-invalid timestamp
        # (`when: 2024-02-30`) is PyYAML's own bare, uncaught
        # `ValueError` -- propagates the same way an ordinary YAML
        # syntax error now does (issue #778: no "let use(inline) handle
        # it" swallow left to distinguish them), so this is `ok: False`
        # like the unparseable-YAML case above.
        "done: false\neffects: []\nwhen: 2024-02-30\n",
    ]
    return [extract_done_case(t) for t in texts]


# ---------------------------------------------------------------------
# _render_reflector_prime
# ---------------------------------------------------------------------


def prime_case(
    template: str, *, max_effects: int = 8, goal: str = "g", context: str = "c"
) -> dict:
    try:
        rendered = _render_reflector_prime(
            prime_template=template, max_effects=max_effects, goal=goal, context=context
        )
        return {
            "kind": "prime",
            "template": template,
            "max_effects": max_effects,
            "goal": goal,
            "context": context,
            "ok": True,
            "rendered": rendered,
        }
    except Exception as exc:  # no fallback to the raw template (issue #778)
        return {
            "kind": "prime",
            "template": template,
            "max_effects": max_effects,
            "goal": goal,
            "context": context,
            "ok": False,
            "error": str(exc),
        }


def prime_cases() -> list[dict]:
    return [
        prime_case("Goal: {{goal}}\nContext: {{context}}\nMax: {{max_effects}}"),
        prime_case("Unknown field: {{nope}}"),
        prime_case("Literal braces: {not a field}"),
        prime_case("", max_effects=3, goal="", context=""),
        # No HTML escaping (#397/#778: a prompt template, like any
        # other) -- `&`/`<`/`>`/quotes survive verbatim.
        prime_case("Goal: {{goal}}", goal="Ben & Jerry's <ice> \"cream\""),
        prime_case("Non-ascii: {{goal}}", goal="caf\u00e9 \U0001f600"),
        # A malformed Mustache template -- refused, never a raw-text
        # fallback (a document error at check/compile time for a real
        # orchestration; this generator calls the renderer directly).
        prime_case("Unclosed section: {{#bad}}"),
        # A partial tag -- refused the same way any other template
        # rejects one (`core.templates.render_template`).
        prime_case("Partial: {{> other}}"),
    ]


# ---------------------------------------------------------------------
# _best_effort_context, called directly against a SimpleNamespace
# ---------------------------------------------------------------------


def context_case(effective_settings: dict) -> dict:
    store = SimpleNamespace(
        true_root_state={"runtime": {"effective_settings": effective_settings}}
    )
    return {
        "kind": "context",
        "effective_settings": effective_settings,
        "context": _best_effort_context(store),
    }


def context_cases() -> list[dict]:
    return [
        context_case({}),
        context_case({"model": "gpt-4", "adapter": "openai", "plugins": ["json", "shell"]}),
        context_case({"model": None, "adapter": None, "plugins": None}),
        context_case({"model": "caf\u00e9-\U0001f600", "adapter": "openai", "plugins": []}),
        # A secret-shaped *value* inside one of the three fields
        # `_best_effort_context`'s own summary keeps (`model`/`adapter`/
        # `plugins`) -- PR #510's first review: an
        # earlier draft put `api_key` as a sibling top-level key, which
        # the summary-building step drops before redaction ever runs,
        # so it never actually exercised `redact()` at all.
        context_case(
            {
                "model": "sk-" + "a" * 32,
                "adapter": "openai",
                "plugins": ["json"],
            }
        ),
        context_case({"model": "m" * 3000, "adapter": "a", "plugins": []}),
    ]


# ---------------------------------------------------------------------
# _best_effort_goal, called directly against a SimpleNamespace
# ---------------------------------------------------------------------


def goal_case(prime: dict) -> dict:
    store = SimpleNamespace(true_root_state={"prime": prime})
    return {
        "kind": "goal",
        "prime": prime,
        "goal": _best_effort_goal(store),
    }


def goal_cases() -> list[dict]:
    return [
        goal_case({}),
        goal_case({"goal": {"value": "  Summarize the project.  "}}),
        goal_case({"goal": {"value": ""}}),
        goal_case({"goal": {"value": "   "}}),
        goal_case({"goal": {"value": 5}}),
        goal_case({"goal": "not a dict"}),
    ]


def build_corpus() -> list[dict]:
    return (
        clean_fences_cases()
        + extract_done_cases()
        + prime_cases()
        + context_cases()
        + goal_cases()
    )


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
