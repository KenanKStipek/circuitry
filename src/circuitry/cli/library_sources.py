"""Library source registry — where `cof list/info/run/eject` find orchestrations.

A *library source* is anything that can enumerate orchestration entries and
resolve a name to a file on disk. The bundled curation library is one source;
a folder of local `*.yml` files is another; a GitHub repository subtree (served
from a SHA-pinned local cache) is a third. Sources are declared in config under
`runtime.library.sources` and are ordered — earlier entries win when a bare
name matches in more than one source.

```json
{
  "runtime": {
    "library": {
      "sources": [
        {"type": "curation"},
        {"type": "folder", "name": "local", "path": "./orchestrations"},
        {"type": "github", "name": "hub", "repo": "owner/name", "path": "library/"}
      ]
    }
  }
}
```

Only `refresh()` may touch the network; `list_entries()`/`resolve()` are always
local reads, which keeps every other command usable offline.

When `sources` is absent the registry defaults to `[{"type": "curation"}]`,
which makes zero-config behaviour byte-identical to the pre-registry CLI.

Names may be *source-qualified* as `"<source>:<name>"` (e.g. `local:my_pipeline`),
which bypasses precedence entirely. Bare names search sources in order.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional, Protocol, runtime_checkable

import yaml  # type: ignore[import-untyped]

from .config import CircuitryConfig
from .registry import _curation_dir, _name_matches, _normalise_entry, load_index

FOLDER_SUFFIXES = (".yml", ".yaml")

CURATION_SOURCE_NAME = "curation"

DEFAULT_SOURCES: list[dict[str, Any]] = [{"type": "curation"}]


class LibrarySourceError(ValueError):
    """Raised when `runtime.library.sources` is malformed."""


class LibraryFetchError(RuntimeError):
    """Raised when a remote source cannot be refreshed (network, auth, 404…)."""


@dataclass(frozen=True)
class RefreshResult:
    """Outcome of refreshing one source, for `cof library refresh` output."""

    source: str
    status: str  # "updated" | "unchanged" | "skipped"
    sha: Optional[str] = None
    detail: str = ""

    def summary(self) -> str:
        pinned = f" ({self.sha[:7]})" if self.sha else ""
        suffix = f" — {self.detail}" if self.detail else ""
        return f"{self.source}: {self.status}{pinned}{suffix}"


@dataclass(frozen=True)
class Entry:
    """A single orchestration exposed by a library source."""

    name: str
    category: str
    metadata: dict[str, Any] = field(default_factory=dict)
    source: str = ""
    path: Optional[Path] = None

    @property
    def qualified_name(self) -> str:
        """`"<source>:<name>"` — always unambiguous."""
        return f"{self.source}:{self.name}"

    def as_dict(self, *, include_source: bool) -> dict[str, Any]:
        """Legacy index-shaped dict, optionally carrying the source name.

        The un-sourced shape is byte-identical to what `registry.load_index()`
        produced before sources existed, which is what keeps `cof list --json`
        stable for zero-config users.
        """
        payload = dict(self.metadata)
        if include_source:
            payload["source"] = self.source
        return payload


@runtime_checkable
class LibrarySource(Protocol):
    """Anything that can enumerate and resolve orchestrations."""

    name: str

    def list_entries(self) -> list[Entry]:
        """Return every entry this source exposes, in display order."""
        ...

    def resolve(self, ref: str) -> Optional[Path]:
        """Resolve a (source-local, unqualified) name to a file path."""
        ...


# ── curation source ──────────────────────────────────────────────────────────


class CurationSource:
    """The bundled curation library at `src/circuitry/curation/`."""

    def __init__(self, name: str = CURATION_SOURCE_NAME) -> None:
        self.name = name

    def list_entries(self) -> list[Entry]:
        root = _curation_dir()
        entries: list[Entry] = []
        for raw in load_index():
            rel = str(raw.get("file", ""))
            entries.append(
                Entry(
                    name=str(raw.get("name", "")),
                    category=str(raw.get("category", "")),
                    metadata=raw,
                    source=self.name,
                    path=(root / rel) if rel else None,
                )
            )
        return entries

    def resolve(self, ref: str) -> Optional[Path]:
        # Delegate to the original resolver so curation behaviour — manifest
        # lookup, then the slash-delimited filesystem walk — is unchanged.
        from .registry import resolve_bundled

        return resolve_bundled(ref)

    def refresh(self) -> None:  # pragma: no cover - nothing is cached
        return None


# ── folder source ────────────────────────────────────────────────────────────


class FolderSource:
    """A directory of `*.yml` / `*.yaml` orchestrations, scanned recursively.

    Metadata comes from an optional `manifest.json` at the folder root (same
    shape as the curation manifest: `{"entries": [...]}`). Files not covered by
    the manifest — or every file, when there is no manifest — get metadata
    derived from the YAML itself: description from the leading comment block or
    the first prompt template, inputs from `interface.inputs`.
    """

    def __init__(self, name: str, path: Path) -> None:
        self.name = name
        self.path = path
        self._cache: Optional[list[Entry]] = None

    def refresh(self) -> None:
        """Drop the cached scan so the next `list_entries()` re-reads disk."""
        self._cache = None

    def list_entries(self) -> list[Entry]:
        if self._cache is None:
            self._cache = self._scan()
        return list(self._cache)

    def resolve(self, ref: str) -> Optional[Path]:
        for entry in self.list_entries():
            stem = entry.path.stem if entry.path is not None else ""
            if _name_matches(ref, entry.name, stem):
                return entry.path
        return None

    # -- internals ----------------------------------------------------------

    def _scan(self) -> list[Entry]:
        if not self.path.exists() or not self.path.is_dir():
            return []

        manifest = self._load_manifest()
        entries: list[Entry] = []
        for file_path in self._orchestration_files():
            rel = file_path.relative_to(self.path).as_posix()
            name = rel.rsplit(".", 1)[0]
            raw = manifest.get(rel) or manifest.get(name)
            if raw is None:
                metadata = derive_metadata(file_path, name=name, rel=rel)
            else:
                metadata = _normalise_entry({**raw, "file": rel, "name": raw.get("name") or name})
            entries.append(
                Entry(
                    name=str(metadata.get("name") or name),
                    category=str(metadata.get("category") or ""),
                    metadata=metadata,
                    source=self.name,
                    path=file_path,
                )
            )
        return entries

    def _orchestration_files(self) -> list[Path]:
        found: list[Path] = []
        for suffix in FOLDER_SUFFIXES:
            found.extend(self.path.rglob(f"*{suffix}"))
        return sorted(
            (p for p in found if p.is_file()),
            key=lambda p: p.relative_to(self.path).as_posix(),
        )

    def _load_manifest(self) -> dict[str, dict[str, Any]]:
        """Index the folder's optional `manifest.json` by `file` and by `name`."""
        manifest_path = self.path / "manifest.json"
        if not manifest_path.exists():
            return {}
        try:
            data = json.loads(manifest_path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            return {}
        if not isinstance(data, dict):
            return {}
        raw_entries = data.get("entries") or data.get("orchestrations") or []
        if not isinstance(raw_entries, list):
            return {}

        indexed: dict[str, dict[str, Any]] = {}
        for raw in raw_entries:
            if not isinstance(raw, dict):
                continue
            file_key = str(raw.get("file") or "")
            name_key = str(raw.get("name") or "")
            if file_key:
                indexed[file_key] = raw
            if name_key:
                indexed.setdefault(name_key, raw)
        return indexed


def derive_metadata(path: Path, *, name: str, rel: str) -> dict[str, Any]:
    """Build an index-shaped metadata dict for an unmanifested orchestration."""
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        text = ""

    try:
        data = yaml.safe_load(text) or {}
    except yaml.YAMLError:
        data = {}
    if not isinstance(data, dict):
        data = {}

    category = rel.rsplit("/", 1)[0] if "/" in rel else ""
    return _normalise_entry(
        {
            "name": name,
            "file": rel,
            "category": category,
            "description": _derive_description(text, data),
            "inputs": _derive_inputs(data),
            "backends": [],
        }
    )


def _derive_description(text: str, data: dict[str, Any]) -> str:
    """Leading comment block first, then the first prompt template."""
    comment = _leading_comment(text)
    if comment:
        return comment

    interface = data.get("interface")
    if isinstance(interface, dict):
        described = interface.get("description")
        if isinstance(described, str) and described.strip():
            return described.strip()

    for effect in data.get("effects") or []:
        if not isinstance(effect, dict):
            continue
        template = effect.get("template")
        if isinstance(template, str) and template.strip():
            return _first_line(template)
    return ""


def _leading_comment(text: str) -> str:
    """First non-empty line of the file's opening `#` comment block."""
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        if not stripped.startswith("#"):
            return ""
        body = stripped.lstrip("#").strip()
        if body:
            return body
    return ""


def _first_line(value: str) -> str:
    for line in value.splitlines():
        if line.strip():
            return line.strip()
    return ""


def _derive_inputs(data: dict[str, Any]) -> dict[str, Any]:
    interface = data.get("interface")
    if not isinstance(interface, dict):
        return {}
    inputs = interface.get("inputs")
    if not isinstance(inputs, dict):
        return {}
    return inputs


# ── registry ─────────────────────────────────────────────────────────────────


@dataclass(frozen=True)
class Resolution:
    """The winning entry for a reference, plus any sources it also matched."""

    entry: Entry
    ambiguous_sources: list[str] = field(default_factory=list)

    @property
    def path(self) -> Optional[Path]:
        return self.entry.path

    @property
    def is_ambiguous(self) -> bool:
        return bool(self.ambiguous_sources)

    def ambiguity_warning(self, ref: str) -> str:
        others = ", ".join(f"{s}:{self.entry.name}" for s in self.ambiguous_sources)
        return (
            f"{ref!r} matched multiple sources; using "
            f"{self.entry.qualified_name} (also in: {others}). "
            "Qualify the name to be explicit."
        )


class LibraryRegistry:
    """Ordered collection of library sources. Order *is* precedence."""

    def __init__(self, sources: list[LibrarySource]) -> None:
        self.sources = sources

    # -- construction -------------------------------------------------------

    @classmethod
    def from_config(cls, cfg: Optional[CircuitryConfig] = None) -> "LibraryRegistry":
        """Build from `runtime.library.sources`, defaulting to curation-only."""
        raw_sources = _configured_sources(cfg)
        return cls([_build_source(spec, index) for index, spec in enumerate(raw_sources)])

    @classmethod
    def from_runtime(cls, runtime: Optional[dict[str, Any]] = None) -> "LibraryRegistry":
        """Build from a bare `runtime` mapping rather than a `CircuitryConfig`.

        The core runtime only ever carries the resolved `runtime` dict (see
        `effective_settings`), so this is the entry point `use ref:` resolution
        and static cycle detection use.
        """
        return cls.from_config(CircuitryConfig(runtime=dict(runtime or {})))

    @classmethod
    def default(cls) -> "LibraryRegistry":
        return cls([CurationSource()])

    # -- queries ------------------------------------------------------------

    @property
    def source_names(self) -> list[str]:
        return [s.name for s in self.sources]

    @property
    def is_multi_source(self) -> bool:
        """True when more than one source is configured.

        `cof list` only grows a Source column (and `--json` a `source` key) in
        this case, so the zero-config output stays exactly as it was.
        """
        return len(self.sources) > 1

    def get_source(self, name: str) -> Optional[LibrarySource]:
        for source in self.sources:
            if source.name == name:
                return source
        return None

    def list_entries(self, *, source: Optional[str] = None) -> list[Entry]:
        """Aggregate entries across sources, in source precedence order."""
        entries: list[Entry] = []
        for candidate in self.sources:
            if source is not None and candidate.name != source:
                continue
            entries.extend(candidate.list_entries())
        return entries

    def split_ref(self, ref: str) -> tuple[Optional[str], str]:
        """Split `"<source>:<name>"` when the prefix names a configured source.

        Anything else (including Windows paths and URLs) comes back unqualified,
        so a stray colon can never be mistaken for a source qualifier.
        """
        if ":" not in ref:
            return (None, ref)
        prefix, _, rest = ref.partition(":")
        if prefix in self.source_names and rest:
            return (prefix, rest)
        return (None, ref)

    def resolve(self, ref: str) -> Optional[Resolution]:
        """Resolve a bare or source-qualified reference to a `Resolution`."""
        source_name, bare = self.split_ref(ref)

        matches: list[Entry] = []
        for source in self.sources:
            if source_name is not None and source.name != source_name:
                continue
            entry = self._resolve_in(source, bare)
            if entry is not None:
                matches.append(entry)

        if not matches:
            return None
        return Resolution(entry=matches[0], ambiguous_sources=[e.source for e in matches[1:]])

    def find_entry(self, ref: str) -> Optional[Entry]:
        resolution = self.resolve(ref)
        return resolution.entry if resolution is not None else None

    def notices(self, *, source: Optional[str] = None) -> list[str]:
        """User-facing hints from sources that cannot serve entries yet."""
        out: list[str] = []
        for candidate in self.sources:
            if source is not None and candidate.name != source:
                continue
            notice = getattr(candidate, "notice", None)
            if not callable(notice):
                continue
            message = notice()
            if message:
                out.append(str(message))
        return out

    def refresh(self, *, source: Optional[str] = None) -> list[RefreshResult]:
        """Refresh every source (or one), returning a result per source.

        Local sources have nothing to fetch and report `skipped`; only remote
        sources do network work, and they do it *only* here.
        """
        results: list[RefreshResult] = []
        for candidate in self.sources:
            if source is not None and candidate.name != source:
                continue
            refresh = getattr(candidate, "refresh", None)
            if not callable(refresh):
                results.append(
                    RefreshResult(source=candidate.name, status="skipped", detail="not refreshable")
                )
                continue
            outcome = refresh()
            if isinstance(outcome, RefreshResult):
                results.append(outcome)
            else:
                results.append(
                    RefreshResult(
                        source=candidate.name,
                        status="skipped",
                        detail="local source — nothing to fetch",
                    )
                )
        return results

    # -- internals ----------------------------------------------------------

    def _resolve_in(self, source: LibrarySource, bare: str) -> Optional[Entry]:
        """Resolve within one source, preferring a listed entry for metadata."""
        path = source.resolve(bare)
        if path is None:
            return None
        for entry in source.list_entries():
            if entry.path is not None and entry.path == path:
                return entry
        # Resolvable but unlisted (e.g. the curation filesystem-walk fallback):
        # synthesise a minimal entry so callers still get a path and a source.
        return Entry(
            name=bare,
            category="",
            metadata={"name": bare, "file": path.name, "description": "", "category": "", "backends": [], "inputs": []},
            source=source.name,
            path=path,
        )


def _configured_sources(cfg: Optional[CircuitryConfig]) -> list[dict[str, Any]]:
    if cfg is None:
        return list(DEFAULT_SOURCES)
    library = (cfg.runtime or {}).get("library")
    if not isinstance(library, dict):
        return list(DEFAULT_SOURCES)
    sources = library.get("sources")
    if sources is None:
        return list(DEFAULT_SOURCES)
    if not isinstance(sources, list) or not sources:
        raise LibrarySourceError(
            "runtime.library.sources must be a non-empty list of source objects."
        )
    out: list[dict[str, Any]] = []
    for spec in sources:
        if not isinstance(spec, dict):
            raise LibrarySourceError(
                f"runtime.library.sources entries must be objects; got {type(spec).__name__}."
            )
        out.append(spec)
    return out


def _build_source(spec: dict[str, Any], index: int) -> LibrarySource:
    source_type = str(spec.get("type") or "").strip().lower()
    if not source_type:
        raise LibrarySourceError(
            f"runtime.library.sources[{index}] is missing required field 'type'."
        )

    if source_type == "curation":
        return CurationSource(name=str(spec.get("name") or CURATION_SOURCE_NAME))

    if source_type == "folder":
        raw_path = str(spec.get("path") or "").strip()
        if not raw_path:
            raise LibrarySourceError(
                f"runtime.library.sources[{index}] (folder) requires a 'path'."
            )
        path = Path(raw_path).expanduser()
        name = str(spec.get("name") or "").strip() or (path.name or "folder")
        return FolderSource(name=name, path=path)

    if source_type == "github":
        return _build_github_source(spec, index)

    raise LibrarySourceError(
        f"Unknown library source type: {source_type!r}. "
        "Supported types: curation, folder, github."
    )


def _build_github_source(spec: dict[str, Any], index: int) -> LibrarySource:
    # Imported here so `library_sources` stays import-cycle-free: the GitHub
    # source depends on FolderSource for reading its cache.
    from .github_source import DEFAULT_REF, GitHubSource

    repo = str(spec.get("repo") or "").strip().strip("/")
    if not repo:
        raise LibrarySourceError(
            f"runtime.library.sources[{index}] (github) requires a 'repo' as 'owner/name'."
        )
    if repo.count("/") != 1 or not all(part for part in repo.split("/")):
        raise LibrarySourceError(
            f"runtime.library.sources[{index}] (github) 'repo' must be 'owner/name'; "
            f"got {repo!r}."
        )

    name = str(spec.get("name") or "").strip() or repo.split("/")[1]
    cache_dir = spec.get("cache_dir")
    return GitHubSource(
        name=name,
        repo=repo,
        ref=str(spec.get("ref") or DEFAULT_REF).strip(),
        path=str(spec.get("path") or "").strip(),
        token_env=str(spec.get("token_env") or "").strip() or None,
        cache_root=Path(str(cache_dir)).expanduser() if cache_dir else None,
        api_base=str(spec.get("api_base") or "").strip() or "https://api.github.com",
    )


def build_registry(
    *, config_path: Optional[Path] = None, cfg: Optional[CircuitryConfig] = None
) -> LibraryRegistry:
    """Convenience constructor used by the CLI commands."""
    if cfg is None:
        from .config import resolve_config

        cfg = resolve_config(explicit_path=config_path)
    return LibraryRegistry.from_config(cfg)
