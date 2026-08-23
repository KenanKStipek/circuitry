"""``cof run --explain-routing`` — the per-effect line printed as a run dispatches.

:mod:`circuitry.cli.score` answers "what would this orchestration score
before I run it"; this module answers the run-time half — "what did this
effect score, and what did that mean" — printed the moment before the effect
dispatches, via the same ``effect_start_observer`` hook
:class:`~circuitry.cli.runtime_shim.RunRequest` already exposes.

Everything printed here reads data :mod:`circuitry.core.prompt` already wrote
to the node before firing ``on_effect_start`` — ``meta.model``,
``meta.model_reason`` and ``meta.complexity`` (with its optional ``band``).
Nothing is computed twice and nothing is guessed: the line is a plain
transcription of the pre-dispatch meta block, which is why it needs no access
to the compiled orchestration or the resolved complexity settings, only the
``(effect_path, effect_node)`` pair the hook already hands over.

A node with no ``meta.complexity`` prints nothing — the non-prompt effects
(tool, loop, conditional, dynamic containers) that fire the same hook, and
every prompt effect when scoring is off. That single check is what makes
"emits nothing when scoring is disabled" true without a separate switch to
thread through here: nothing ever puts a ``complexity`` key on a node without
the switch on.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

from ..core.complexity import MAX_SCORE

__all__ = ["explain_line", "make_explain_routing_observer"]


def _number(value: Any) -> float | None:
    if isinstance(value, bool) or value is None:
        return None
    if isinstance(value, (int, float)):
        return float(value)
    return None


def explain_line(path: str, node: Any) -> str | None:
    """One Rich-markup line for *node*, or ``None`` if there is nothing to say.

    ``None`` covers every effect this feature has no opinion about: a
    non-prompt effect, and a prompt effect scored with nothing worth
    reporting (an unusable payload). Both are ordinary, not errors.
    """
    if not isinstance(node, dict):
        return None
    meta = node.get("meta")
    if not isinstance(meta, dict):
        return None
    complexity = meta.get("complexity")
    if not isinstance(complexity, dict):
        return None

    score = _number(complexity.get("score"))
    if score is None:
        return None
    max_score = _number(complexity.get("max_score"))
    if max_score is None or max_score <= 0:
        max_score = MAX_SCORE

    model = meta.get("model")
    model_label = model if isinstance(model, str) and model else "—"
    reason = meta.get("model_reason")
    reason_label = reason if isinstance(reason, str) and reason else "default"

    band = complexity.get("band")
    if isinstance(band, dict) and (band.get("name") or band.get("model")):
        band_label = band.get("name") or band.get("model")
        routing_bit = f"band {band_label}"
    else:
        # ``band`` is only ever written when
        # ``runtime.complexity.routing.enabled`` is true (see
        # ``core.prompt._score_complexity``), so its absence *is* "routing
        # off" — nothing else needs to say so separately.
        routing_bit = "routing off"

    return (
        f"[cyan]▸[/cyan] {path} [dim]score {score:.1f}/{max_score:.0f}"
        f" · {routing_bit} · model {model_label} · why {reason_label}[/dim]"
    )


def make_explain_routing_observer(
    print_line: Callable[[str], None],
) -> Callable[[str, dict[str, Any]], None]:
    """Build an ``effect_start_observer`` that prints :func:`explain_line`.

    *print_line* is the caller's output sink (typically ``console.print``),
    kept separate from this module so it stays free of a Rich ``Console``
    dependency and easy to test with a plain list.
    """

    def _observer(path: str, node: dict[str, Any]) -> None:
        line = explain_line(path, node)
        if line is not None:
            print_line(line)

    return _observer
