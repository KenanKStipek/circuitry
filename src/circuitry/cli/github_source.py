"""GitHub library source — publish orchestrations by PR, consume from a cache.

A `github` source points at a subtree of a GitHub repository:

```json
{
  "type": "github",
  "repo": "owner/name",
  "ref": "main",
  "path": "library/",
  "name": "hub",
  "token_env": "GITHUB_TOKEN"
}
```

The network is touched **only** by `refresh()`. Everything else — `cof list`,
`cof info`, `cof run`, `cof eject` — reads the on-disk cache at
`~/.cache/circuitry/library/<source>/<sha>/…`, so the CLI is fully usable
offline and never surprises a run with a silent fetch. An unfetched source is
simply an empty source that carries a "run `cof library refresh`" notice.

`refresh()` resolves `ref` → commit SHA, and re-downloads only when that SHA
differs from the cached one. The cache directory is named after the SHA, which
is what makes a refresh atomic-ish: the new tree is materialised beside the old
one and only becomes live when the index file is rewritten.

Auth is by environment variable *name* (`token_env`) — the config, the cache
index, and every serialised state file therefore hold the name, never the
secret. Public repositories work with no token at all.

Fetching uses the REST contents API over stdlib `urllib`, matching the
convention set by `plugins/http.py` (no `requests` dependency).
"""

from __future__ import annotations

import base64
import json
import os
import shutil
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from .library_sources import (
    FOLDER_SUFFIXES,
    Entry,
    FolderSource,
    LibraryFetchError,
    RefreshResult,
)

GITHUB_API_BASE = "https://api.github.com"

#: Files worth caching: orchestrations plus an optional metadata manifest.
MANIFEST_NAME = "manifest.json"

INDEX_NAME = "index.json"

DEFAULT_REF = "main"

DEFAULT_TIMEOUT_SECONDS = 30

API_VERSION = "2022-11-28"

USER_AGENT = "circuitry-cof"


def default_cache_root() -> Path:
    """`~/.cache/circuitry/library`, honouring `XDG_CACHE_HOME`.

    `CIRCUITRY_CACHE_DIR` overrides both — it points at the *library* cache
    root directly, which is what tests and sandboxed environments want.
    """
    override = os.environ.get("CIRCUITRY_CACHE_DIR")
    if override:
        return Path(override).expanduser()
    xdg = os.environ.get("XDG_CACHE_HOME")
    base = Path(xdg).expanduser() if xdg else Path.home() / ".cache"
    return base / "circuitry" / "library"


class GitHubSource:
    """A GitHub repository subtree, consumed from a SHA-pinned local cache."""

    def __init__(
        self,
        *,
        name: str,
        repo: str,
        ref: str = DEFAULT_REF,
        path: str = "",
        token_env: str | None = None,
        cache_root: Path | None = None,
        api_base: str = GITHUB_API_BASE,
        timeout_seconds: int = DEFAULT_TIMEOUT_SECONDS,
    ) -> None:
        self.name = name
        self.repo = repo
        self.ref = ref or DEFAULT_REF
        self.path = path.strip("/")
        self.token_env = token_env or None
        self.api_base = api_base.rstrip("/")
        self.timeout_seconds = timeout_seconds
        self._cache_root = cache_root
        self._folder: FolderSource | None = None
        self._folder_dir: Path | None = None

    # -- cache layout -------------------------------------------------------

    @property
    def cache_root(self) -> Path:
        return self._cache_root if self._cache_root is not None else default_cache_root()

    @property
    def cache_dir(self) -> Path:
        """Per-source cache directory: `<root>/<source name>`."""
        return self.cache_root / _safe_dir_name(self.name)

    @property
    def index_path(self) -> Path:
        return self.cache_dir / INDEX_NAME

    def read_index(self) -> dict[str, Any] | None:
        """The cache index (`sha`, `fetched_at`, …), or None when unfetched."""
        try:
            data = json.loads(self.index_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return None
        return data if isinstance(data, dict) else None

    @property
    def cached_sha(self) -> str | None:
        index = self.read_index()
        if index is None:
            return None
        sha = index.get("sha")
        return str(sha) if sha else None

    @property
    def entries_dir(self) -> Path | None:
        """Directory holding the cached subtree, or None when unfetched."""
        sha = self.cached_sha
        if not sha:
            return None
        candidate = self.cache_dir / sha
        return candidate if candidate.is_dir() else None

    # -- LibrarySource protocol --------------------------------------------

    def list_entries(self) -> list[Entry]:
        """Entries from the cache only — never a network call."""
        folder = self._folder_source()
        if folder is None:
            return []
        return folder.list_entries()

    def resolve(self, ref: str) -> Path | None:
        folder = self._folder_source()
        if folder is None:
            return None
        return folder.resolve(ref)

    def notice(self) -> str | None:
        """A user-facing hint when the cache cannot serve this source yet."""
        if self.entries_dir is not None:
            return None
        return (
            f"Library source {self.name!r} ({self.repo}@{self.ref}) has not been "
            f"fetched yet — run `cof library refresh {self.name}`."
        )

    # -- refresh ------------------------------------------------------------

    def refresh(self) -> RefreshResult:
        """Resolve `ref` → SHA and re-download the subtree when it changed."""
        sha = self._resolve_sha()
        if sha == self.cached_sha and (self.cache_dir / sha).is_dir():
            return RefreshResult(
                source=self.name,
                status="unchanged",
                sha=sha,
                detail=f"{self.repo}@{self.ref} already cached",
            )

        files = self._download_subtree(sha)
        self._write_cache(sha=sha, files=files)
        self._folder = None
        self._folder_dir = None
        return RefreshResult(
            source=self.name,
            status="updated",
            sha=sha,
            detail=f"{len(files)} file(s) from {self.repo}@{self.ref}",
        )

    # -- internals ----------------------------------------------------------

    def _folder_source(self) -> FolderSource | None:
        """A `FolderSource` over the cached subtree — cached-tree reuse.

        The cache is laid out exactly like a folder source (YAML files plus an
        optional `manifest.json`), so listing, metadata derivation, and name
        matching are the folder source's behaviour verbatim.
        """
        directory = self.entries_dir
        if directory is None:
            self._folder = None
            self._folder_dir = None
            return None
        if self._folder is None or self._folder_dir != directory:
            self._folder = FolderSource(self.name, directory)
            self._folder_dir = directory
        return self._folder

    def _headers(self) -> dict[str, str]:
        headers = {
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": API_VERSION,
            "User-Agent": USER_AGENT,
        }
        token = self._token()
        if token:
            headers["Authorization"] = f"Bearer {token}"
        return headers

    def _token(self) -> str | None:
        if not self.token_env:
            return None
        value = os.environ.get(self.token_env, "").strip()
        return value or None

    def _resolve_sha(self) -> str:
        """`ref` (branch, tag, or SHA) → the commit SHA it currently points at."""
        payload = self._get_json(f"/repos/{self.repo}/commits/{_quote(self.ref)}")
        if isinstance(payload, dict):
            sha = payload.get("sha")
            if isinstance(sha, str) and sha:
                return sha
        raise LibraryFetchError(
            f"Library source {self.name!r}: GitHub did not return a commit SHA for "
            f"{self.repo}@{self.ref}."
        )

    def _download_subtree(self, sha: str) -> dict[str, bytes]:
        """Walk the contents API from `path`, returning `{relpath: bytes}`."""
        files: dict[str, bytes] = {}
        self._walk(self.path, sha=sha, files=files)
        if not files:
            where = f"{self.repo}@{sha[:7]}" + (f":{self.path}" if self.path else "")
            raise LibraryFetchError(
                f"Library source {self.name!r}: no orchestrations (*.yml / *.yaml) "
                f"found under {where}. Check the source's 'path'."
            )
        return files

    def _walk(self, subpath: str, *, sha: str, files: dict[str, bytes]) -> None:
        listing = self._contents(subpath, sha=sha)
        if isinstance(listing, dict):
            # The contents API returns an object (not an array) when `path`
            # names a single file; treat that as a one-file subtree.
            item = listing
            name = str(item.get("name") or "")
            if _is_wanted(name):
                files[_safe_relative_name(name)] = self._file_bytes(item, sha=sha)
            return

        for item in listing:
            if not isinstance(item, dict):
                continue
            item_type = str(item.get("type") or "")
            item_path = str(item.get("path") or "")
            if item_type == "dir":
                self._walk(item_path, sha=sha, files=files)
            elif item_type == "file" and _is_wanted(str(item.get("name") or "")):
                rel = _relative_to(self.path, item_path)
                files[rel] = self._file_bytes(item, sha=sha)

    def _contents(self, subpath: str, *, sha: str) -> Any:
        suffix = f"/{_quote_path(subpath)}" if subpath else ""
        return self._get_json(f"/repos/{self.repo}/contents{suffix}?ref={_quote(sha)}")

    def _file_bytes(self, item: dict[str, Any], *, sha: str) -> bytes:
        """Fetch one blob through the contents API (base64-encoded body)."""
        encoded = item.get("content")
        if not isinstance(encoded, str) or not encoded:
            payload = self._contents(str(item.get("path") or ""), sha=sha)
            encoded = payload.get("content") if isinstance(payload, dict) else None
        if not isinstance(encoded, str):
            raise LibraryFetchError(
                f"Library source {self.name!r}: no content returned for "
                f"{item.get('path')!r}."
            )
        try:
            return base64.b64decode(encoded)
        except (ValueError, TypeError) as exc:
            raise LibraryFetchError(
                f"Library source {self.name!r}: could not decode {item.get('path')!r} "
                f"({exc})."
            ) from exc

    def _get_json(self, endpoint: str) -> Any:
        url = f"{self.api_base}{endpoint}"
        req = urllib.request.Request(url, method="GET")
        for key, value in self._headers().items():
            req.add_header(key, value)

        try:
            with urllib.request.urlopen(req, timeout=self.timeout_seconds) as resp:
                body = resp.read().decode("utf-8", errors="replace")
        except urllib.error.HTTPError as exc:
            raise self._http_error(exc, url) from exc
        except urllib.error.URLError as exc:
            raise LibraryFetchError(
                f"Library source {self.name!r}: could not reach {url} ({exc.reason}). "
                "The cache is still usable offline; only `cof library refresh` "
                "needs the network."
            ) from exc

        try:
            return json.loads(body) if body else None
        except json.JSONDecodeError as exc:
            raise LibraryFetchError(
                f"Library source {self.name!r}: {url} returned a non-JSON response."
            ) from exc

    def _http_error(self, exc: urllib.error.HTTPError, url: str) -> LibraryFetchError:
        """Turn an HTTP status into something the user can act on."""
        status = int(exc.code)
        headers = {k.lower(): v for k, v in (exc.headers or {}).items()}
        prefix = f"Library source {self.name!r}: "

        if status in (401, 403) and headers.get("x-ratelimit-remaining") == "0":
            reset = _format_reset(headers.get("x-ratelimit-reset"))
            hint = (
                f"Set {self.token_env} in the environment to raise the limit."
                if self.token_env
                else "Add a 'token_env' to this source and export a token to raise the limit."
            )
            when = f" Limit resets at {reset}." if reset else ""
            return LibraryFetchError(
                f"{prefix}GitHub API rate limit exceeded for {self.repo}.{when} {hint}"
            )

        if status == 401:
            return LibraryFetchError(
                f"{prefix}GitHub rejected the credentials for {self.repo} (401). "
                + self._token_hint(invalid=True)
            )

        if status == 403:
            return LibraryFetchError(
                f"{prefix}access to {self.repo} is forbidden (403). "
                + self._token_hint(invalid=False)
            )

        if status == 404:
            target = f"{self.repo}@{self.ref}" + (f":{self.path}" if self.path else "")
            return LibraryFetchError(
                f"{prefix}{target} was not found (404). Check 'repo', 'ref', and "
                "'path' — a private repository also reports 404 when unauthenticated. "
                + self._token_hint(invalid=False)
            )

        return LibraryFetchError(f"{prefix}GET {url} failed with HTTP {status}: {exc.reason}")

    def _token_hint(self, *, invalid: bool) -> str:
        if not self.token_env:
            return (
                "No 'token_env' is configured for this source; add one and export a "
                "GitHub token with repo read access."
            )
        if self._token() is None:
            return f"${self.token_env} is not set in the environment."
        verb = "is set but was rejected" if invalid else "is set"
        return f"${self.token_env} {verb}; confirm the token has read access."

    def _write_cache(self, *, sha: str, files: dict[str, bytes]) -> None:
        """Materialise the subtree beside the old one, then flip the index."""
        self.cache_dir.mkdir(parents=True, exist_ok=True)
        staging = self.cache_dir / f".{sha}.partial"
        shutil.rmtree(staging, ignore_errors=True)
        try:
            for rel, blob in files.items():
                dest = staging / rel
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(blob)

            final = self.cache_dir / sha
            shutil.rmtree(final, ignore_errors=True)
            staging.rename(final)
        finally:
            shutil.rmtree(staging, ignore_errors=True)

        self.index_path.write_text(
            json.dumps(
                {
                    "source": self.name,
                    "repo": self.repo,
                    "ref": self.ref,
                    "path": self.path,
                    "sha": sha,
                    "fetched_at": datetime.now(timezone.utc).isoformat(),
                    "file_count": len(files),
                },
                indent=2,
                sort_keys=True,
            )
            + "\n",
            encoding="utf-8",
        )
        self._prune(keep=sha)

    def _prune(self, *, keep: str) -> None:
        """Drop superseded SHA directories so the cache stays bounded."""
        for child in self.cache_dir.iterdir():
            if child.is_dir() and child.name != keep:
                shutil.rmtree(child, ignore_errors=True)


# ── helpers ──────────────────────────────────────────────────────────────────


def _is_wanted(filename: str) -> bool:
    return filename.endswith(FOLDER_SUFFIXES) or filename == MANIFEST_NAME


def _relative_to(root: str, item_path: str) -> str:
    """Path relative to the source's `path`, validated against traversal."""
    rel = item_path[len(root) :] if root and item_path.startswith(root) else item_path
    return _safe_relative_name(rel, original=item_path)


def _safe_relative_name(rel: str, *, original: str | None = None) -> str:
    """Normalise a cache-relative path, refusing anything that could escape."""
    parts = [p for p in rel.lstrip("/").split("/") if p not in ("", ".")]
    if not parts or any(p == ".." for p in parts):
        raise LibraryFetchError(
            f"Refusing to cache unsafe repository path: {(original or rel)!r}"
        )
    return "/".join(parts)


def _safe_dir_name(name: str) -> str:
    """A source name is user-supplied; keep it from escaping the cache root."""
    cleaned = "".join(c if (c.isalnum() or c in "-_.") else "_" for c in name)
    return cleaned.strip(".") or "source"


def _quote(value: str) -> str:
    return urllib.parse.quote(value, safe="")


def _quote_path(value: str) -> str:
    return urllib.parse.quote(value, safe="/")


def _format_reset(raw: str | None) -> str:
    if not raw:
        return ""
    try:
        stamp = datetime.fromtimestamp(int(raw), tz=timezone.utc)
    except (ValueError, OSError, OverflowError):
        return ""
    return stamp.strftime("%Y-%m-%d %H:%M:%SZ")
