#!/usr/bin/env python3
"""Generate electricity-template's golden corpus from Circuitry's real
chevron port (`core/templates.py::render_template`/`template_syntax_error`,
`core/tool.py::_json_aware_ctx`).

Two kinds of cases, since `template_syntax_error` doesn't take a context at
all:

- ``syntax_cases``: a template alone, and the syntax-error description
  `template_syntax_error` returns for it (or ``null`` if it's valid Mustache).
- ``render_cases``: a template, a context, a label, and whether the context
  is wrapped through `_json_aware_ctx` first (DESIGN.md §3.3's
  `params_json`-only splice serializer, Quirk Q2) -- paired with either the
  real rendered text or `str(TemplateError)`.

Every case is synthetic, written directly against the rules in
`electricity/DESIGN.md` §3.3 and `electricity/docs/spec/runtime-semantics.md`
§3 (plus chevron's own confirmed quirks -- the `.`-plus-`True` scope-stack
bug, falsy list elements suppressing their whole iteration including
literal text) -- nothing here is copied from any real orchestration.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI). Usage: python3 generate_template_corpus.py [--check]
"""

from __future__ import annotations

import datetime
import json
import struct
import sys
from pathlib import Path

from circuitry.core.templates import TemplateError, render_template, template_syntax_error
from circuitry.core.tool import _json_aware_ctx

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-template"
    / "tests"
    / "golden"
    / "corpus.json"
)


# ---------------------------------------------------------------------
# Tagged value encoding (matches generate_value_corpus.py's, so the two
# crates' golden-corpus tests can share one decoder shape)
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
    if isinstance(value, (bytes, bytearray)):
        return {"t": "bytes", "v": bytes(value).hex()}
    if isinstance(value, list):
        return {"t": "list", "v": [encode(item) for item in value]}
    if isinstance(value, dict):
        return {"t": "dict", "v": [[encode(k), encode(v)] for k, v in value.items()]}
    if isinstance(value, datetime.datetime):
        naive = value.replace(tzinfo=None)
        offset = value.utcoffset()
        return {
            "t": "datetime",
            "naive": naive.isoformat(timespec="microseconds"),
            "offset_seconds": None if offset is None else round(offset.total_seconds()),
        }
    if isinstance(value, datetime.date):
        return {"t": "date", "v": value.isoformat()}
    raise TypeError(f"no tagged encoding for {type(value)!r}")


# ---------------------------------------------------------------------
# Syntax cases: template_syntax_error(template)
# ---------------------------------------------------------------------


def syntax_templates() -> list[str]:
    return [
        "no tags here",
        "{{name}}",
        "{{{name}}}",
        "{{&name}}",
        "{{#section}}x{{/section}}",
        "{{^section}}x{{/section}}",
        "{{! a comment }}x",
        "{{=<% %>=}}<%a%><%={{ }}=%>{{b}}",
        "{{a",  # unclosed tag
        "{{#a}}x",  # unclosed section, EOF
        "{{#a}}{{#b}}x{{/b}}",  # unclosed section, EOF (nested)
        "{{/a}}",  # unopened close
        "{{#a}}{{/b}}",  # mismatched close
        "{{#a}}{{#b}}{{/a}}{{/b}}",  # mismatched close (nested, wrong order)
        "{{=a}}",  # unclosed set-delimiter tag (no trailing '=')
        "{{}}",  # empty tag -- IndexError in chevron, not ChevronError
        "{{> partial}}",
        "{{#a}}{{> p}}{{/a}}",
        "text\n{{#a}}\nbody\n{{/a}}\ntail",  # standalone trimming, still valid
        "{{a.b.c}}",
        "{{items.0}}",
        "a{{! standalone? }}\nb",
    ]


def build_syntax_cases() -> list[dict]:
    return [
        {"template": t, "syntax_error": template_syntax_error(t)} for t in syntax_templates()
    ]


# ---------------------------------------------------------------------
# Render cases: render_template(template, ctx, label=label)
# ---------------------------------------------------------------------


def render_case(template: str, ctx: object, *, label: str = "template", json_aware: bool = False) -> dict:
    effective_ctx = _json_aware_ctx(ctx) if json_aware else ctx
    try:
        text = render_template(template, effective_ctx, label=label)
    except TemplateError as exc:
        return {
            "template": template,
            "ctx": encode(ctx),
            "label": label,
            "json_aware": json_aware,
            "render_ok": None,
            "render_err": str(exc),
        }
    else:
        return {
            "template": template,
            "ctx": encode(ctx),
            "label": label,
            "json_aware": json_aware,
            "render_ok": text,
            "render_err": None,
        }


def build_render_cases() -> list[dict]:
    cases: list[dict] = []

    # --- escaping and scalar rendering (runtime-semantics §3.1/§3.2) ---
    cases.append(render_case("{{s}}", {"s": "<b>&'\""}))
    cases.append(render_case("{{{s}}}", {"s": "<b>&'\""}))
    cases.append(render_case("{{&s}}", {"s": "<b>&'\""}))
    cases.append(render_case("{{t}}", {"t": True}))
    cases.append(render_case("{{t}}", {"t": False}))
    cases.append(render_case("{{n}}", {"n": None}))
    cases.append(render_case("{{i}}", {"i": 42}))
    cases.append(render_case("{{z}}", {"z": 0}))
    cases.append(render_case("{{f}}", {"f": 1.0}))
    cases.append(render_case("{{f}}", {"f": 0.0}))
    cases.append(render_case("{{f}}", {"f": -0.0}))
    cases.append(render_case("{{f}}", {"f": 1e-05}))
    cases.append(render_case("{{l}}", {"l": [1, 2, 3]}))
    cases.append(render_case("{{l}}", {"l": []}))
    cases.append(render_case("{{d}}", {"d": {"a": 1}}))
    cases.append(render_case("{{d}}", {"d": {}}))
    cases.append(render_case("{{l}}", {"l": [1, "a", None, True]}))

    # --- dotted names, list indexing, missing keys ---
    cases.append(render_case("[{{missing}}]", {}))
    cases.append(render_case("{{a.b.c}}", {"a": {"b": {}}}))
    cases.append(render_case("{{a.b.c}}", {"a": {"b": {"c": "deep"}}}))
    cases.append(render_case("{{items.0}}", {"items": ["first", "second"]}))
    cases.append(render_case("{{items.-1}}", {"items": ["first", "second"]}))
    cases.append(render_case("{{items.5}}", {"items": ["first", "second"]}))
    cases.append(render_case("{{d.1}}", {"d": {1: "int-key"}}))

    # --- sections over lists ---
    cases.append(render_case("{{#items}}[{{.}}]{{/items}}", {"items": [1, 2, 3]}))
    cases.append(render_case("{{#items}}[{{.}}]{{/items}}", {"items": []}))
    cases.append(
        render_case(
            "{{#items}}<{{.}}>{{/items}}",
            {"items": [1, False, 0, "x", None, "", [], {}]},
        )
    )
    cases.append(
        render_case(
            "{{#items}}{{name}}-{{/items}}",
            {"items": [{"name": "a"}, {"name": "b"}]},
        )
    )
    cases.append(
        render_case(
            "{{#items}}{{outer}}{{/items}}",
            {"items": [{"name": "a"}], "outer": "OUT"},
        )
    )

    # --- sections over non-list truthy/falsy values ---
    cases.append(render_case("{{#x}}yes{{/x}}", {"x": True}))
    cases.append(render_case("{{#x}}yes{{/x}}", {"x": False}))
    cases.append(render_case("{{#x}}yes{{/x}}", {"x": 0}))
    cases.append(render_case("{{#x}}{{field}}{{/x}}", {"x": {"field": "v"}}))
    cases.append(render_case("{{#x}}{{field}}{{/x}}", {"x": {}}))

    # --- inverted sections ---
    cases.append(render_case("{{^x}}empty{{/x}}", {"x": False}))
    cases.append(render_case("{{^x}}empty{{/x}}", {"x": True}))
    cases.append(render_case("{{^x}}empty{{/x}}", {"x": 0}))
    cases.append(render_case("{{^x}}empty{{/x}}", {"x": []}))
    cases.append(render_case("{{^x}}empty{{/x}}", {}))

    # --- nested sections ---
    cases.append(
        render_case(
            "{{#outer}}{{#inner}}{{.}}{{/inner}}{{/outer}}",
            {"outer": [{"inner": [1, 2]}, {"inner": []}]},
        )
    )
    cases.append(
        render_case(
            "{{#a}}{{^b}}A-not-B{{/b}}{{/a}}",
            {"a": True, "b": False},
        )
    )

    # --- standalone-line whitespace trimming ---
    cases.append(render_case("text\n{{#a}}\nbody\n{{/a}}\ntail", {"a": True}))
    cases.append(render_case("text\n{{#a}}\nbody\n{{/a}}\ntail", {"a": False}))
    cases.append(render_case("  {{! comment }}\nafter", {}))

    # --- custom delimiters ---
    cases.append(render_case("{{=<% %>=}}<%a%>", {"a": "custom"}))

    # --- the `.`-plus-`True` chevron bug ---
    cases.append(render_case("{{#flag}}{{.}}{{/flag}}", {"flag": True, "marker": "OUTER"}))
    cases.append(render_case("{{{.}}}", True, label="template"))
    cases.append(render_case("{{.}}", True, label="template"))  # raises: no outer scope
    cases.append(render_case("{{#items}}[{{.}}]{{/items}}", {"items": [True]}))

    # --- labels (prefix in the wrapped error / unused on success) ---
    cases.append(render_case("{{x}}", {"x": "ok"}, label="messages[0].content"))
    cases.append(render_case("{{a", {}, label="messages[0].content"))

    # --- JsonAwareCtx: the params_json splice serializer ---
    cases.append(render_case("{{x}}", {"x": [1, 2, 3]}, label="params_json", json_aware=True))
    cases.append(render_case("{{x}}", {"x": []}, label="params_json", json_aware=True))
    cases.append(render_case("{{x}}", {"x": {}}, label="params_json", json_aware=True))
    cases.append(render_case("{{x}}", {"x": {"a": 1}}, label="params_json", json_aware=True))
    cases.append(
        render_case(
            "{{x}}", {"x": [1, "a", None, True, False]}, label="params_json", json_aware=True
        )
    )
    cases.append(render_case("[{{x}}]", {"x": []}, label="template", json_aware=False))

    # --- bytes / date / datetime scalars ---
    cases.append(render_case("{{b}}", {"b": bytes([0, 255, 97, 98, 99])}))
    cases.append(render_case("{{b}}", {"b": b""}))
    cases.append(render_case("{{d}}", {"d": datetime.date(2020, 1, 2)}))
    utc_dt = datetime.datetime(2020, 1, 2, 3, 4, 5, tzinfo=datetime.timezone.utc)
    cases.append(render_case("{{dt}}", {"dt": utc_dt}))

    # --- more section/inverted nesting and nested falsy-list suppression ---
    cases.append(
        render_case(
            "{{#a}}{{#b}}in{{/b}}out{{/a}}",
            {"a": [{"b": True}, {"b": False}]},
        )
    )
    cases.append(
        render_case(
            "{{#items}}{{^flag}}no-{{/flag}}{{.}}{{/items}}",
            {"items": [1, 2]},
        )
    )
    cases.append(render_case("{{#a}}{{! nested comment }}body{{/a}}", {"a": True}))

    # --- multiple delimiter switches in one template ---
    cases.append(
        render_case(
            "{{=<< >>=}}<<a>><<=[[ ]]=>>[[b]][[={{ }}=]]{{c}}",
            {"a": "A", "b": "B", "c": "C"},
        )
    )

    # --- negative / out-of-range list indices ---
    cases.append(render_case("{{items.-2}}", {"items": ["a", "b", "c"]}))
    cases.append(render_case("{{items.-99}}", {"items": ["a", "b", "c"]}))

    # --- unicode content through escaping ---
    unicode_sample = "caf\u00e9 \u4e2d\u6587 \U0001f389 <ok>"
    cases.append(render_case("{{s}}", {"s": unicode_sample}))

    # --- whitespace-only standalone lines around multiple siblings ---
    cases.append(
        render_case(
            "{{#items}}\n- {{.}}\n{{/items}}\ndone",
            {"items": ["a", "b"]},
        )
    )

    return cases


def build_corpus() -> dict:
    return {
        "syntax_cases": build_syntax_cases(),
        "render_cases": build_render_cases(),
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
