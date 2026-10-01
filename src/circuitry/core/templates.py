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


def template_syntax_error(template: str) -> str | None:
    """Why *template* is not valid Mustache, or ``None`` when it parses."""
    try:
        for _ in chevron.tokenizer.tokenize(template):
            pass
    except Exception as exc:  # chevron raises ChevronError, and IndexError on '{{}}'
        return _describe(exc)
    return None


def render_template(template: str, ctx: Any, *, label: str = "template") -> str:
    """Render *template* against *ctx*; raise :class:`TemplateError` if it cannot."""
    try:
        return str(chevron.render(template, ctx))
    except ChevronError as exc:
        raise TemplateError(
            f"{label}: malformed Mustache template: {_describe(exc)}"
        ) from exc
    except Exception as exc:
        raise TemplateError(f"{label}: could not render: {_describe(exc)}") from exc
