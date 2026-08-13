"""The last-run stash: where ``cof run`` records what to replay for ``--last``.

``cof run`` writes its arguments to ``~/.config/circuitry/last-run.json`` after
a successful run (credential-shaped ``-e`` values redacted first — see
:mod:`circuitry.cli.redaction`) so ``cof run --last`` can repeat it. The TUI's
Runs view offers the same replay from a keypress, so the *location* of that
file and the rules for reading it back live here rather than inside the CLI
command that happens to write it.

Reading is total: a missing stash is ``None`` and an unreadable one is a
:class:`LastRun` carrying ``error``, because a corrupt file must render as an
error state in a view, not raise inside a repaint.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .config import GLOBAL_CONFIG_DIR
from .redaction import REDACTED

__all__ = [
    "LAST_RUN_PATH",
    "LastRun",
    "read_last_run",
    "state_from_env_pairs",
]

#: Where ``cof run`` stashes the arguments ``--last`` replays.
LAST_RUN_PATH = GLOBAL_CONFIG_DIR / "last-run.json"

#: What a replay refuses to do, and why, when the stash holds redacted values.
REDACTED_REFUSAL = (
    "The last run passed secrets via -e, so they were redacted before being "
    "stashed. Replaying would send the literal redaction marker — re-run it "
    "with the secret supplied through an environment variable or config file."
)


@dataclass(frozen=True)
class LastRun:
    """The stashed arguments of the most recent successful ``cof run``."""

    path: Path
    args: dict[str, Any] = field(default_factory=dict)
    #: Set when the file exists but could not be read; ``args`` is then empty.
    error: str = ""

    @property
    def ok(self) -> bool:
        return not self.error

    @property
    def orchestration(self) -> str:
        return str(self.args.get("orchestration") or "")

    @property
    def adapter(self) -> str:
        return str(self.args.get("adapter") or "")

    @property
    def model(self) -> str:
        return str(self.args.get("model") or "")

    @property
    def profile(self) -> str:
        return str(self.args.get("profile") or "")

    @property
    def dry_run(self) -> bool:
        return bool(self.args.get("dry_run"))

    @property
    def skip_preflight(self) -> bool:
        return bool(self.args.get("skip_preflight"))

    @property
    def env_pairs(self) -> list[str]:
        pairs = self.args.get("env_vars")
        if not isinstance(pairs, list):
            return []
        return [pair for pair in pairs if isinstance(pair, str)]

    def _path_arg(self, key: str) -> Path | None:
        value = self.args.get(key)
        return Path(str(value)) if value else None

    @property
    def config_path(self) -> Path | None:
        return self._path_arg("config")

    @property
    def state_path(self) -> Path | None:
        return self._path_arg("state")

    @property
    def out_path(self) -> Path | None:
        return self._path_arg("out")

    @property
    def live_state_path(self) -> Path | None:
        return self._path_arg("live_state")

    @property
    def has_redacted_secrets(self) -> bool:
        """True when replaying would pass a redaction marker as a real value."""
        return any(pair.endswith(f"={REDACTED}") for pair in self.env_pairs)

    @property
    def blocked_reason(self) -> str:
        """Why this run must not be replayed, or ``""`` when it may be."""
        if self.error:
            return self.error
        if not self.orchestration:
            return "The stashed run names no orchestration."
        if self.has_redacted_secrets:
            return REDACTED_REFUSAL
        return ""

    def initial_state(self) -> dict[str, Any]:
        """The ``-e`` values as a state mapping (empty when there were none)."""
        return state_from_env_pairs(self.env_pairs)

    def summary_rows(self) -> list[tuple[str, str]]:
        """Label/value rows describing the stashed run, for a detail panel."""
        if self.error:
            return [("error", self.error)]
        rows = [("orchestration", self.orchestration or "—")]
        for label, value in (
            ("adapter", self.adapter),
            ("model", self.model),
            ("profile", self.profile),
        ):
            if value:
                rows.append((label, value))
        inputs = self.initial_state()
        if inputs:
            rows.append(("inputs", ", ".join(sorted(inputs))))
        for label, path in (("state in", self.state_path), ("state out", self.out_path)):
            if path is not None:
                rows.append((label, str(path)))
        if self.dry_run:
            rows.append(("dry run", "yes"))
        return rows


def read_last_run(path: Path | None = None) -> LastRun | None:
    """The stashed run, ``None`` when nothing was ever stashed.

    Never raises: an unreadable or malformed stash comes back as a
    :class:`LastRun` whose ``error`` explains what is wrong with it.
    """
    target = LAST_RUN_PATH if path is None else path
    try:
        raw = target.read_text(encoding="utf-8")
    except FileNotFoundError:
        return None
    except OSError as exc:
        return LastRun(path=target, error=f"Could not read {target}: {exc.strerror or exc}")
    try:
        args = json.loads(raw)
    except json.JSONDecodeError as exc:
        return LastRun(path=target, error=f"{target} is not valid JSON — {exc.msg}.")
    if not isinstance(args, dict):
        return LastRun(path=target, error=f"{target} does not hold a JSON object.")
    return LastRun(path=target, args=args)


def state_from_env_pairs(pairs: list[str] | None) -> dict[str, Any]:
    """Parse stashed ``KEY=VALUE`` pairs into initial state, JSON where it parses.

    Tolerant on purpose — this reads a file written by an earlier run, so a
    malformed entry is skipped rather than raised. The CLI's own parser is
    strict because it is reading what the user just typed.
    """
    state: dict[str, Any] = {}
    for entry in pairs or []:
        if not isinstance(entry, str) or "=" not in entry:
            continue
        key, _, value = entry.partition("=")
        try:
            state[key] = json.loads(value)
        except (json.JSONDecodeError, ValueError):
            state[key] = value
    return state
