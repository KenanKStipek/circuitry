"""Lenient boolean/number parsing for a model's free-text reply.

Shared by ``prompt_type: boolean``/``number`` decoding (:mod:`circuitry.core.prompt`)
and model-mode ``if``/``while`` conditions (:mod:`circuitry.core.conditional`,
:mod:`circuitry.core.loop`) so a natural reply like ``"Yes."`` or ``"42
degrees"`` is read the same way everywhere in the runtime.

A real model rarely replies with a bare ``yes``/``42`` — it wraps the answer
in a trailing period, a markdown emphasis, an explanation. The parsers here
strip that wrapping, but they never *guess*: an answer that isn't
unambiguously a yes/no or a number raises :class:`AnswerParseError` rather
than returning ``None`` or defaulting to false, so ``on_error``, retries and
provider fallbacks all get a chance to act on it.
"""

from __future__ import annotations

_QUOTE_CHARS = "\"'`"
_EMPHASIS_MARKERS = ("**", "__", "*", "_", "`")
_TRAILING_PUNCT = ".,!?;:"

_TRUE_WORDS = {"true", "yes", "y", "1"}
_FALSE_WORDS = {"false", "no", "n", "0"}


class AnswerParseError(ValueError):
    """A model's reply did not unambiguously parse as the expected answer type.

    Carries the raw, unstripped reply so a caller several layers up an
    exception chain can recover exactly what was rejected.
    """

    def __init__(self, message: str, *, raw_response_text: str) -> None:
        super().__init__(message)
        self.raw_response_text = raw_response_text


def _strip_wrapping(text: str) -> str:
    """Strip whitespace, then any surrounding quote or markdown-emphasis pair.

    Repeats so ``'**"yes"**'`` unwraps fully rather than stopping after one
    layer.
    """
    text = text.strip()
    changed = True
    while changed and text:
        changed = False
        if len(text) >= 2 and text[0] == text[-1] and text[0] in _QUOTE_CHARS:
            text = text[1:-1].strip()
            changed = True
            continue
        for marker in _EMPHASIS_MARKERS:
            width = len(marker)
            if len(text) >= 2 * width and text.startswith(marker) and text.endswith(marker):
                text = text[width:-width].strip()
                changed = True
                break
    return text


def _split_leading_token(text: str) -> tuple[str, str]:
    """``(leading token, remainder)`` after normalising *text*.

    The token has its wrapping quotes/emphasis and trailing punctuation
    removed; the remainder is whatever whitespace-separated content follows
    it, unprocessed.
    """
    text = _strip_wrapping(text)
    if not text:
        return "", ""
    parts = text.split(None, 1)
    token = parts[0].strip(_TRAILING_PUNCT)
    token = _strip_wrapping(token)
    remainder = parts[1] if len(parts) > 1 else ""
    return token, remainder


def parse_boolean_answer(text: str) -> bool:
    """Parse a lenient yes/no answer from a model's reply.

    ``"Yes."``, ``"yes, because ..."``, ``"**TRUE**"`` and ``"Y"`` parse as
    ``True``; ``"No."`` and ``"false!"`` parse as ``False``. Anything else —
    ``"maybe"``, an empty reply — raises :class:`AnswerParseError`.
    """
    token, _remainder = _split_leading_token(text)
    lowered = token.lower()
    if lowered in _TRUE_WORDS:
        return True
    if lowered in _FALSE_WORDS:
        return False
    raise AnswerParseError(
        f"could not parse a yes/no answer from {text!r}", raw_response_text=text
    )


def parse_number_answer(text: str) -> int | float:
    """Parse a lenient number from a model's reply.

    ``"42"``, ``"42."``, ``"3.5"``, ``"-1"`` and ``"1e3"`` parse; anything
    with trailing words attached — ``"about 42"``, ``"42 degrees"`` — raises
    rather than guessing which number was meant.
    """
    token, remainder = _split_leading_token(text)
    if not token or remainder:
        raise AnswerParseError(
            f"could not parse a number from {text!r}", raw_response_text=text
        )
    try:
        return int(token)
    except ValueError:
        pass
    try:
        return float(token)
    except ValueError:
        raise AnswerParseError(
            f"could not parse a number from {text!r}", raw_response_text=text
        ) from None
