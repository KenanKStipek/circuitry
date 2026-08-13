"""Tests for the `github` library source.

The network is faked at `urllib.request.urlopen` (same approach as
`tests/plugins/test_http.py`), so these tests exercise the real contents-API
walk, the cache layout, SHA pinning, auth headers, and error surfacing without
ever leaving the process.

The offline guarantee gets its own section: with `urlopen` monkeypatched to
fail, everything except `refresh()` must still work from the cache.
"""

from __future__ import annotations

import base64
import io
import json
import urllib.parse
from pathlib import Path
from typing import Any, Optional
from urllib.error import HTTPError, URLError

import pytest
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.github_source import GitHubSource, default_cache_root
from circuitry.cli.library_sources import (
    LibraryFetchError,
    LibraryRegistry,
    LibrarySourceError,
)

runner = CliRunner()

REPO = "owner/name"
SHA_ONE = "1111111111111111111111111111111111111111"
SHA_TWO = "2222222222222222222222222222222222222222"

PIPELINE_YML = """\
# Hub pipeline — summarises a document.

interface:
  inputs:
    document:
      type: string
      required: true

effects:
  - type: prompt
    name: summarise
    template: "Summarise this: {{document}}"
"""

NESTED_YML = """\
# A nested hub orchestration.

effects:
  - type: prompt
    name: think
    template: "Think about it."
"""

MANIFEST = json.dumps(
    {
        "entries": [
            {
                "name": "my_pipeline",
                "file": "my_pipeline.yml",
                "category": "hub",
                "description": "Manifest-provided description.",
                "backends": ["llm"],
            }
        ]
    }
)

TREE: dict[str, str] = {
    "library/my_pipeline.yml": PIPELINE_YML,
    "library/nested/deep.yml": NESTED_YML,
    "library/manifest.json": MANIFEST,
    "library/README.md": "# not an orchestration",
    "docs/outside.yml": "# outside the configured path",
}


# ── fake GitHub ──────────────────────────────────────────────────────────────


class _FakeResponse:
    def __init__(self, body: bytes) -> None:
        self._body = body
        self.status = 200
        self.headers: dict[str, str] = {"Content-Type": "application/json"}

    def read(self) -> bytes:
        return self._body

    def __enter__(self) -> "_FakeResponse":
        return self

    def __exit__(self, *args: Any) -> None:
        return None


class FakeGitHub:
    """A minimal, realistic stand-in for the GitHub contents API."""

    def __init__(
        self,
        *,
        sha: str = SHA_ONE,
        tree: Optional[dict[str, str]] = None,
        repo: str = REPO,
    ) -> None:
        self.sha = sha
        self.tree = dict(TREE if tree is None else tree)
        self.repo = repo
        self.requests: list[dict[str, Any]] = []
        self.error: Optional[HTTPError] = None
        self.offline = False

    # -- installation ---------------------------------------------------------

    def install(self, monkeypatch: pytest.MonkeyPatch) -> "FakeGitHub":
        def fake_urlopen(req: Any, timeout: int = 0) -> _FakeResponse:
            self.requests.append(
                {
                    "url": req.full_url,
                    "method": req.get_method(),
                    "headers": {k.lower(): v for k, v in req.header_items()},
                }
            )
            if self.offline:
                raise URLError("Network is unreachable")
            if self.error is not None:
                raise self.error
            return _FakeResponse(json.dumps(self._route(req.full_url)).encode("utf-8"))

        monkeypatch.setattr("urllib.request.urlopen", fake_urlopen)
        return self

    # -- routing --------------------------------------------------------------

    def _route(self, url: str) -> Any:
        parsed = urllib.parse.urlsplit(url)
        path = urllib.parse.unquote(parsed.path)
        prefix = f"/repos/{self.repo}/"
        if not path.startswith(prefix):
            raise self._http_error(url, 404)
        rest = path[len(prefix) :]

        if rest.startswith("commits/"):
            return {"sha": self.sha}
        if rest == "contents" or rest.startswith("contents"):
            target = rest[len("contents") :].lstrip("/")
            return self._contents(url, target)
        raise self._http_error(url, 404)

    def _contents(self, url: str, target: str) -> Any:
        if target in self.tree:
            return self._file_object(target, with_content=True)

        prefix = f"{target}/" if target else ""
        children: dict[str, Any] = {}
        for repo_path in sorted(self.tree):
            if not repo_path.startswith(prefix):
                continue
            remainder = repo_path[len(prefix) :]
            head, slash, _ = remainder.partition("/")
            if slash:
                children.setdefault(
                    head,
                    {"name": head, "path": f"{prefix}{head}", "type": "dir"},
                )
            else:
                # Directory listings carry no content — the caller must fetch
                # each blob, exactly like the real API.
                children[head] = self._file_object(repo_path, with_content=False)
        if not children:
            raise self._http_error(url, 404)
        return list(children.values())

    def _file_object(self, repo_path: str, *, with_content: bool) -> dict[str, Any]:
        obj: dict[str, Any] = {
            "name": repo_path.rsplit("/", 1)[-1],
            "path": repo_path,
            "type": "file",
            "size": len(self.tree[repo_path]),
        }
        if with_content:
            obj["encoding"] = "base64"
            obj["content"] = base64.b64encode(
                self.tree[repo_path].encode("utf-8")
            ).decode("ascii")
        return obj

    @staticmethod
    def _http_error(
        url: str, status: int, headers: Optional[dict[str, str]] = None
    ) -> HTTPError:
        return HTTPError(
            url,
            status,
            {401: "Unauthorized", 403: "Forbidden", 404: "Not Found"}.get(status, "Error"),
            headers or {},  # type: ignore[arg-type]
            io.BytesIO(b"{}"),
        )

    # -- assertions helpers ---------------------------------------------------

    def content_requests(self) -> list[str]:
        return [r["url"] for r in self.requests if "/contents" in r["url"]]


def _source(tmp_path: Path, **overrides: Any) -> GitHubSource:
    kwargs: dict[str, Any] = {
        "name": "hub",
        "repo": REPO,
        "ref": "main",
        "path": "library/",
        "cache_root": tmp_path / "cache",
    }
    kwargs.update(overrides)
    return GitHubSource(**kwargs)


def _write_config(path: Path, tmp_path: Path, **overrides: Any) -> Path:
    spec: dict[str, Any] = {
        "type": "github",
        "name": "hub",
        "repo": REPO,
        "ref": "main",
        "path": "library/",
        "cache_dir": str(tmp_path / "cache"),
    }
    spec.update(overrides)
    path.write_text(
        json.dumps({"runtime": {"library": {"sources": [spec]}}}), encoding="utf-8"
    )
    return path


# ── fetch → cache layout ─────────────────────────────────────────────────────


def test_refresh_writes_sha_pinned_cache(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeGitHub().install(monkeypatch)
    source = _source(tmp_path)

    result = source.refresh()

    assert result.status == "updated"
    assert result.sha == SHA_ONE
    cache = tmp_path / "cache" / "hub" / SHA_ONE
    assert (cache / "my_pipeline.yml").read_text(encoding="utf-8") == PIPELINE_YML
    assert (cache / "nested" / "deep.yml").read_text(encoding="utf-8") == NESTED_YML
    assert (cache / "manifest.json").exists()
    # Non-orchestration files and paths outside the source's `path` stay out.
    assert not (cache / "README.md").exists()
    assert not (cache / "outside.yml").exists()


def test_refresh_writes_index_with_sha_and_fetched_at(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeGitHub().install(monkeypatch)
    source = _source(tmp_path)
    source.refresh()

    index = json.loads((tmp_path / "cache" / "hub" / "index.json").read_text("utf-8"))
    assert index["sha"] == SHA_ONE
    assert index["repo"] == REPO
    assert index["ref"] == "main"
    assert index["path"] == "library"
    assert index["file_count"] == 3
    assert index["fetched_at"].endswith("+00:00")


def test_no_partial_directory_survives(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeGitHub().install(monkeypatch)
    _source(tmp_path).refresh()
    names = {p.name for p in (tmp_path / "cache" / "hub").iterdir()}
    assert names == {SHA_ONE, "index.json"}


def test_list_and_resolve_come_from_cache(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeGitHub().install(monkeypatch)
    source = _source(tmp_path)
    source.refresh()

    entries = {e.name: e for e in source.list_entries()}
    assert set(entries) == {"my_pipeline", "nested/deep"}
    assert all(e.source == "hub" for e in entries.values())
    # The cached `manifest.json` supplies metadata, just like a folder source.
    assert entries["my_pipeline"].metadata["description"] == "Manifest-provided description."
    assert entries["my_pipeline"].category == "hub"
    # Unmanifested files still derive metadata from the YAML.
    assert entries["nested/deep"].metadata["description"] == "A nested hub orchestration."

    resolved = source.resolve("my_pipeline")
    assert resolved == tmp_path / "cache" / "hub" / SHA_ONE / "my_pipeline.yml"
    assert source.resolve("nested/deep") is not None
    assert source.resolve("nope") is None


def test_single_file_path_is_supported(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeGitHub().install(monkeypatch)
    source = _source(tmp_path, path="library/my_pipeline.yml")
    source.refresh()
    assert [e.name for e in source.list_entries()] == ["my_pipeline"]


def test_empty_subtree_is_an_actionable_error(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeGitHub(tree={"library/README.md": "# nothing here"}).install(monkeypatch)
    with pytest.raises(LibraryFetchError, match="no orchestrations"):
        _source(tmp_path).refresh()


# ── SHA pinning: update vs no-op ─────────────────────────────────────────────


def test_refresh_no_ops_when_sha_is_unchanged(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = FakeGitHub().install(monkeypatch)
    source = _source(tmp_path)
    source.refresh()
    before = len(fake.content_requests())

    second = source.refresh()

    assert second.status == "unchanged"
    assert second.sha == SHA_ONE
    # Only the ref → SHA lookup went out; no blobs were re-downloaded.
    assert len(fake.content_requests()) == before


def test_refresh_updates_on_new_sha_and_prunes_the_old_tree(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = FakeGitHub().install(monkeypatch)
    source = _source(tmp_path)
    source.refresh()

    fake.sha = SHA_TWO
    fake.tree["library/added.yml"] = "# A newly published orchestration.\n" + NESTED_YML
    result = source.refresh()

    assert result.status == "updated"
    assert result.sha == SHA_TWO
    assert (tmp_path / "cache" / "hub" / SHA_TWO / "added.yml").exists()
    assert not (tmp_path / "cache" / "hub" / SHA_ONE).exists()
    assert "added" in {e.name for e in source.list_entries()}


def test_refresh_recovers_when_cache_tree_is_deleted(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    import shutil

    FakeGitHub().install(monkeypatch)
    source = _source(tmp_path)
    source.refresh()
    shutil.rmtree(tmp_path / "cache" / "hub" / SHA_ONE)

    assert source.list_entries() == []
    assert source.refresh().status == "updated"
    assert source.list_entries() != []


# ── auth ─────────────────────────────────────────────────────────────────────


def test_authorization_header_present_when_token_env_is_set(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = FakeGitHub().install(monkeypatch)
    monkeypatch.setenv("HUB_TOKEN", "s3cret")
    _source(tmp_path, token_env="HUB_TOKEN").refresh()

    assert all(r["headers"].get("authorization") == "Bearer s3cret" for r in fake.requests)


def test_no_authorization_header_without_a_token(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = FakeGitHub().install(monkeypatch)
    monkeypatch.delenv("HUB_TOKEN", raising=False)
    _source(tmp_path, token_env="HUB_TOKEN").refresh()

    assert all("authorization" not in r["headers"] for r in fake.requests)
    assert fake.requests, "expected at least one request"


def test_empty_token_env_var_is_treated_as_unset(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = FakeGitHub().install(monkeypatch)
    monkeypatch.setenv("HUB_TOKEN", "   ")
    _source(tmp_path, token_env="HUB_TOKEN").refresh()
    assert all("authorization" not in r["headers"] for r in fake.requests)


def test_token_value_never_lands_in_the_cache_index(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeGitHub().install(monkeypatch)
    monkeypatch.setenv("HUB_TOKEN", "s3cret")
    _source(tmp_path, token_env="HUB_TOKEN").refresh()

    index_text = (tmp_path / "cache" / "hub" / "index.json").read_text("utf-8")
    assert "s3cret" not in index_text
    assert "token" not in index_text


def test_config_stores_only_the_env_var_name(tmp_path: Path) -> None:
    cfg = CircuitryConfig(
        runtime={
            "library": {
                "sources": [
                    {"type": "github", "repo": REPO, "name": "hub", "token_env": "HUB_TOKEN"}
                ]
            }
        }
    )
    source = LibraryRegistry.from_config(cfg).get_source("hub")
    assert isinstance(source, GitHubSource)
    assert source.token_env == "HUB_TOKEN"


# ── error surfaces ───────────────────────────────────────────────────────────


def test_401_names_the_token_env_var(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = FakeGitHub().install(monkeypatch)
    fake.error = FakeGitHub._http_error("https://api.github.test", 401)
    monkeypatch.setenv("HUB_TOKEN", "bad")

    with pytest.raises(LibraryFetchError) as excinfo:
        _source(tmp_path, token_env="HUB_TOKEN").refresh()
    message = str(excinfo.value)
    assert "401" in message
    assert "$HUB_TOKEN" in message


def test_404_explains_private_repos(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = FakeGitHub().install(monkeypatch)
    fake.error = FakeGitHub._http_error("https://api.github.test", 404)

    with pytest.raises(LibraryFetchError) as excinfo:
        _source(tmp_path).refresh()
    message = str(excinfo.value)
    assert "404" in message
    assert "private repository" in message
    assert "owner/name@main:library" in message


def test_rate_limit_is_distinguished_from_plain_403(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = FakeGitHub().install(monkeypatch)
    fake.error = FakeGitHub._http_error(
        "https://api.github.test",
        403,
        {"X-RateLimit-Remaining": "0", "X-RateLimit-Reset": "1700000000"},
    )

    with pytest.raises(LibraryFetchError) as excinfo:
        _source(tmp_path).refresh()
    message = str(excinfo.value)
    assert "rate limit" in message
    assert "2023-11-14" in message
    assert "'token_env'" in message


def test_rate_limit_names_the_configured_token_env(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = FakeGitHub().install(monkeypatch)
    fake.error = FakeGitHub._http_error(
        "https://api.github.test", 403, {"X-RateLimit-Remaining": "0"}
    )
    with pytest.raises(LibraryFetchError, match="Set HUB_TOKEN"):
        _source(tmp_path, token_env="HUB_TOKEN").refresh()


def test_plain_403_is_reported_as_forbidden(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = FakeGitHub().install(monkeypatch)
    fake.error = FakeGitHub._http_error("https://api.github.test", 403)
    with pytest.raises(LibraryFetchError, match="forbidden"):
        _source(tmp_path).refresh()


def test_missing_sha_in_commit_payload_is_reported(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = FakeGitHub().install(monkeypatch)
    fake.sha = ""
    with pytest.raises(LibraryFetchError, match="did not return a commit SHA"):
        _source(tmp_path).refresh()


# ── offline behaviour ────────────────────────────────────────────────────────


def _fail_urlopen(monkeypatch: pytest.MonkeyPatch) -> None:
    def boom(*args: Any, **kwargs: Any) -> Any:
        raise URLError("Network is unreachable")

    monkeypatch.setattr("urllib.request.urlopen", boom)


def test_everything_but_refresh_works_offline(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeGitHub().install(monkeypatch)
    _source(tmp_path).refresh()

    _fail_urlopen(monkeypatch)
    offline = _source(tmp_path)
    assert {e.name for e in offline.list_entries()} == {"my_pipeline", "nested/deep"}
    assert offline.resolve("my_pipeline") is not None
    assert offline.notice() is None
    assert offline.cached_sha == SHA_ONE


def test_refresh_offline_is_an_actionable_error(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _fail_urlopen(monkeypatch)
    with pytest.raises(LibraryFetchError) as excinfo:
        _source(tmp_path).refresh()
    assert "could not reach" in str(excinfo.value)


def test_unfetched_source_is_empty_and_carries_a_notice(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _fail_urlopen(monkeypatch)
    source = _source(tmp_path)
    assert source.list_entries() == []
    assert source.resolve("my_pipeline") is None
    notice = source.notice()
    assert notice is not None
    assert "cof library refresh hub" in notice


def test_corrupt_index_is_treated_as_unfetched(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _fail_urlopen(monkeypatch)
    source = _source(tmp_path)
    source.cache_dir.mkdir(parents=True)
    source.index_path.write_text("{not json", encoding="utf-8")
    assert source.read_index() is None
    assert source.list_entries() == []


# ── config validation & defaults ─────────────────────────────────────────────


def test_github_source_requires_a_repo() -> None:
    cfg = CircuitryConfig(runtime={"library": {"sources": [{"type": "github"}]}})
    with pytest.raises(LibrarySourceError, match="requires a 'repo'"):
        LibraryRegistry.from_config(cfg)


@pytest.mark.parametrize("repo", ["justname", "a/b/c", "/name", "owner/"])
def test_github_repo_must_be_owner_slash_name(repo: str) -> None:
    cfg = CircuitryConfig(
        runtime={"library": {"sources": [{"type": "github", "repo": repo}]}}
    )
    with pytest.raises(LibrarySourceError, match="must be 'owner/name'"):
        LibraryRegistry.from_config(cfg)


def test_github_source_defaults(tmp_path: Path) -> None:
    cfg = CircuitryConfig(
        runtime={"library": {"sources": [{"type": "github", "repo": REPO}]}}
    )
    source = LibraryRegistry.from_config(cfg).get_source("name")
    assert isinstance(source, GitHubSource)
    assert source.ref == "main"
    assert source.path == ""
    assert source.token_env is None
    assert source.cache_root == default_cache_root()


def test_cache_root_honours_env_overrides(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("CIRCUITRY_CACHE_DIR", raising=False)
    monkeypatch.setenv("XDG_CACHE_HOME", "/tmp/xdg")
    assert default_cache_root() == Path("/tmp/xdg/circuitry/library")
    monkeypatch.setenv("CIRCUITRY_CACHE_DIR", "/tmp/explicit")
    assert default_cache_root() == Path("/tmp/explicit")


def test_source_name_cannot_escape_the_cache_root(tmp_path: Path) -> None:
    source = _source(tmp_path, name="../evil")
    assert source.cache_dir.parent == tmp_path / "cache"
    assert ".." not in source.cache_dir.name


# ── registry integration ─────────────────────────────────────────────────────


def test_registry_refresh_reports_every_source(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeGitHub().install(monkeypatch)
    cfg = CircuitryConfig(
        runtime={
            "library": {
                "sources": [
                    {"type": "curation"},
                    {
                        "type": "github",
                        "name": "hub",
                        "repo": REPO,
                        "path": "library/",
                        "cache_dir": str(tmp_path / "cache"),
                    },
                ]
            }
        }
    )
    registry = LibraryRegistry.from_config(cfg)
    results = {r.source: r for r in registry.refresh()}

    assert results["curation"].status == "skipped"
    assert results["hub"].status == "updated"
    assert results["hub"].sha == SHA_ONE
    assert "hub: updated (1111111)" in results["hub"].summary()


def test_registry_refresh_can_target_one_source(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeGitHub().install(monkeypatch)
    cfg = CircuitryConfig(
        runtime={
            "library": {
                "sources": [
                    {"type": "curation"},
                    {
                        "type": "github",
                        "name": "hub",
                        "repo": REPO,
                        "path": "library/",
                        "cache_dir": str(tmp_path / "cache"),
                    },
                ]
            }
        }
    )
    results = LibraryRegistry.from_config(cfg).refresh(source="hub")
    assert [r.source for r in results] == ["hub"]


def test_registry_notices_flag_the_unfetched_source(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _fail_urlopen(monkeypatch)
    cfg = CircuitryConfig(
        runtime={
            "library": {
                "sources": [
                    {"type": "curation"},
                    {
                        "type": "github",
                        "name": "hub",
                        "repo": REPO,
                        "cache_dir": str(tmp_path / "cache"),
                    },
                ]
            }
        }
    )
    registry = LibraryRegistry.from_config(cfg)
    notices = registry.notices()
    assert len(notices) == 1
    assert "hub" in notices[0]
    # Curation still resolves with the network down.
    assert registry.resolve("learn/hello") is not None


# ── CLI ──────────────────────────────────────────────────────────────────────


def test_cli_library_refresh_all_sources(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeGitHub().install(monkeypatch)
    cfg = _write_config(tmp_path / "config.json", tmp_path)

    result = runner.invoke(app, ["library", "refresh", "-c", str(cfg)])
    assert result.exit_code == 0
    assert "updated" in result.output
    assert (tmp_path / "cache" / "hub" / SHA_ONE / "my_pipeline.yml").exists()


def test_cli_library_refresh_named_source_is_idempotent(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeGitHub().install(monkeypatch)
    cfg = _write_config(tmp_path / "config.json", tmp_path)

    assert runner.invoke(app, ["library", "refresh", "hub", "-c", str(cfg)]).exit_code == 0
    second = runner.invoke(app, ["library", "refresh", "hub", "-c", str(cfg)])
    assert second.exit_code == 0
    assert "unchanged" in second.output


def test_cli_library_refresh_json(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeGitHub().install(monkeypatch)
    cfg = _write_config(tmp_path / "config.json", tmp_path)

    result = runner.invoke(app, ["library", "refresh", "--json", "-c", str(cfg)])
    assert result.exit_code == 0
    payload = json.loads(result.output)
    assert payload == [
        {
            "source": "hub",
            "status": "updated",
            "sha": SHA_ONE,
            "detail": f"3 file(s) from {REPO}@main",
        }
    ]


def test_cli_library_refresh_unknown_source(tmp_path: Path) -> None:
    cfg = _write_config(tmp_path / "config.json", tmp_path)
    result = runner.invoke(app, ["library", "refresh", "nope", "-c", str(cfg)])
    assert result.exit_code == 1
    assert "Unknown library source" in result.output


def test_cli_library_refresh_surfaces_errors(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = FakeGitHub().install(monkeypatch)
    fake.error = FakeGitHub._http_error("https://api.github.test", 404)
    cfg = _write_config(tmp_path / "config.json", tmp_path)

    result = runner.invoke(app, ["library", "refresh", "-c", str(cfg)])
    assert result.exit_code == 1
    assert "404" in result.output.replace("\n", " ")


def test_cli_list_warns_before_refresh_then_lists_after(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _fail_urlopen(monkeypatch)
    cfg = _write_config(tmp_path / "config.json", tmp_path)

    before = runner.invoke(app, ["list", "-c", str(cfg)])
    assert "not been fetched yet" in before.output.replace("\n", " ")

    FakeGitHub().install(monkeypatch)
    assert runner.invoke(app, ["library", "refresh", "-c", str(cfg)]).exit_code == 0

    _fail_urlopen(monkeypatch)
    after = runner.invoke(app, ["list", "--json", "-c", str(cfg)])
    assert after.exit_code == 0
    assert sorted(e["name"] for e in json.loads(after.output)) == [
        "my_pipeline",
        "nested/deep",
    ]


def test_cli_run_from_cache_with_the_network_down(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeGitHub().install(monkeypatch)
    cfg = _write_config(tmp_path / "config.json", tmp_path)
    assert runner.invoke(app, ["library", "refresh", "-c", str(cfg)]).exit_code == 0

    _fail_urlopen(monkeypatch)
    result = runner.invoke(
        app,
        ["run", "hub:my_pipeline", "--dry-run", "-e", "document=hi", "-c", str(cfg)],
    )
    assert result.exit_code == 0


def test_cli_info_on_unfetched_source_points_at_refresh(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _fail_urlopen(monkeypatch)
    cfg = _write_config(tmp_path / "config.json", tmp_path)
    result = runner.invoke(app, ["info", "hub:my_pipeline", "-c", str(cfg)])
    assert result.exit_code == 1
    assert "cof library refresh hub" in result.output.replace("\n", " ")
