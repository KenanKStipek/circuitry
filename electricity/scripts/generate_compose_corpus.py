#!/usr/bin/env python3
"""Generate electricity-vm's compose golden corpus from Circuitry's real
code: `core/templates.py::render_template`'s `escape` parameter (#397),
`core/prompt_compose.py::render_with_composition` (#396), and
`core/yield_effect.py::YieldRuntime.execute` (issue #487).

Three sections, one generator (the no-escape section is replayed against
`electricity-template` directly, the rest against `electricity-vm::compose`/
`exec::yield_`):

- ``escape_cases``: `render_template(template, ctx, escape=...)`, over the
  same templates with both `escape` values, so no-escape rendering's own
  additive seam is pinned against chevron's real behaviour.
- ``compose_cases``: `render_with_composition(template, ctx, declared=...,
  known_effect_names=..., label=..., escape=...)`, over a plain-dict `ctx`
  -- declared-prompt expansion, effect references, odd tags, context
  interplay, the shared synthetic-key counter, and the 8 MiB cap.
- ``yield_cases``: `YieldRuntime(...).execute(store=..., ctx=...)`, with
  the node's own two timestamps replaced by a fixed placeholder (they are
  never byte-compared, only checked for presence, by every consumer of
  this file) and the raised exception's own text/class recorded for a
  failure under `on_error: fail`.

Every case is synthetic, written directly against runtime-semantics.md
§3.1/§3.4/§3.6/§5.9 and the rulings on issue #487 -- nothing here is
copied from any real orchestration.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI, `electricity/scripts/generator-constraints.txt` pinned).
Usage: python3 generate_compose_corpus.py [--check]
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

from circuitry.core.prompt_compose import render_with_composition
from circuitry.core.store.store import Store
from circuitry.core.templates import TemplateError, render_template
from circuitry.core.tool import _json_aware_ctx
from circuitry.core.yield_effect import YieldDefinition, YieldRuntime

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-vm"
    / "tests"
    / "golden"
    / "compose_corpus.json"
)

#: Written in place of every real timestamp a yield case's own node
#: carries -- never byte-compared (every consumer of this file only
#: checks that `created_at`/`completed_at` are present, the same way
#: the conformance normalizer treats a timestamp), so a fixed
#: placeholder keeps the committed file stable across regenerations.
TS_PLACEHOLDER = "<ts>"


def _scrub_timestamps(node: dict[str, Any]) -> dict[str, Any]:
    scrubbed = dict(node)
    meta = scrubbed.get("meta")
    if isinstance(meta, dict):
        scrubbed_meta = dict(meta)
        for key in ("created_at", "completed_at"):
            if key in scrubbed_meta and scrubbed_meta[key] is not None:
                scrubbed_meta[key] = TS_PLACEHOLDER
        scrubbed["meta"] = scrubbed_meta
    return scrubbed


# ---------------------------------------------------------------------
# escape_cases: render_template(template, ctx, escape=...)
# ---------------------------------------------------------------------


def escape_case(template: str, ctx: Any, *, label: str = "template", escape: bool) -> dict:
    try:
        text = render_template(template, ctx, label=label, escape=escape)
    except TemplateError as exc:
        return {
            "template": template,
            "ctx": ctx,
            "label": label,
            "escape": escape,
            "ok": None,
            "err": str(exc),
        }
    return {
        "template": template,
        "ctx": ctx,
        "label": label,
        "escape": escape,
        "ok": text,
        "err": None,
    }


def build_escape_cases() -> list[dict]:
    pairs: list[tuple[str, Any]] = [
        ("{{x}}", {"x": "<b>&'\""}),
        ("{{{x}}}", {"x": "<b>&'\""}),
        ("{{&x}}", {"x": "<b>&'\""}),
        ("{{x}}", {"x": 0}),
        ("{{x}}", {"x": False}),
        ("{{l}}", {"l": [1, "a"]}),
        ("{{l}}", {"l": []}),
        ("{{d}}", {"d": {"a": 1}}),
        ("{{#a}}{{.}}{{/a}}", {"a": True, "marker": "OUTER"}),
        ("{{^a}}empty{{/a}}", {"a": False}),
        ("{{{.}}}", True),
        ("{{.}}", True),
        ("{{#items}}<{{.}}>{{/items}}", {"items": [1, "<x>", False]}),
    ]
    return [
        escape_case(template, ctx, escape=escape)
        for template, ctx in pairs
        for escape in (True, False)
    ]


# ---------------------------------------------------------------------
# compose_cases: render_with_composition(...)
# ---------------------------------------------------------------------


def compose_case(
    template: str,
    ctx: Any,
    *,
    declared: dict[str, str] | None = None,
    known: frozenset[str] = frozenset(),
    label: str = "template",
    escape: bool = True,
    json_aware: bool = False,
) -> dict:
    effective_ctx = _json_aware_ctx(ctx) if json_aware else ctx
    try:
        text = render_with_composition(
            template,
            effective_ctx,
            declared=declared,
            known_effect_names=known,
            label=label,
            escape=escape,
        )
    except TemplateError as exc:
        result: dict[str, Any] = {"ok": None, "err": str(exc)}
    else:
        result = {"ok": text, "err": None}
    return {
        "template": template,
        "ctx": ctx,
        "declared": dict(declared) if declared else {},
        "known_effect_names": sorted(known),
        "label": label,
        "escape": escape,
        "json_aware": json_aware,
        **result,
    }


def _effect(value: Any) -> dict[str, Any]:
    """A minimal `prime.<name>` node -- just enough for
    `_resolve_effect_text`'s own presence check and `.value` read."""
    return {"value": value, "meta": {}}


def build_compose_cases() -> list[dict]:
    cases: list[dict] = []

    # --- a template with no partial renders exactly like render_template ---
    cases.append(compose_case("hi {{name}}", {"name": "world"}))

    # --- declared prompts: nested, newline drop, side-by-side reuse ---
    cases.append(
        compose_case(
            "{{> voice}}",
            {"prime": {}},
            declared={"voice": "be {{tone}}", "tone": "unused"},
            label="template",
        )
    )
    cases.append(
        compose_case(
            "{{> outer}}",
            {"prime": {}, "tone": "brief"},
            declared={"outer": "[{{> inner}}]", "inner": "deep {{tone}}"},
        )
    )
    cases.append(
        compose_case("{{> voice}}after", {"prime": {}}, declared={"voice": "be brief\n"})
    )
    cases.append(
        compose_case("{{> voice}}after", {"prime": {}}, declared={"voice": "be brief\r\n"})
    )
    cases.append(
        compose_case("{{> voice}}after", {"prime": {}}, declared={"voice": "be brief\n\n"})
    )
    cases.append(
        compose_case("{{> voice}}{{> voice}}", {"prime": {}}, declared={"voice": "v"})
    )
    cases.append(
        compose_case(
            "{{#items}}{{> line}}{{/items}}",
            {"prime": {}, "items": ["a", "b"]},
            declared={"line": "<{{.}}>"},
        )
    )
    cases.append(
        compose_case(
            "{{> block}}",
            {"prime": {}, "a": True},
            declared={"block": "{{#a}}\nyes\n{{/a}}\ndone"},
        )
    )
    cases.append(
        compose_case(
            "{{> voice}}",
            {"prime": {}},
            declared={"voice": "before{{! a comment }}after"},
        )
    )
    cases.append(
        compose_case(
            "{{> voice}}",
            {"prime": {}},
            declared={"voice": "{{=<% %>=}}oops"},
        )
    )
    # Runtime cycle among declared prompts (never caught by a static
    # per-name check: each level names a *different* prompt that only
    # closes the cycle several levels down).
    cases.append(
        compose_case(
            "{{> a}}",
            {"prime": {}},
            declared={"a": "{{> b}}", "b": "{{> a}}"},
        )
    )
    cases.append(
        compose_case(
            "{{> a}}",
            {"prime": {}},
            declared={"a": "{{> a}}"},
        )
    )

    # --- effect references: present, None, known-not-run, unknown, dotted ---
    cases.append(
        compose_case("said: {{> reply}}", {"prime": {"reply": _effect("hello")}})
    )
    cases.append(compose_case("[{{> reply}}]", {"prime": {"reply": _effect(None)}}))
    cases.append(compose_case("[{{> later}}]", {"prime": {}}, known=frozenset({"later"})))
    cases.append(compose_case("{{> nope}}", {"prime": {}}))
    cases.append(
        compose_case(
            "{{> pipeline.outline}}",
            {"prime": {"pipeline": {"outline": _effect("plan")}}},
        )
    )
    cases.append(
        compose_case("{{> items}}", {"prime": {"items": _effect([1, "a", None, True])}})
    )
    cases.append(
        compose_case("{{> info}}", {"prime": {"info": _effect({"a": 1, "b": [2, 3]})}})
    )
    cases.append(compose_case("{{> n}}", {"prime": {"n": _effect(42)}}))
    cases.append(compose_case("{{> f}}", {"prime": {"f": _effect(1.5)}}))
    cases.append(compose_case("{{> b}}", {"prime": {"b": _effect(False)}}))
    cases.append(
        compose_case(
            "[{{> items}}]",
            {"prime": {"items": _effect([])}},
        )
    )
    cases.append(
        compose_case(
            "[{{> items}}]",
            {"prime": {"items": _effect({})}},
        )
    )

    # --- odd tags ---
    cases.append(compose_case("{{> }}", {"prime": {}}))
    cases.append(
        compose_case(
            "{{>  reply  }}", {"prime": {"reply": _effect("hi")}}
        )
    )
    cases.append(compose_case("{{> 1bad}}", {"prime": {}}))
    cases.append(compose_case("{{{>x}}}", {">x": "literal"}))
    cases.append(compose_case("{{>a}b}}", {"prime": {}}))

    # --- context interplay ---
    cases.append(
        compose_case(
            "{{=<% %>=}}[{{> later}}]",
            {"prime": {}},
            known=frozenset({"later"}),
        )
    )
    cases.append(
        compose_case(
            "{{> reply}}{{#prime}}seen{{/prime}}",
            {"prime": {"reply": _effect("hi")}},
        )
    )
    cases.append(
        compose_case(
            "{{> reply}}-{{> reply}}",
            {"prime": {"reply": _effect("hi")}},
        )
    )

    # --- shared counter: two sibling declared prompts each nesting a
    # different effect reference (C33's shape) ---
    cases.append(
        compose_case(
            "{{> a}}{{> b}}",
            {"prime": {"x": _effect("X"), "y": _effect("Y")}},
            declared={"a": "<{{> x}}>", "b": "<{{> y}}>"},
        )
    )
    cases.append(
        compose_case(
            "{{> b}} {{> a}}",
            {"prime": {"x": _effect("X"), "y": _effect("Y")}},
            declared={"a": "<{{> x}}>", "b": "<{{> y}}>"},
        )
    )

    # --- errors at each nesting level ---
    cases.append(
        compose_case(
            "{{> outer}}",
            {"prime": {}},
            declared={"outer": "{{> 1bad}}"},
        )
    )
    cases.append(
        compose_case(
            "{{> outer}}",
            {"prime": {}},
            declared={"outer": "{{> inner}}", "inner": "{{=<% %>=}}oops"},
        )
    )

    # --- caps: an exponential declared-prompt chain over the cap ---
    exponential_declared = {"p0": "x" * 1024}
    for level in range(1, 25):
        exponential_declared[f"p{level}"] = (
            f"{{{{> p{level - 1}}}}}{{{{> p{level - 1}}}}}"
        )
    cases.append(compose_case("{{> p24}}", {"prime": {}}, declared=exponential_declared))

    # --- double counting: a fragment nested two levels deep is charged
    # at both levels (small sizes, well under the cap, pinning the
    # mechanism rather than the exact numeric boundary) ---
    cases.append(
        compose_case(
            "{{> a}}",
            {"prime": {}},
            declared={"a": "{{> b}}", "b": "{{> c}}", "c": "leaf"},
        )
    )

    # --- params_json's own JsonAwareCtx: an effect's list/dict value
    # serializes as real JSON, not py_repr, and an empty one is never
    # collapsed to "" ---
    cases.append(
        compose_case(
            "{{> symbols}}",
            {"prime": {"symbols": _effect(["AAPL", "MSFT"])}},
            label="params_json",
            json_aware=True,
        )
    )
    cases.append(
        compose_case(
            "[{{> items}}]",
            {"prime": {"items": _effect([])}},
            label="params_json",
            json_aware=True,
        )
    )

    return cases


# ---------------------------------------------------------------------
# yield_cases: YieldRuntime(...).execute(store=..., ctx=...)
# ---------------------------------------------------------------------


def yield_case(
    name: str,
    template: str,
    *,
    inputs: dict[str, Any] | None = None,
    on_error: str = "fail",
    declared: dict[str, str] | None = None,
    known: frozenset[str] = frozenset(),
    extra_ctx: dict[str, Any] | None = None,
    pre_value: Any = "__absent__",
) -> dict:
    root_state: dict[str, Any] = {"prime": {}}
    if extra_ctx:
        root_state.update(extra_ctx)
    root_store = Store(state=root_state)
    prime_store = root_store.child("prime")
    if pre_value != "__absent__":
        prime_store.state[name] = {"value": pre_value, "meta": {}}
    runtime_config: dict[str, Any] = {}
    if declared:
        runtime_config["_prompts"] = declared
    if known:
        runtime_config["_prompt_effect_names"] = frozenset(known)
    defn = YieldDefinition(name=name, template=template, inputs=inputs, on_error=on_error)
    raised: dict[str, Any] | None = None
    try:
        YieldRuntime(defn, runtime_config=runtime_config).execute(
            store=prime_store, ctx=root_state
        )
    except Exception as exc:  # the only way `execute` ever raises: on_error == "fail"
        raised = {"class": type(exc).__name__, "message": str(exc)}
    node = _scrub_timestamps(prime_store.state[name])
    return {
        "name": name,
        "template": template,
        "inputs": inputs,
        "on_error": on_error,
        "declared": declared or {},
        "known_effect_names": sorted(known),
        "extra_ctx": extra_ctx or {},
        "pre_value": None if pre_value == "__absent__" else pre_value,
        "node": node,
        "raised": raised,
    }


def build_yield_cases() -> list[dict]:
    cases: list[dict] = []

    cases.append(yield_case("brief", "Topic: {{input.topic}}", extra_ctx={"input": {"topic": "circuitry"}}))

    # render failure, under fail / skip / continue
    cases.extend(
        yield_case(
            "broken",
            "{{input.n.0}}",
            on_error=on_error,
            extra_ctx={"input": {"n": 5}},
        )
        for on_error in ("fail", "skip", "continue")
    )

    # raw inputs shadowing a top-level key, never rendered
    cases.append(
        yield_case(
            "say",
            "{{t}} / {{input}}",
            inputs={"t": "{{input.x}}", "input": "shadowed"},
            extra_ctx={"input": "real"},
        )
    )

    # self-reference: fresh node (None -> "") and a reused node from an
    # earlier pass (its own previous value).
    cases.append(yield_case("self", "[{{> self}}]"))
    cases.append(yield_case("self", "[{{> self}}]", pre_value="previous"))

    return cases


def build_corpus() -> dict:
    return {
        "escape_cases": build_escape_cases(),
        "compose_cases": build_compose_cases(),
        "yield_cases": build_yield_cases(),
    }


def render(corpus: dict) -> str:
    return json.dumps(corpus, indent=2, ensure_ascii=False, sort_keys=True) + "\n"


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
