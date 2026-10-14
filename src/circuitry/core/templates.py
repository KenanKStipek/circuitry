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
import datetime
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


#: The only ``getattr`` fallback :func:`_get_key` resolves — a
#: ``datetime.date``/``datetime.datetime``'s own data fields — keyed by
#: the narrowest type each applies to. Everything else a ``getattr`` call
#: could reach (a method, ``int``/``float``'s ``real``/``imag``/
#: ``numerator``/``denominator``, ``tzinfo``/``fold``/``min``/``max``/
#: ``resolution``, any dunder) is a miss, exactly like an
#: ``AttributeError`` would be.
_DATE_ATTRS = frozenset({"year", "month", "day"})
_DATETIME_ATTRS = _DATE_ATTRS | frozenset({"hour", "minute", "second", "microsecond"})


def _get_key(key: str, scopes: list[Any], warn: bool = False) -> Any:
    """chevron's own ``_get_key`` (``renderer.py``), with one change: a
    ``getattr`` hit resolves only a ``datetime.date``/``datetime.datetime``
    value's own ``year``/``month``/``day``/``hour``/``minute``/``second``/
    ``microsecond`` fields; every other attribute — a method, a dunder, any
    other type's own attributes — is treated as a miss, exactly like an
    ``AttributeError`` would be, instead of being returned.

    Mustache has no concept of a method call; a tag whose name happens to
    match one of its scope's methods (``title`` inside ``{{#title}}``,
    ``count``/``format``/... on any string) means the value in scope, not
    a description of that method. Vendored rather than patched upstream
    (#389) so a miss still falls through to the next scope the same way
    any other missing key does, and the one named exception — a date or
    datetime's own calendar fields, ``{{d.year}}`` — keeps resolving
    exactly as before.
    """
    if key == ".":
        return scopes[0]

    for outer_scope in scopes:
        current = outer_scope
        try:
            for child in key.split("."):
                try:
                    current = current[child]
                except (TypeError, AttributeError):
                    try:
                        attr = getattr(current, child)
                    except (TypeError, AttributeError):
                        current = current[int(child)]
                    else:
                        if isinstance(current, datetime.datetime):
                            allowed = child in _DATETIME_ATTRS
                        elif isinstance(current, datetime.date):
                            allowed = child in _DATE_ATTRS
                        else:
                            allowed = False
                        current = attr if allowed else current[int(child)]

            if current in (0, False):
                return current

            try:
                if current._CHEVRON_return_scope_when_falsy:
                    return current
            except AttributeError:
                return current or ""
        except (AttributeError, KeyError, IndexError, ValueError):
            pass

    if warn:
        import sys
        from os import linesep

        sys.stderr.write(f"Could not find key '{key}'{linesep}")

    return ""


chevron.renderer._get_key = _get_key


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
