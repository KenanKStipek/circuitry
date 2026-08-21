"""Reading a prompt effect's complexity score for display.

:mod:`circuitry.core.complexity` computes the score and
:mod:`circuitry.cli.complexity_config` decides what to do with it. Neither
is imported here. What the TUI gets is whatever the runtime left on an
effect's node — ``meta["complexity"]``, the mapping
:meth:`~circuitry.core.complexity.ComplexityScore.to_dict` produces, plus a
``band`` when routing resolved one — arriving either in an
``on_effect_start`` payload or in a state snapshot.

Every function here is therefore *tolerant*, in the specific sense the
views need:

* **The key is usually absent.** Scoring is off by default, and non-prompt
  effects are never scored. :func:`read` answers ``None``, and the views
  draw :data:`NO_SCORE` where a number would go rather than a blank cell.
* **The payload may be partial.** A score with no signals, no band, no
  ``max_score`` is still a score. Missing pieces default; junk ones are
  dropped. Nothing raises — a malformed score is a cosmetic problem, and
  taking down the run view over one would not be.
* **The score may predate the effect.** It is written before dispatch, so
  a running effect has one and a failed one keeps it.

Bands
-----

A band is the qualitative reading of a number that is otherwise hard to
place: 62 out of 100 means little until it is *high*. When routing is
configured its band table names the bands and :func:`read` uses those
names, so the TUI says what the router said. With no table there is still
a score to describe, so :func:`band_for` supplies the default names in
:data:`DEFAULT_BANDS`.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any

__all__ = [
    "DEFAULT_BANDS",
    "DOMINANT_SHARE",
    "MAX_SCORE",
    "NO_SCORE",
    "SCORE_WIDTH",
    "EffectComplexity",
    "SignalBreakdown",
    "band_for",
    "read",
]

#: Upper bound of the score range, restated from
#: :data:`circuitry.core.complexity.MAX_SCORE`. A payload carrying its own
#: ``max_score`` wins; this is the fallback for one that does not.
MAX_SCORE = 100.0

#: Drawn where a score would go on an effect that has none.
NO_SCORE = "—"

#: Cells reserved for the number itself, so ``7`` and ``100`` line up.
SCORE_WIDTH = 3

#: Default band table: inclusive upper bound (as a fraction of the score
#: range) and the name for everything at or below it. Used only when the
#: payload names no band of its own — a configured
#: ``runtime.complexity.routing`` band table always wins, so the TUI never
#: contradicts the router.
DEFAULT_BANDS: tuple[tuple[float, str], ...] = (
    (0.25, "low"),
    (0.50, "moderate"),
    (0.75, "high"),
    (1.00, "severe"),
)

#: Share of the total a run of signals must account for to be called
#: dominant. Signals are taken strongest-first until their contributions
#: reach this much of the score, which is what "which signals dominated"
#: means concretely: the shortest list that explains half the number.
DOMINANT_SHARE = 0.5


def _number(value: Any) -> float | None:
    """Coerce to float, or ``None``. ``bool`` is not a number here."""
    if isinstance(value, bool) or value is None:
        return None
    if isinstance(value, (int, float)):
        return float(value)
    return None


def _text(value: Any) -> str:
    return value.strip() if isinstance(value, str) else ""


def band_for(score: float, max_score: float = MAX_SCORE) -> str:
    """Name the band ``score`` falls in, using :data:`DEFAULT_BANDS`.

    The table is expressed as fractions of the range so it holds whatever
    ``max_score`` a payload declares. A non-positive range has no bands to
    speak of and gets the lowest name.
    """
    if max_score <= 0:
        return DEFAULT_BANDS[0][1]
    fraction = score / max_score
    for limit, name in DEFAULT_BANDS:
        if fraction <= limit:
            return name
    return DEFAULT_BANDS[-1][1]


@dataclass(frozen=True)
class SignalBreakdown:
    """One signal's share of a score, as the breakdown pane shows it."""

    name: str
    #: Score points this signal added. The contributions sum to the total.
    contribution: float = 0.0
    #: The raw mechanical measurement — a token count, a field count.
    raw: float | None = None
    #: ``raw`` mapped onto ``[0, 1]``.
    normalized: float | None = None
    #: Weight applied to ``normalized``.
    weight: float | None = None
    #: The scorer's one-line explanation of the measurement.
    note: str = ""
    #: True when the measurement was inferred rather than observed.
    estimated: bool = False

    @classmethod
    def from_mapping(cls, entry: Any) -> SignalBreakdown | None:
        """Read one signal, or ``None`` when there is no usable name."""
        if not isinstance(entry, Mapping):
            return None
        name = _text(entry.get("name"))
        if not name:
            return None
        return cls(
            name=name,
            contribution=_number(entry.get("contribution")) or 0.0,
            raw=_number(entry.get("raw")),
            normalized=_number(entry.get("normalized")),
            weight=_number(entry.get("weight")),
            note=_text(entry.get("note")),
            estimated=bool(entry.get("estimated")),
        )

    def share(self, total: float) -> float:
        """This signal's fraction of ``total``; 0 when there is no total."""
        return self.contribution / total if total > 0 else 0.0

    def line(self, total: float, *, width: int = 0) -> str:
        """One breakdown row: name, points, share, and the scorer's note."""
        percent = f"{self.share(total) * 100:.0f}%"
        marker = "~" if self.estimated else " "
        head = f"{self.name.ljust(width)}{marker} {self.contribution:5.1f}  {percent:>4}"
        return f"{head}  {self.note}".rstrip()


@dataclass(frozen=True)
class EffectComplexity:
    """A prompt effect's score, band, and the signals behind them."""

    score: float
    max_score: float = MAX_SCORE
    #: Band name — the router's when routing named one, else :func:`band_for`.
    band: str = ""
    #: Model the band routed to, when the payload carried one.
    model: str = ""
    #: ``"rendered"`` or ``"static"``; empty when the payload did not say.
    mode: str = ""
    #: True when at least one signal was inferred rather than measured.
    estimated: bool = False
    signals: tuple[SignalBreakdown, ...] = ()
    warnings: tuple[str, ...] = ()

    @property
    def cell(self) -> str:
        """The run view's gutter cell: the rounded score, then the band."""
        return f"{self.score:>{SCORE_WIDTH}.0f} {self.band}".rstrip()

    @property
    def summary(self) -> str:
        """One-line reading of the score, for the inspector's meta table."""
        text = f"{self.score:.1f}/{self.max_score:.0f}"
        if self.band:
            text = f"{text}  {self.band}"
        if self.model:
            text = f"{text} → {self.model}"
        qualifiers = [bit for bit in (self.mode, "estimated" if self.estimated else "") if bit]
        return f"{text}  ({', '.join(qualifiers)})" if qualifiers else text

    @property
    def ranked(self) -> tuple[SignalBreakdown, ...]:
        """Signals strongest first; ties broken by name so order is stable."""
        return tuple(sorted(self.signals, key=lambda s: (-s.contribution, s.name)))

    @property
    def dominant(self) -> tuple[SignalBreakdown, ...]:
        """The signals that account for :data:`DOMINANT_SHARE` of the score.

        Taken strongest-first until their running total crosses the
        threshold, so the answer is always the *shortest* set that explains
        the number — one signal when one signal did it, three when the
        score is a broad average. Empty when nothing contributed.
        """
        ranked = [signal for signal in self.ranked if signal.contribution > 0]
        if not ranked:
            return ()
        target = sum(signal.contribution for signal in ranked) * DOMINANT_SHARE
        running = 0.0
        chosen: list[SignalBreakdown] = []
        for signal in ranked:
            chosen.append(signal)
            running += signal.contribution
            if running >= target:
                break
        return tuple(chosen)

    def breakdown_lines(self) -> list[str]:
        """The signal breakdown as plain text rows, strongest first.

        Dominant signals are marked with ``▸`` and named again underneath,
        because "which signals dominated" is the question the pane exists
        to answer and reading it off a sorted table is work.
        """
        if not self.signals:
            return ["no signal breakdown recorded"]
        total = sum(signal.contribution for signal in self.signals)
        dominant = {signal.name for signal in self.dominant}
        width = max(len(signal.name) for signal in self.signals)
        lines = [
            f"{'▸' if signal.name in dominant else ' '} {signal.line(total, width=width)}"
            for signal in self.ranked
        ]
        if dominant:
            lines.append(f"dominated by {', '.join(s.name for s in self.dominant)}")
        lines.extend(f"! {warning}" for warning in self.warnings)
        return lines


def _read_band(payload: Mapping[str, Any]) -> tuple[str, str]:
    """``(band name, routed model)`` as the payload declares them.

    Routing may record its decision as a bare name or as the band object
    itself; both are read, and neither is required.
    """
    band = payload.get("band")
    if isinstance(band, Mapping):
        return _text(band.get("name")), _text(band.get("model"))
    return _text(band), _text(payload.get("model"))


def _read_signals(payload: Mapping[str, Any]) -> tuple[SignalBreakdown, ...]:
    raw = payload.get("signals")
    if not isinstance(raw, Sequence) or isinstance(raw, (str, bytes)):
        return ()
    read = (SignalBreakdown.from_mapping(entry) for entry in raw)
    return tuple(signal for signal in read if signal is not None)


def _read_warnings(payload: Mapping[str, Any]) -> tuple[str, ...]:
    raw = payload.get("warnings")
    if not isinstance(raw, Sequence) or isinstance(raw, (str, bytes)):
        return ()
    return tuple(_text(entry) for entry in raw if _text(entry))


def read(meta: Any) -> EffectComplexity | None:
    """Read ``meta["complexity"]`` for display, or ``None`` when there is none.

    ``None`` is the ordinary answer, not a failure: scoring is off by
    default and only prompt effects are ever scored. It is also the answer
    for a payload with no usable number in it, so a caller has exactly one
    case to handle — a score, or no score.

    Accepts the mapping
    :meth:`~circuitry.core.complexity.ComplexityScore.to_dict` produces, a
    partial one carrying nothing but ``score``, or a bare number.
    """
    if not isinstance(meta, Mapping):
        return None
    payload = meta.get("complexity")

    bare = _number(payload)
    if bare is not None:
        payload = {"score": bare}
    elif not isinstance(payload, Mapping):
        return None

    score = _number(payload.get("score"))
    if score is None:
        return None

    max_score = _number(payload.get("max_score"))
    if max_score is None or max_score <= 0:
        max_score = MAX_SCORE
    score = min(max(score, 0.0), max_score)

    band, model = _read_band(payload)
    return EffectComplexity(
        score=score,
        max_score=max_score,
        band=band or band_for(score, max_score),
        model=model,
        mode=_text(payload.get("mode")),
        estimated=bool(payload.get("estimated")),
        signals=_read_signals(payload),
        warnings=_read_warnings(payload),
    )
