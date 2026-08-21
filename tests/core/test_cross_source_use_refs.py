"""Cross-source `use ref:` resolution — chains, pins, preflight, offline re-runs.

The chain under test spans all three source types: a temp-file root uses a
`folder` entry, which uses a `github` entry served from a SHA-pinned cache,
which uses a bundled `curation` entry. GitHub is faked at
`urllib.request.urlopen` (same approach as `tests/cli/test_github_source.py`),
so the cache is populated by a real `refresh()` and the offline re-run proves
itself by making every network call raise.
"""

from __future__ import annotations

import base64
import json
import urllib.parse
from pathlib import Path
from typing import Any, Optional
from unittest.mock import MagicMock
from urllib.error import URLError

import pytest
import yaml

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.library_sources import LibraryRegistry
from circuitry.cli.runtime_shim import RunRequest, run, validate
from circuitry.core.library_ref import LibraryRefError, resolve_ref

REPO = "owner/name"
SHA = "abc1234567890abc1234567890abc1234567890a"

CRITIQUE_JSON = json.dumps({"score": 8, "issues": ["Tighten it"], "strengths": ["Clear"]})


# ── fake GitHub ──────────────────────────────────────────────────────────────


class _FakeResponse:
    def __init__(self, body: bytes) -> None:
        self._body = body

    def read(self) -> bytes:
        return self._body

    def __enter__(self) -> "_FakeResponse":
        return self

    def __exit__(self, *args: Any) -> None:
        return None


class FakeGitHub:
    """Just enough of the contents API to serve one subtree at one SHA."""

    def __init__(self, tree: dict[str, str], *, sha: str = SHA) -> None:
        self.tree = dict(tree)
        self.sha = sha
        self.offline = False
        self.calls = 0

    def install(self, monkeypatch: pytest.MonkeyPatch) -> "FakeGitHub":
        def fake_urlopen(req: Any, timeout: int = 0) -> _FakeResponse:
            self.calls += 1
            if self.offline:
                raise URLError("Network is unreachable")
            return _FakeResponse(json.dumps(self._route(req.full_url)).encode("utf-8"))

        monkeypatch.setattr("urllib.request.urlopen", fake_urlopen)
        return self

    def _route(self, url: str) -> Any:
        path = urllib.parse.unquote(urllib.parse.urlsplit(url).path)
        rest = path[len(f"/repos/{REPO}/") :]
        if rest.startswith("commits/"):
            return {"sha": self.sha}
        target = rest[len("contents") :].lstrip("/")
        if target in self.tree:
            return self._file(target, with_content=True)
        prefix = f"{target}/" if target else ""
        children: dict[str, Any] = {}
        for repo_path in sorted(self.tree):
            if not repo_path.startswith(prefix):
                continue
            head, slash, _ = repo_path[len(prefix) :].partition("/")
            if slash:
                children.setdefault(
                    head, {"name": head, "path": f"{prefix}{head}", "type": "dir"}
                )
            else:
                children[head] = self._file(repo_path, with_content=False)
        return list(children.values())

    def _file(self, repo_path: str, *, with_content: bool) -> dict[str, Any]:
        obj: dict[str, Any] = {
            "name": repo_path.rsplit("/", 1)[-1],
            "path": repo_path,
            "type": "file",
        }
        if with_content:
            obj["encoding"] = "base64"
            obj["content"] = base64.b64encode(
                self.tree[repo_path].encode("utf-8")
            ).decode("ascii")
        return obj


# ── fixtures ─────────────────────────────────────────────────────────────────


def _write(path: Path, content: Any) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        content if isinstance(content, str) else yaml.dump(content), encoding="utf-8"
    )
    return path


def _config(tmp_path: Path, *, sources: Optional[list[dict[str, Any]]] = None) -> CircuitryConfig:
    return CircuitryConfig(
        default_model="test-model",
        default_adapter="ollama",
        runtime={
            "library": {
                "sources": sources
                if sources is not None
                else [
                    {"type": "curation"},
                    {"type": "folder", "name": "local", "path": str(tmp_path / "local")},
                    {
                        "type": "github",
                        "name": "hub",
                        "repo": REPO,
                        "path": "library/",
                        "cache_dir": str(tmp_path / "cache"),
                    },
                ]
            }
        },
    )


#: The hub-side orchestration, which itself reaches into the curation library.
HUB_PIPELINE = """\
# Hub pipeline — critiques a document via the curation library.

interface:
  inputs:
    document:
      type: string
      required: true

effects:
  - type: use
    name: critique
    ref: utilities/critique
    inputs:
      content: "{{document}}"
      criteria: "clarity"
    outputs:
      critique: prime.critique.value
"""

HUB_TREE = {"library/pipeline.yml": HUB_PIPELINE}


def _mock_adapter() -> MagicMock:
    adapter = MagicMock()
    adapter.name = "mock"
    result = MagicMock()
    result.text = CRITIQUE_JSON
    result.raw = {}
    result.tokens_sent = 0
    result.tokens_received = 0
    adapter.generate.return_value = result
    return adapter


def _chain_root(tmp_path: Path) -> Path:
    """root (temp file) → local:relay (folder) → hub:pipeline (github) → curation."""
    _write(
        tmp_path / "local" / "relay.yml",
        {
            "effects": [
                {
                    "type": "use",
                    "name": "hub_step",
                    "ref": "hub:pipeline",
                    "inputs": {"document": "{{document}}"},
                    "outputs": {"critique": "prime.critique.value.critique"},
                }
            ]
        },
    )
    return _write(
        tmp_path / "root.yml",
        {
            "effects": [
                {
                    "type": "use",
                    "name": "relay_step",
                    "ref": "local:relay",
                    "inputs": {"document": "A draft paragraph."},
                    "outputs": {"critique": "prime.hub_step.value.critique"},
                }
            ]
        },
    )


def _refresh_hub(cfg: CircuitryConfig) -> None:
    results = LibraryRegistry.from_config(cfg).refresh(source="hub")
    assert [r.status for r in results] == ["updated"], [r.summary() for r in results]


def _run(path: Path, cfg: CircuitryConfig) -> Any:
    return run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=cfg,
            adapter=_mock_adapter(),
        )
    )


# ── AC 1: the chain executes and pins land in state ──────────────────────────


def test_chain_across_curation_folder_and_github_records_pins(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeGitHub(HUB_TREE).install(monkeypatch)
    cfg = _config(tmp_path)
    _refresh_hub(cfg)
    root = _chain_root(tmp_path)

    result = _run(root, cfg)

    assert result.ok, result.error
    assert result.state["prime"]["relay_step"]["value"]["critique"]["score"] == 8

    pins = {p["source"]: p for p in result.state["runtime"]["library_refs"]}
    assert set(pins) == {"local", "hub", "curation"}
    assert pins["hub"]["ref"] == "hub:pipeline"
    assert pins["hub"]["sha"] == SHA
    assert pins["hub"]["cache_path"] == str(tmp_path / "cache" / "hub" / SHA)
    assert pins["hub"]["path"].startswith(pins["hub"]["cache_path"])
    # Local sources have no commit to pin — the resolved path is the record.
    assert pins["local"]["sha"] is None
    assert pins["curation"]["ref"] == "utilities/critique"
    assert pins["curation"]["path"].endswith("curation/utilities/critique.yml")


def test_pin_is_mirrored_onto_the_use_effect_meta(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeGitHub(HUB_TREE).install(monkeypatch)
    cfg = _config(tmp_path)
    _refresh_hub(cfg)

    result = _run(_chain_root(tmp_path), cfg)

    assert result.ok, result.error
    meta = result.state["prime"]["relay_step"]["meta"]["library_ref"]
    assert meta["source"] == "local"
    assert meta["ref"] == "local:relay"


# ── AC 4: offline re-run from cache ──────────────────────────────────────────


def test_second_run_succeeds_with_the_network_unplugged(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    github = FakeGitHub(HUB_TREE).install(monkeypatch)
    cfg = _config(tmp_path)
    _refresh_hub(cfg)
    root = _chain_root(tmp_path)

    first = _run(root, cfg)
    assert first.ok, first.error

    # Every network call now fails; only the cache remains.
    github.offline = True
    calls_before = github.calls
    second = _run(root, cfg)

    assert second.ok, second.error
    assert github.calls == calls_before, "an offline re-run must not touch the network"
    first_pin = next(p for p in first.state["runtime"]["library_refs"] if p["source"] == "hub")
    second_pin = next(p for p in second.state["runtime"]["library_refs"] if p["source"] == "hub")
    assert second_pin["sha"] == first_pin["sha"] == SHA
    assert second_pin["cache_path"] == first_pin["cache_path"]


# ── AC 2: an unfetched source fails before anything runs ─────────────────────


def test_unfetched_source_ref_fails_validation_with_refresh_command(
    tmp_path: Path,
) -> None:
    cfg = _config(tmp_path)  # never refreshed — the hub cache does not exist
    root = _write(
        tmp_path / "root.yml",
        {"effects": [{"type": "use", "name": "hub_step", "ref": "hub:pipeline"}]},
    )

    result = validate(root, config=cfg)

    assert result["ok"] is False
    joined = " ".join(result["errors"])
    assert "library_ref:hub:pipeline" in joined
    assert "`cof library refresh hub`" in joined


def test_unfetched_source_ref_fails_preflight_not_mid_run(tmp_path: Path) -> None:
    cfg = _config(tmp_path)
    root = _write(
        tmp_path / "root.yml",
        {"effects": [{"type": "use", "name": "hub_step", "ref": "hub:pipeline"}]},
    )

    result = run(
        RunRequest(
            orchestration_path=root,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=cfg,
        )
    )

    assert result.ok is False
    assert "Preflight failed" in (result.error or "")
    assert "`cof library refresh hub`" in (result.error or "")
    # Preflight ran before the effect did: no `use` node was ever written.
    assert "prime" not in result.state


def test_transitively_referenced_unfetched_source_is_caught(tmp_path: Path) -> None:
    """A ref two orchestrations deep still fails at validate time."""
    _write(
        tmp_path / "local" / "relay.yml",
        {"effects": [{"type": "use", "name": "hub_step", "ref": "hub:pipeline"}]},
    )
    cfg = _config(tmp_path)
    root = _write(
        tmp_path / "root.yml",
        {"effects": [{"type": "use", "name": "relay_step", "ref": "local:relay"}]},
    )

    result = validate(root, config=cfg)

    assert result["ok"] is False
    assert "`cof library refresh hub`" in " ".join(result["errors"])


# ── AC 3: cycle detection across sources ─────────────────────────────────────


def test_cycle_through_a_github_cache_is_detected(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A (folder) → B (github cache) → A (folder) is caught statically."""
    cycle_tree = {
        "library/b.yml": yaml.dump(
            {"effects": [{"type": "use", "name": "back_to_a", "ref": "local:a"}]}
        )
    }
    FakeGitHub(cycle_tree).install(monkeypatch)
    cfg = _config(tmp_path)
    a = _write(
        tmp_path / "local" / "a.yml",
        {"effects": [{"type": "use", "name": "to_b", "ref": "hub:b"}]},
    )
    _refresh_hub(cfg)

    result = validate(a, config=cfg)

    assert result["ok"] is False
    cycle_errors = [e for e in result["errors"] if e.startswith("Cycle:")]
    assert cycle_errors, result["errors"]
    assert "a.yml" in cycle_errors[0]
    assert f"{SHA}/b.yml" in cycle_errors[0]


def test_cycle_detection_ignores_unresolvable_refs(tmp_path: Path) -> None:
    """An unfetched source is preflight's error, not a spurious cycle."""
    from circuitry.core.cycle_check import detect_cycles

    orch = {"effects": [{"type": "use", "name": "hub_step", "ref": "hub:pipeline"}]}
    root = _write(tmp_path / "root.yml", orch)

    assert detect_cycles(orch, root_path=root, runtime=_config(tmp_path).runtime) is None


# ── resolution semantics ─────────────────────────────────────────────────────


def test_source_qualified_ref_bypasses_precedence(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    shared = {"library/shared.yml": yaml.dump({"effects": []})}
    FakeGitHub(shared).install(monkeypatch)
    cfg = _config(tmp_path)
    _write(tmp_path / "local" / "shared.yml", {"effects": []})
    _refresh_hub(cfg)

    qualified = resolve_ref("hub:shared", runtime=cfg.runtime)
    bare = resolve_ref("shared", runtime=cfg.runtime)

    assert qualified is not None and qualified.source == "hub"
    assert qualified.sha == SHA
    # Bare names take the first source in precedence order and report the rest.
    assert bare is not None and bare.source == "local"
    assert bare.ambiguous_sources == ["hub"]
    assert bare.sha is None


def test_qualified_ref_to_unfetched_source_raises_with_the_command(
    tmp_path: Path,
) -> None:
    with pytest.raises(LibraryRefError) as exc:
        resolve_ref("hub:pipeline", runtime=_config(tmp_path).runtime)

    assert "`cof library refresh hub`" in str(exc.value)


def test_unknown_ref_resolves_to_none_without_raising(tmp_path: Path) -> None:
    cfg = _config(tmp_path, sources=[{"type": "curation"}])

    assert resolve_ref("nope/not-a-thing", runtime=cfg.runtime) is None


def test_curation_only_default_is_unchanged() -> None:
    """Zero-config behaviour: a bare curation ref still resolves, unpinned."""
    resolved = resolve_ref("utilities/critique")

    assert resolved is not None
    assert resolved.source == "curation"
    assert resolved.sha is None
    assert resolved.path.name == "critique.yml"
