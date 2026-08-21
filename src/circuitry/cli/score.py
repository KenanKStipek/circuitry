"""``cof score`` — a per-effect complexity preview, without running anything.

:func:`~circuitry.core.compiler.compile_orchestration` freezes the entire
effect graph before a single model call is made, so the tree can be walked and
every prompt in it scored statically. That is what this command does: compile,
walk, score, print. No adapter is built, no store is opened, nothing executes.

What the preview cannot know
---------------------------

Two things, and the command says both out loud rather than printing a table
that merely *looks* complete:

* **The scores are template-based estimates.** A static score measures the
  authored template; the runtime scores the *rendered* prompt, after state has
  been interpolated. Interpolation only adds text, so a runtime score is at
  least as high — and much higher wherever a template pulls in a large blob.
* **Some effects do not exist yet.** A reflector plans its effects at runtime,
  and a ``use`` effect compiles the orchestration it references only when it
  executes. Neither is in the frozen tree. They are reported as unscoreable
  rows carrying the reason, never silently dropped.

Effects a ``dynamic`` container declares *are* in the frozen tree — the
compiler compiles them alongside everything else — so they are scored like any
other nested effect.

Addressing
----------

Rows are keyed by the same dotted paths a profile uses
(:func:`circuitry.cli.profiles.collect_orchestration_effect_paths`): relative
to the ``prime`` root, with anonymous conditionals and loops contributing no
segment of their own. A loop body's effects therefore appear once, at the path
they are addressed by, not once per iteration — the iteration count is not
known before the run either.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Optional, Sequence

import typer
from rich.console import Console
from rich.table import Table

from ..core.compiler import apply_effect_overrides, compile_orchestration
from ..core.complexity import MAX_SCORE, ComplexityScore, SignalScore, StructureContext
from ..core.complexity import score as score_prompt
from ..core.conditional import ConditionalDefinition
from ..core.dynamic import DynamicDefinition
from ..core.loop import LoopDefinition
from ..core.prompt import PromptDefinition
from ..core.reflector import ReflectorDefinition
from ..core.tool import ToolDefinition
from ..core.use import UseDefinition
from .complexity_config import ComplexityBand, ComplexitySettings
from .config import resolve_config
from .effective_settings import resolve_effective_settings
from .orchestration_loader import load_orchestration_file
from .profiles import ProfileError, ProfileSettings, load_profile

console = Console()
err_console = Console(stderr=True)

__all__ = ["ScoredEffect", "register_score", "score_orchestration"]

#: Printed under every table and carried in the JSON payload. The honesty
#: requirement is part of the command's contract, not a nicety: a preview that
#: reads as a measurement would be worse than no preview.
ESTIMATE_NOTICE = (
    "Scores are static estimates measured against prompt templates, not "
    "rendered prompts. The runtime score is computed once state has been "
    "interpolated and will be higher wherever a template pulls in a large "
    "value."
)

#: Reasons an effect in the frozen tree cannot be scored before the run. Each
#: says *why*, so a reader can tell "not applicable" from "not knowable yet".
REFLECTOR_REASON = (
    "planned at runtime: the reflector generates these effects from its "
    "'{plan_from_step}' output, so they do not exist until the run reaches it."
)
USE_REASON = (
    "compiled at runtime: a 'use' effect resolves and compiles the "
    "orchestration it references only when it executes, so its effects are "
    "not in this tree."
)
TOOL_REASON = (
    "not a prompt: a tool effect calls a plugin rather than a model, so there "
    "is no prompt to score."
)
DISABLED_REASON = "disabled for this run by the profile, so it will not execute."

#: How many signals the table names per effect. Three is enough to explain a
#: surprising number; the full breakdown is one ``--json`` away.
_DOMINANT_SIGNALS = 3


def _join(scope: str, name: Optional[str]) -> str:
    """Append *name* to a dotted scope, matching ``compiler._scope_child``."""
    if not name:
        return scope
    return f"{scope}.{name}" if scope else name


@dataclass(frozen=True)
class ScoredEffect:
    """One row of the preview: a scored prompt, or a reason there is no score."""

    path: str
    type: str
    scoreable: bool
    reason: Optional[str] = None
    result: Optional[ComplexityScore] = None
    band: Optional[ComplexityBand] = None

    @property
    def score(self) -> Optional[float]:
        return self.result.score if self.result is not None else None

    def dominant_signals(self, limit: int = _DOMINANT_SIGNALS) -> list[SignalScore]:
        """The signals that actually moved the number, largest first.

        Ties break on name so the output is byte-stable run to run, and signals
        contributing nothing are dropped — listing them would pad the row
        without explaining anything.
        """
        if self.result is None:
            return []
        ranked = sorted(
            self.result.signals, key=lambda s: (-s.contribution, s.name)
        )
        return [entry for entry in ranked if entry.contribution > 0][:limit]

    def to_dict(self) -> dict[str, Any]:
        return {
            "path": self.path,
            "type": self.type,
            "scoreable": self.scoreable,
            "reason": self.reason,
            "score": self.score,
            "band": self.band.name if self.band is not None else None,
            "band_model": self.band.model if self.band is not None else None,
            "dominant_signals": [
                {"name": entry.name, "contribution": entry.contribution}
                for entry in self.dominant_signals()
            ],
            "breakdown": self.result.to_dict() if self.result is not None else None,
        }


def _band_for(
    value: float, bands: Sequence[ComplexityBand]
) -> Optional[ComplexityBand]:
    """First band whose inclusive ``max`` covers *value*; the catch-all last.

    Mirrors the band-table contract validated in
    :mod:`circuitry.cli.complexity_config`: bands ascend, and the final entry
    has no ``max``.
    """
    for band in bands:
        if band.max is None or value <= band.max:
            return band
    return None


def _walk(
    node: Any,
    *,
    scope: str,
    depth: int,
    loop_depth: int,
    disabled: bool,
    settings: ComplexitySettings,
    rows: list[ScoredEffect],
) -> None:
    """Score *node* and everything under it, appending rows in tree order.

    Shaped after ``compiler._overlay_effect``: the same per-type recursion, the
    same dotted addressing, so a path printed here is a path a profile can
    target. Containers contribute no row of their own — they hold no prompt —
    but the effects they *would* have produced at runtime do, as unscoreable
    rows.
    """
    name = getattr(node, "name", None)
    own_path = _join(scope, name) if name else scope
    # A disabled container never executes its subtree (see ``core.disabled``),
    # so the flag travels down rather than being re-read per node.
    node_disabled = disabled or not getattr(node, "enabled", True)

    if isinstance(node, PromptDefinition):
        rows.append(
            _score_prompt_row(
                node,
                path=own_path,
                depth=depth,
                loop_depth=loop_depth,
                disabled=node_disabled,
                settings=settings,
            )
        )
        return

    if isinstance(node, ToolDefinition):
        rows.append(
            ScoredEffect(
                path=own_path,
                type="tool",
                scoreable=False,
                reason=DISABLED_REASON if node_disabled else TOOL_REASON,
            )
        )
        return

    if isinstance(node, UseDefinition):
        rows.append(
            ScoredEffect(
                path=own_path,
                type="use",
                scoreable=False,
                reason=DISABLED_REASON if node_disabled else USE_REASON,
            )
        )
        return

    if isinstance(node, ReflectorDefinition):
        # The reflector's own effects are authored and compiled, so they score
        # like anything else. What it *plans* does not exist yet.
        for child in node.inner.effects:
            _walk(
                child,
                scope=own_path,
                depth=depth + 1,
                loop_depth=loop_depth,
                disabled=node_disabled,
                settings=settings,
                rows=rows,
            )
        rows.append(
            ScoredEffect(
                path=_join(own_path, node.generated_key),
                type="reflector_generated",
                scoreable=False,
                reason=(
                    DISABLED_REASON
                    if node_disabled
                    else REFLECTOR_REASON.format(plan_from_step=node.plan_from_step)
                ),
            )
        )
        return

    if isinstance(node, DynamicDefinition):
        for child in node.effects:
            _walk(
                child,
                scope=own_path,
                depth=depth + 1,
                loop_depth=loop_depth,
                disabled=node_disabled,
                settings=settings,
                rows=rows,
            )
        return

    if isinstance(node, ConditionalDefinition):
        # Both branches are previewed: which one runs depends on state that
        # does not exist yet, and showing only one would be a guess.
        for child in (*node.then_effects, *node.else_effects):
            _walk(
                child,
                scope=own_path,
                depth=depth + 1,
                loop_depth=loop_depth,
                disabled=node_disabled,
                settings=settings,
                rows=rows,
            )
        return

    if isinstance(node, LoopDefinition):
        for child in node.body:
            _walk(
                child,
                scope=own_path,
                depth=depth + 1,
                loop_depth=loop_depth + 1,
                disabled=node_disabled,
                settings=settings,
                rows=rows,
            )
        return


def _score_prompt_row(
    node: PromptDefinition,
    *,
    path: str,
    depth: int,
    loop_depth: int,
    disabled: bool,
    settings: ComplexitySettings,
) -> ScoredEffect:
    if disabled:
        return ScoredEffect(
            path=path, type="prompt", scoreable=False, reason=DISABLED_REASON
        )

    # An empty configured keyword table means "none configured", not "disable
    # the signal" — the scorer's own default table stands in. Turning the
    # signal off is spelled by zeroing ``weights.keywords``.
    keywords = dict(settings.scoring.keywords) or None
    result = score_prompt(
        node,
        weights=settings.scoring.scorer_weights(),
        keyword_weights=keywords,
        structure=StructureContext(depth=depth, loop_depth=loop_depth),
    )
    return ScoredEffect(
        path=path,
        type="prompt",
        scoreable=True,
        result=result,
        band=_band_for(result.score, settings.routing.bands),
    )


def score_orchestration(
    orch: dict[str, Any],
    *,
    settings: ComplexitySettings,
    profile: Optional[ProfileSettings] = None,
) -> list[ScoredEffect]:
    """Compile *orch* and score every prompt effect in the frozen tree.

    Pure with respect to the filesystem and the network: it compiles, walks and
    scores. A *profile* is applied first, through the same
    :func:`~circuitry.core.compiler.apply_effect_overrides` a run uses, so the
    preview reflects the model/provider overrides and disabled effects that run
    would see.
    """
    root = compile_orchestration(orch=orch)
    if profile is not None and profile.effects:
        root, _matched = apply_effect_overrides(root, profile.effects)

    rows: list[ScoredEffect] = []
    for child in root.effects:
        _walk(
            child,
            scope="",
            depth=0,
            loop_depth=0,
            disabled=not root.enabled,
            settings=settings,
            rows=rows,
        )
    return rows


def _summary(rows: Sequence[ScoredEffect]) -> dict[str, Any]:
    scored = [row for row in rows if row.result is not None]
    highest = max(scored, key=lambda r: (r.score or 0.0, r.path), default=None)
    return {
        "effects": len(rows),
        "scored": len(scored),
        "unscoreable": len(rows) - len(scored),
        "highest": (
            {"path": highest.path, "score": highest.score}
            if highest is not None
            else None
        ),
    }


def _build_payload(
    rows: Sequence[ScoredEffect],
    *,
    orchestration: Path,
    config: Optional[Path],
    profile: Optional[str],
    settings: ComplexitySettings,
) -> dict[str, Any]:
    return {
        "orchestration": str(orchestration),
        "config": str(config) if config is not None else None,
        "profile": profile,
        "mode": "static",
        "estimated": True,
        "notice": ESTIMATE_NOTICE,
        "max_score": MAX_SCORE,
        "weights": settings.scoring.scorer_weights(),
        "bands": [band.as_dict() for band in settings.routing.bands],
        "effects": [row.to_dict() for row in rows],
        "summary": _summary(rows),
    }


def _render_table(
    rows: Sequence[ScoredEffect], *, settings: ComplexitySettings
) -> Table:
    show_band = bool(settings.routing.bands)
    table = Table(title="Circuitry · Score (static preview)", show_lines=False)
    table.add_column("Effect", overflow="fold")
    table.add_column("Type")
    table.add_column("Score", justify="right")
    if show_band:
        table.add_column("Band")
    table.add_column("Dominant signals / reason", overflow="fold")

    for row in rows:
        if row.result is not None:
            score_cell = f"{row.result.score:.1f}"
            detail = ", ".join(
                f"{entry.name} {entry.contribution:.1f}"
                for entry in row.dominant_signals()
            ) or "no signal contributes"
        else:
            score_cell = "[dim]—[/dim]"
            detail = f"[yellow]not scoreable[/yellow] — {row.reason}"

        cells = [row.path, row.type, score_cell]
        if show_band:
            band = row.band
            cells.append(
                (band.name or band.model) if band is not None else "[dim]—[/dim]"
            )
        cells.append(detail)
        table.add_row(*cells)

    return table


def _scoring_disabled_message(config_path: Optional[Path]) -> str:
    where = f" in {config_path}" if config_path is not None else ""
    return (
        f"Complexity scoring is disabled{where}. `cof score` previews the "
        "scores a run would compute, so it needs the same switch a run does. "
        "Enable it with runtime.complexity.scoring.enabled: true (see "
        "docs/complexity-config.md), or pass --config/--profile pointing at "
        "settings that already have it on."
    )


def register_score(app: typer.Typer) -> None:
    @app.command(
        "score",
        help=(
            "Preview per-effect complexity scores for an orchestration "
            "without running it."
        ),
    )
    def score_cmd(
        orchestration: Path = typer.Argument(
            ...,
            exists=True,
            dir_okay=False,
            readable=True,
            help="Path to orchestration file.",
        ),
        config: Optional[Path] = typer.Option(
            None,
            "--config",
            "-c",
            help="Path to config JSON (or use CIRCUITRY_CONFIG).",
        ),
        profile: Optional[str] = typer.Option(
            None,
            "--profile",
            help=(
                "Named profile to preview under (profiles/<name>.yml). Applies "
                "the same model/provider overrides and disabled effects a run "
                "would."
            ),
        ),
        json_out: bool = typer.Option(
            False, "--json", help="Output machine-readable JSON only."
        ),
    ) -> None:
        cfg = resolve_config(explicit_path=config)
        orch = load_orchestration_file(orchestration)

        profile_settings: Optional[ProfileSettings] = None
        if profile:
            try:
                profile_settings = load_profile(
                    name=profile, orchestration_path=orchestration, orch=orch
                )
            except ProfileError as exc:
                err_console.print(f"[red]Error:[/red] {exc}")
                raise typer.Exit(code=1) from exc

        effective = resolve_effective_settings(
            cfg=cfg, orch=orch, profile=profile_settings
        )
        settings = effective.complexity

        if not settings.scoring.enabled:
            message = _scoring_disabled_message(config)
            if json_out:
                typer.echo(
                    json.dumps(
                        {
                            "ok": False,
                            "scoring_enabled": False,
                            "error": message,
                        },
                        indent=2,
                    )
                )
            else:
                err_console.print(f"[yellow]Scoring disabled.[/yellow] {message}")
            raise typer.Exit(code=1)

        try:
            rows = score_orchestration(
                orch, settings=settings, profile=profile_settings
            )
        except ValueError as exc:
            # A tree that will not compile cannot be previewed; report it the
            # way `cof validate` would rather than as a traceback.
            err_console.print(f"[red]Error:[/red] {exc}")
            raise typer.Exit(code=1) from exc

        payload = _build_payload(
            rows,
            orchestration=orchestration,
            config=config,
            profile=profile,
            settings=settings,
        )

        if json_out:
            # Plain stdout, not ``console.print_json``: a machine-readable
            # payload must not be re-wrapped to the terminal width.
            typer.echo(json.dumps(payload, indent=2, ensure_ascii=False))
            return

        if not rows:
            console.print(
                "[yellow]No effects to score.[/yellow] The orchestration "
                "defines no effects."
            )
            return

        console.print(_render_table(rows, settings=settings))
        summary = payload["summary"]
        console.print(
            f"[dim]{summary['scored']} scored, "
            f"{summary['unscoreable']} not scoreable, "
            f"{summary['effects']} effect(s) total.[/dim]"
        )
        console.print(f"[dim]{ESTIMATE_NOTICE}[/dim]")
