"""Hosting the wizard — the conversation loop, the validator, and saving.

`curation/agents/wizard.yml` handles exactly one turn. Everything the *host*
owes it lives here: the seed, the accumulated transcript, the current draft,
the verdict on that draft, and the two ways a finished draft leaves the app.
None of it imports Textual, so the loop that drives a chat screen is the same
loop `scripts/wizard-chat` drives from a terminal — and a test can exercise it
without booting an app.

    Seed + Conversation  →  run_turn (api.run_orchestration)  →  Turn
                         ←  draft + DraftStatus  ←  validate_draft

Two rules the rest of the module exists to enforce:

- **The pane's verdict is the file's verdict.** Every draft is re-validated
  here with :func:`~circuitry.cli.runtime_shim.validate` — the gate `cof check`
  runs — rather than trusting the ``valid`` flag the wizard reports about its
  own work.
- **An invalid draft cannot be saved.** Both save paths re-validate and raise
  :class:`InvalidDraft` rather than write, so a green pane is not something the
  UI can get out of step with.
"""

from __future__ import annotations

import json
import re
import tempfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any, Callable, Optional

if TYPE_CHECKING:  # pragma: no cover - typing only
    from ..adapters import Adapter
    from ..cli.config import CircuitryConfig

__all__ = [
    "CATEGORIES",
    "DEFAULT_CATEGORY",
    "Conversation",
    "DraftStatus",
    "InvalidDraft",
    "LibrarySave",
    "Message",
    "Seed",
    "Turn",
    "TurnRunner",
    "default_library_dir",
    "default_runner",
    "dig",
    "manifest_entry",
    "run_turn",
    "save_to_file",
    "save_to_library",
    "slugify",
    "validate_draft",
    "wizard_path",
]

#: The curation entry the chat view hosts.
WIZARD_REF = "agents/wizard"

#: Categories a saved orchestration may claim. Fixed by the curation manifest
#: schema's ``category`` enum — a generated entry has to pass the same gate the
#: bundled library does.
CATEGORIES: tuple[str, ...] = ("recipes", "patterns", "utilities", "agents", "learn")

DEFAULT_CATEGORY = "recipes"

#: Where a library save lands when no folder source is configured.
FALLBACK_LIBRARY_DIR = Path.home() / ".circuitry" / "library"

MANIFEST_SCHEMA_VERSION = "2.0.0"

#: The turn contract, as declared by the wizard's own ``interface.outputs``.
#: `tests/tui/test_wizard_host.py` checks these against the YAML.
TURN_PATHS: dict[str, str] = {
    "say": "prime.turn.decide.respond.value.say",
    "yaml": "prime.turn.decide.check.value.yaml",
    "done": "prime.turn.decide.done.value",
    "valid": "prime.turn.decide.check.value.ok",
    "errors": "prime.turn.decide.check.value.errors",
}


def wizard_path() -> Path:
    """Resolve the bundled wizard orchestration, wherever it is installed."""
    from ..cli.registry import resolve_bundled

    path = resolve_bundled(WIZARD_REF)
    if path is None:  # pragma: no cover - only if the package is broken
        raise FileNotFoundError(f"The curation library has no {WIZARD_REF!r} entry.")
    return path


def dig(state: dict[str, Any], path: str) -> Any:
    """Read a dot-path out of a run's final state. Missing → ``None``."""
    cursor: Any = state
    for segment in path.split("."):
        if not isinstance(cursor, dict) or segment not in cursor:
            return None
        cursor = cursor[segment]
    return cursor


# ── the seed form ────────────────────────────────────────────────────────────


def slugify(name: str) -> str:
    """Turn a typed name into a manifest-legal slug (``[a-z][a-z0-9_]*``)."""
    slug = re.sub(r"[^a-z0-9]+", "_", name.strip().lower()).strip("_")
    if slug and slug[0].isdigit():
        slug = f"o_{slug}"
    return slug


@dataclass(frozen=True)
class Seed:
    """The light form that starts a conversation: name, category, goal."""

    name: str = ""
    category: str = DEFAULT_CATEGORY
    goal: str = ""

    @property
    def slug(self) -> str:
        return slugify(self.name)

    def problems(self) -> tuple[str, ...]:
        """Everything wrong with the form, for the seed screen to show."""
        problems: list[str] = []
        if not self.name.strip():
            problems.append("Give it a name.")
        elif not self.slug:
            problems.append("The name needs at least one letter or digit.")
        if self.category not in CATEGORIES:
            problems.append(f"Category must be one of: {', '.join(CATEGORIES)}.")
        if not self.goal.strip():
            problems.append("Say in one line what it should do.")
        return tuple(problems)

    @property
    def ok(self) -> bool:
        return not self.problems()


# ── one turn ─────────────────────────────────────────────────────────────────


@dataclass(frozen=True)
class Message:
    """One conversation turn, in the shape the wizard's ``conversation`` takes."""

    role: str  # "user" | "wizard"
    content: str

    def as_state(self) -> dict[str, str]:
        return {"role": self.role, "content": self.content}


@dataclass(frozen=True)
class Turn:
    """What one wizard run hands back — the turn contract, unpacked."""

    say: str
    yaml: Optional[str] = None
    done: bool = False
    valid: Optional[bool] = None
    errors: tuple[str, ...] = ()

    @classmethod
    def from_state(cls, state: dict[str, Any]) -> "Turn":
        raw_errors = dig(state, TURN_PATHS["errors"]) or []
        yaml_text = dig(state, TURN_PATHS["yaml"])
        valid = dig(state, TURN_PATHS["valid"])
        return cls(
            say=str(dig(state, TURN_PATHS["say"]) or ""),
            yaml=yaml_text if isinstance(yaml_text, str) and yaml_text.strip() else None,
            done=bool(dig(state, TURN_PATHS["done"])),
            valid=valid if isinstance(valid, bool) else None,
            errors=tuple(str(e) for e in raw_errors) if isinstance(raw_errors, list) else (),
        )


#: A host-supplied "run one turn" function. The chat screen only ever calls
#: this, which is what keeps it adapter-agnostic: the default goes through
#: ``api.run_orchestration`` and whatever adapter the config resolves; a test
#: passes one wired to a scripted fake.
TurnRunner = Callable[[dict[str, Any]], Turn]


def run_turn(
    state: dict[str, Any],
    *,
    config: Optional["CircuitryConfig"] = None,
    adapter: Optional["Adapter"] = None,
    verbose: bool = False,
) -> Turn:
    """Run the wizard once over ``{goal, conversation, draft}``."""
    from ..api import run_orchestration

    result = run_orchestration(
        orchestration_path=wizard_path(),
        state=state,
        config=config,
        adapter=adapter,
        verbose=verbose,
    )
    return Turn.from_state(result.state)


def default_runner(
    *,
    config: Optional["CircuitryConfig"] = None,
    adapter: Optional["Adapter"] = None,
) -> TurnRunner:
    """A runner over the config on disk — what the view uses when unconfigured."""

    def _run(state: dict[str, Any]) -> Turn:
        resolved = config
        if resolved is None:
            from ..cli.config import resolve_config

            resolved = resolve_config()
        return run_turn(state, config=resolved, adapter=adapter)

    return _run


# ── the verdict on a draft ───────────────────────────────────────────────────


@dataclass(frozen=True)
class DraftStatus:
    """The validator's verdict on one draft — what the YAML pane renders."""

    ok: bool
    errors: tuple[str, ...] = ()

    def headline(self) -> str:
        if self.ok:
            return "✔ Valid"
        count = len(self.errors)
        return f"✘ {count} problem{'' if count == 1 else 's'}"


def validate_draft(text: str) -> DraftStatus:
    """Validate a draft with the gate ``cof check`` runs.

    Structure only — schema, allowlist-free, compile, cycles. Preflight is
    skipped on purpose: whether *this* machine can reach the adapter says
    nothing about whether the document the human is drafting is well-formed.
    """
    if not text.strip():
        return DraftStatus(False, ("The draft is empty.",))

    from ..cli.runtime_shim import validate

    with tempfile.TemporaryDirectory(prefix="circuitry-draft-") as tmp:
        path = Path(tmp) / "draft.yml"
        path.write_text(text, encoding="utf-8")
        report = validate(path, skip_preflight=True)

    raw = report.get("errors") or []
    errors = tuple(str(e) for e in raw) if isinstance(raw, list) else (str(raw),)
    return DraftStatus(bool(report.get("ok")), errors)


# ── the conversation ─────────────────────────────────────────────────────────


@dataclass
class Conversation:
    """The host's whole job: transcript in, state out, verdict on the draft."""

    seed: Seed
    messages: list[Message] = field(default_factory=list)
    draft: str = ""
    status: Optional[DraftStatus] = None
    done: bool = False

    def state(self) -> dict[str, Any]:
        """The input state for the next run of the wizard."""
        return {
            "goal": self.seed.goal,
            "conversation": [m.as_state() for m in self.messages],
            "draft": self.draft,
        }

    def add_user(self, text: str) -> Message:
        message = Message("user", text)
        self.messages.append(message)
        return message

    def record(self, turn: Turn, *, validator: Callable[[str], DraftStatus] = validate_draft) -> None:
        """Fold a turn into the conversation, re-validating any draft it carries.

        The wizard reports its own ``valid`` flag; the pane never shows it.
        The draft is put back through the validator here, so what the human
        sees is what the file on disk would be judged by.
        """
        self.messages.append(Message("wizard", turn.say))
        if turn.yaml is not None:
            self.draft = turn.yaml
            self.status = validator(turn.yaml)
        self.done = turn.done and self.can_save

    @property
    def can_save(self) -> bool:
        """A draft may be saved only once the validator has passed it."""
        return bool(self.draft) and self.status is not None and self.status.ok


# ── saving ───────────────────────────────────────────────────────────────────


class InvalidDraft(RuntimeError):
    """Raised instead of writing a draft the validator rejects."""

    def __init__(self, status: DraftStatus) -> None:
        super().__init__("; ".join(status.errors) or "The draft is not valid.")
        self.status = status


@dataclass(frozen=True)
class LibrarySave:
    """Where a library save landed, and under what name."""

    path: Path
    manifest_path: Path
    entry: dict[str, Any]

    @property
    def name(self) -> str:
        return str(self.entry["name"])


def _checked(draft: str) -> str:
    status = validate_draft(draft)
    if not status.ok:
        raise InvalidDraft(status)
    return draft if draft.endswith("\n") else draft + "\n"


def save_to_file(draft: str, path: Path) -> Path:
    """Write a validated draft to ``path``. Invalid drafts raise."""
    text = _checked(draft)
    path = path.expanduser()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    return path


def default_library_dir(config: Optional["CircuitryConfig"] = None) -> Path:
    """The local library: the first configured folder source, else the fallback.

    A folder source is what makes a saved orchestration reachable as
    ``cof run <name>``, so saving into the one already configured is what the
    human means by "my library". With none configured we fall back to
    ``~/.circuitry/library``, which they can then point a source at.
    """
    try:
        from ..cli.library_sources import FolderSource, LibraryRegistry

        registry = LibraryRegistry.from_config(config)
        for source in registry.sources:
            if isinstance(source, FolderSource):
                return source.path
    except Exception:  # noqa: BLE001 - a broken config must not block saving
        pass
    return FALLBACK_LIBRARY_DIR


def _mapping(value: Any) -> dict[str, Any]:
    """``value`` if it is a mapping, else an empty one."""
    return value if isinstance(value, dict) else {}


def manifest_entry(draft: str, seed: Seed) -> dict[str, Any]:
    """Build a curation-manifest-shaped entry describing ``draft``.

    Everything the schema requires (``name``, ``file``, ``category``,
    ``intent``) comes from the seed form; the rest is read off the document so
    the entry describes the orchestration rather than repeating the goal.
    """
    import yaml as _yaml

    try:
        parsed = _yaml.safe_load(draft) or {}
    except _yaml.YAMLError:  # pragma: no cover - a parsed draft got here
        parsed = {}
    if not isinstance(parsed, dict):
        parsed = {}

    interface = _mapping(parsed.get("interface"))
    inputs = _mapping(interface.get("inputs"))
    outputs = _mapping(interface.get("outputs"))

    slug = seed.slug
    entry: dict[str, Any] = {
        "name": f"{seed.category}/{slug}",
        "file": f"{seed.category}/{slug}.yml",
        "category": seed.category,
        "intent": seed.goal.strip(),
        "description": seed.goal.strip(),
        "primitives": _primitives(parsed),
        "backends": ["llm"],
        "inputs": inputs,
        "tags": ["wizard"],
        "example": f"cof run {seed.category}/{slug}",
    }
    if outputs:
        entry["outputs"] = outputs
    return entry


#: Effect types the manifest's ``primitives`` field names. Anything else with a
#: ``type`` key in the document (a JSON Schema's ``type: string``, say) is not
#: an effect and must not leak into the entry.
PRIMITIVES: tuple[str, ...] = (
    "prompt",
    "dynamic",
    "if",
    "conditional",
    "loop",
    "tool",
    "use",
    "reflector",
)


def _primitives(parsed: dict[str, Any]) -> list[str]:
    """Every effect type used in the document, in first-seen order."""
    found: list[str] = []

    def walk(node: Any) -> None:
        if isinstance(node, dict):
            kind = node.get("type")
            if isinstance(kind, str) and kind in PRIMITIVES and kind not in found:
                found.append(kind)
            for value in node.values():
                walk(value)
        elif isinstance(node, list):
            for item in node:
                walk(item)

    walk(parsed.get("effects"))
    return found


def save_to_library(
    draft: str,
    seed: Seed,
    *,
    library_dir: Path,
) -> LibrarySave:
    """Write a validated draft into the local library and index it.

    ``<library_dir>/<category>/<slug>.yml`` plus an entry in the folder's
    ``manifest.json`` — the same shape ``FolderSource`` reads and the curation
    manifest schema validates. Re-saving the same name replaces its entry
    rather than appending a duplicate.
    """
    text = _checked(draft)
    library_dir = Path(library_dir).expanduser()

    entry = manifest_entry(draft, seed)
    path = library_dir / str(entry["file"])
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")

    manifest_path = library_dir / "manifest.json"
    manifest = _load_manifest(manifest_path)
    entries = [e for e in manifest["entries"] if e.get("name") != entry["name"]]
    entries.append(entry)
    manifest["entries"] = entries
    manifest_path.write_text(
        json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    return LibrarySave(path=path, manifest_path=manifest_path, entry=entry)


def _load_manifest(path: Path) -> dict[str, Any]:
    """The folder's manifest, or a fresh one. A broken file is replaced."""
    fresh: dict[str, Any] = {"schema_version": MANIFEST_SCHEMA_VERSION, "entries": []}
    if not path.exists():
        return fresh
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return fresh
    if not isinstance(data, dict) or not isinstance(data.get("entries"), list):
        return fresh
    data.setdefault("schema_version", MANIFEST_SCHEMA_VERSION)
    data["entries"] = [e for e in data["entries"] if isinstance(e, dict)]
    return data
