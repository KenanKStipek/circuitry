"""A run's label: the document's own ``label:`` template, or ``--label`` (#752).

``label:`` is a top-level Mustache template, rendered once when the run
starts, against a context holding only this run's resolved ``input.*`` —
nothing has run yet. ``--label TEXT`` is literal text (never rendered) that
wins over the document's own. Either way the result passes through
:func:`clean_label` before it reaches ``--events``' ``run_start`` or
``runtime.last_run.label`` — both omit the key entirely when there is no
label, so existing events/state files are unaffected.

A malformed ``label:`` template is still a document error ``cof check``
reports (:func:`circuitry.core.document_check.label_syntax_error`) — but a
*run* that reaches a template error anyway (an unchecked file, or any other
render failure) just warns once and continues with no label, the same as
every other render failure here. This module owns only the clean/render
logic; the warning text lives in :func:`render_document_label`'s caller,
``cli.runtime_shim.run``, which also owns ``--resume``'s re-render and
``--label``'s precedence.
"""

from __future__ import annotations

import re
from typing import Any

from .templates import TemplateError, render_template

__all__ = ["MAX_LABEL_LENGTH", "clean_label", "render_document_label"]

#: Cut point, in Unicode code points (not grapheme clusters).
MAX_LABEL_LENGTH = 64

#: An ESC-initiated sequence: an OSC (``ESC ]`` ... terminated by BEL or
#: ``ESC \``), a CSI (``ESC [`` then parameter/intermediate bytes then one
#: final byte in ``@``-``~``), or — for any other byte following ESC — that
#: one byte, treated as a simple two-byte escape. A lone trailing ESC with
#: nothing after it matches none of these and is removed instead by
#: `_CONTROL_RE` below (ESC is itself a C0 control character).
_ESCAPE_RE = re.compile(
    r"\x1b(?:\][^\x07\x1b]*(?:\x07|\x1b\\)|\[[0-?]*[ -/]*[@-~]|.)",
    re.DOTALL,
)
#: ASCII whitespace only — collapsed to one space *before* `_CONTROL_RE`
#: below runs, since \t/\n/\v/\f/\r are themselves C0 control bytes and
#: must become a space rather than be dropped outright. A non-ASCII
#: Unicode space (NBSP, ...) isn't whitespace by this narrower,
#: cross-engine-stable definition and isn't a control character either,
#: so a run of them in the middle of the text is left alone; one at
#: either end is still removed, by the final ``.strip()`` below, like
#: any other Unicode whitespace — ``str.strip()`` doesn't consult this
#: regex at all.
_WHITESPACE_RE = re.compile(r"[ \t\n\r\f\v]+")
#: C0 controls (0x00-0x1F), DEL (0x7F), C1 controls (0x80-0x9F). Run after
#: whitespace collapsing, so the C0 whitespace bytes it just replaced with
#: a literal space are gone and can't be caught here too.
_CONTROL_RE = re.compile(r"[\x00-\x1f\x7f\x80-\x9f]")


def clean_label(text: str) -> str:
    """*text*, cleaned for display as a run's label.

    Every whitespace run (line breaks and tabs included) becomes one space;
    every control character and escape sequence is dropped; the result is
    trimmed and cut at :data:`MAX_LABEL_LENGTH` Unicode characters, with
    nothing appended. An empty result means no label — callers check for
    that themselves (this always returns a string, never ``None``).
    """
    text = _ESCAPE_RE.sub("", text)
    text = _WHITESPACE_RE.sub(" ", text)
    text = _CONTROL_RE.sub("", text).strip()
    # The cut can land right after a whitespace run's collapsed single
    # space, leaving a trailing space the first .strip() never saw.
    return text[:MAX_LABEL_LENGTH].rstrip()


def render_document_label(
    label_template: Any, ctx: dict[str, Any]
) -> tuple[str | None, str | None]:
    """Render a document's top-level ``label:`` against *ctx*.

    Returns ``(label, warning)``: *label* is ``None`` when *label_template*
    isn't a non-empty string, or the rendered+cleaned text was empty;
    *warning* is set, with *label* ``None``, when rendering raised
    (malformed template or any other render failure) — the caller decides
    how to surface it, but never raises itself, matching "a render failure
    warns once and the run continues with no label" (#752).

    Rendered with ``escape=False``: a label is display text in a terminal
    or a log, not HTML, so the same reasoning that turned escaping off for
    prompt text (#397) applies here too.
    """
    if not isinstance(label_template, str) or not label_template:
        return None, None
    try:
        rendered = render_template(label_template, ctx, label="label", escape=False)
    except TemplateError as exc:
        return None, f"{exc}; the run continues without a label."
    cleaned = clean_label(rendered)
    return (cleaned or None), None
