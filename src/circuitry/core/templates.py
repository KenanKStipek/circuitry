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

from typing import Any

import chevron  # type: ignore[import-untyped]
from chevron.tokenizer import ChevronError  # type: ignore[import-untyped]

__all__ = ["TemplateError", "render_template", "template_syntax_error"]


class TemplateError(ValueError):
    """A Mustache template could not be parsed or rendered."""


def _describe(exc: Exception) -> str:
    # chevron's messages span several lines ("Trying to close tag ...\nlast
    # open tag is ..."); one line reads better inside a list of errors.
    return " ".join(str(exc).split()) or type(exc).__name__


def _reject_partials(template: str) -> None:
    """Raise :class:`ChevronError` if *template* contains a partial tag.

    Checked with chevron's own tokenizer (token type ``"partial"``), not a
    regex, so ``{{{ }}}`` and ``{{& }}`` (both tokenize as ``"no escape"``)
    are unaffected — only ``{{> name}}`` is a partial. Partials are not
    supported: a template is rendered with chevron's defaults, so ``{{>
    name}}`` would read ``name.mustache`` from the process's working
    directory.
    """
    for token_type, value in chevron.tokenizer.tokenize(template):
        if token_type == "partial":
            raise ChevronError(f"partials are not supported: {{{{> {value}}}}}")


def template_syntax_error(template: str) -> str | None:
    """Why *template* is not valid Mustache, or ``None`` when it parses."""
    try:
        _reject_partials(template)
    except Exception as exc:  # chevron raises ChevronError, and IndexError on '{{}}'
        return _describe(exc)
    return None


def render_template(template: str, ctx: Any, *, label: str = "template") -> str:
    """Render *template* against *ctx*; raise :class:`TemplateError` if it cannot.

    Fails before rendering — and never touches the file system — for a
    template containing a partial tag, rather than handing it to chevron:
    this is the only render path (:func:`chevron.render` is never called
    elsewhere in this codebase), so it covers templates that exist only at
    run time — generated reflector/decompose plans, ``use: inline``
    children — not just ones ``cof check`` sees statically. ``partials_dict``
    and ``partials_path`` are also pinned so a partial tag reintroduced by a
    future chevron version still can't reach the file system.
    """
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
