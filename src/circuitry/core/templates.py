"""Mustache syntax checks and rendering that fails loudly.

A malformed template — an unclosed ``{{tag}``, a section closed under the
wrong name — is an authoring error. The compiler tokenizes every template an
orchestration renders (:func:`template_syntax_error`), so ``cof check`` and
``cof run`` reject it before anything executes; :func:`render_template`
raises :class:`TemplateError` instead of handing back the raw text, so a
template that still fails at run time goes through the effect's
``on_error`` like any other failure.
"""

from __future__ import annotations

import contextvars
from typing import Any

import chevron  # type: ignore[import-untyped]
import chevron.renderer  # type: ignore[import-untyped]
from chevron.tokenizer import ChevronError  # type: ignore[import-untyped]

__all__ = ["TemplateError", "render_template", "template_syntax_error"]

#: Per-call, per-thread/task override for ``render_template(..., escape=False)``
#: (#397) — read by :func:`_escape_aware`, which replaces
#: ``chevron.renderer._html_escape`` below. A plain module-level flag would
#: race across the real OS threads a tree-flow/loop dynamic dispatches
#: effects on; a ``ContextVar`` isolates each one (a new thread starts with
#: the default, unaffected by a sibling's in-flight render), and ``set``/
#: ``reset`` nest correctly for a render that itself triggers a render (a
#: declared prompt's own ``{{> name}}``).
_NO_ESCAPE = contextvars.ContextVar("circuitry_template_no_escape", default=False)

#: chevron's own escaper, before the module-level name below is replaced.
_html_escape = chevron.renderer._html_escape


def _escape_aware(string: str) -> str:
    if _NO_ESCAPE.get():
        return string
    return _html_escape(string)


# chevron's renderer calls ``_html_escape(thing)`` as a bare module-global
# name at render time (not a bound default argument), so replacing the name
# in its module namespace is enough to redirect every call through here —
# see the module docstring of this file for why ``render_template`` is the
# only place chevron is ever invoked.
chevron.renderer._html_escape = _escape_aware


class TemplateError(ValueError):
    """A Mustache template could not be parsed or rendered."""


def _describe(exc: Exception) -> str:
    # chevron's messages span several lines ("Trying to close tag ...\nlast
    # open tag is ..."); one line reads better inside a list of errors.
    return " ".join(str(exc).split()) or type(exc).__name__


def _reject_partials(template: str, *, allow_partials: bool = False) -> None:
    """Raise :class:`ChevronError` if *template* contains a partial tag.

    Checked with chevron's own tokenizer (token type ``"partial"``), not a
    regex, so ``{{{ }}}`` and ``{{& }}`` (both tokenize as ``"no escape"``)
    are unaffected — only ``{{> name}}`` is a partial. By default partials
    are not supported: a template is rendered with chevron's defaults, so
    ``{{> name}}`` would read ``name.mustache`` from the process's working
    directory. ``allow_partials=True`` — used only by the compile-time
    syntax check (:func:`template_syntax_error`) on the exact fields
    ``{{> name}}`` composition reaches (``core.prompt_compose``) — lets a
    well-formed partial tag pass this *syntax* gate; whether the name it
    carries actually resolves is a separate, later check
    (``core.prompt_compose.check_prompt_composition``), and
    :func:`render_template` itself still always rejects one, unconditionally
    (#396 expands every ``{{> name}}`` before a template ever reaches it).
    """
    for token_type, value in chevron.tokenizer.tokenize(template):
        if token_type == "partial" and not allow_partials:
            raise ChevronError(f"partials are not supported: {{{{> {value}}}}}")


def template_syntax_error(template: str, *, allow_partials: bool = False) -> str | None:
    """Why *template* is not valid Mustache, or ``None`` when it parses."""
    try:
        _reject_partials(template, allow_partials=allow_partials)
    except Exception as exc:  # chevron raises ChevronError, and IndexError on '{{}}'
        return _describe(exc)
    return None


def render_template(
    template: str, ctx: Any, *, label: str = "template", escape: bool = True
) -> str:
    """Render *template* against *ctx*; raise :class:`TemplateError` if it cannot.

    Fails before rendering — and never touches the file system — for a
    template containing a partial tag, rather than handing it to chevron:
    this is the only render path (:func:`chevron.render` is never called
    elsewhere in this codebase), so it covers templates that exist only at
    run time — generated reflector/decompose plans, ``use: inline``
    children — not just ones ``cof check`` sees statically. ``partials_dict``
    and ``partials_path`` are also pinned so a partial tag reintroduced by a
    future chevron version still can't reach the file system.

    *escape* defaults to ``True`` — chevron's own default, so every existing
    call site is unaffected. ``escape=False`` makes every ``{{x}}`` in this
    one render insert its value exactly as ``{{{x}}}``/``{{&x}}`` already do
    (#397): prompt text (a ``prompt``/``yield`` effect's own template and
    messages, a declared prompt, a prompt file) is not HTML, so escaping it
    only corrupts quoted text, code, JSON and XML-style tags a prompt commonly
    carries. Every other template — tool params, ``params_json``, ``use``
    inputs/``inline``, asset refs — keeps the default.
    """
    token = _NO_ESCAPE.set(not escape)
    try:
        _reject_partials(template)
        return str(
            chevron.render(template, ctx, partials_dict={}, partials_path=None)
        )
    except ChevronError as exc:
        raise TemplateError(
            f"{label}: malformed Mustache template: {_describe(exc)}"
        ) from exc
    except Exception as exc:
        raise TemplateError(f"{label}: could not render: {_describe(exc)}") from exc
    finally:
        _NO_ESCAPE.reset(token)
