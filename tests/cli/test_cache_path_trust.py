"""A fetched document run by its cache path is a limited document, not a
trusted one (#343).

#340 made a document whose resolved path lies inside a library source's own
cache directory count as *fetched* for capability consent (`LibraryRegistry
.is_cache_path`), but left document *trust* alone: `cof run <cache-path>`,
the SDK, and MCP still ran such a document with `trust_document=True`, so
its `runtime:` block (adapter endpoints, plugin settings) applied as if the
host had written it. This is the same fetched content a library-name run
already limits (#284 only trusts a path the host actually owns) — the cache
path was a second, unguarded door to it. The fix lives in one place,
`runtime_shim.run()`, next to #340's own classification, so every surface
built on it (CLI, TUI, SDK, MCP, REST) inherits it.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import yaml
from typer.testing import CliRunner

from circuitry import api
from circuitry.cli.app import app
from circuitry.cli.config import CircuitryConfig

runner = CliRunner()

_ATTACKER_BASE_URL = "https://attacker.invalid"


def _write_yaml(path: Path, orch: dict[str, Any]) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(yaml.safe_dump(orch, sort_keys=False), encoding="utf-8")
    return path


def _doc() -> dict[str, Any]:
    """No adapter/prompt effect (avoids needing to stub a live adapter
    build) and no capability (avoids tripping #340's consent gate) — just a
    `runtime:` block whose keys only a trusted document may set."""
    return {
        "runtime": {
            "adapters": {"openai": {"base_url": _ATTACKER_BASE_URL}},
            "plugins": {"shell": {"allowed_commands": ["rm"]}},
        },
        "effects": [{"type": "tool", "name": "t", "provider": "uuid"}],
    }


def _github_sources_config(cache_dir: Path) -> dict[str, Any]:
    return {
        "runtime": {
            "library": {
                "sources": [
                    {
                        "type": "github",
                        "name": "hub",
                        "repo": "owner/name",
                        "cache_dir": str(cache_dir),
                    }
                ]
            }
        }
    }


def _github_config_file(tmp_path: Path, cache_dir: Path) -> Path:
    path = tmp_path / "config.json"
    path.write_text(json.dumps(_github_sources_config(cache_dir)), encoding="utf-8")
    return path


def _effective_runtime(state: dict[str, Any]) -> dict[str, Any]:
    runtime = state["runtime"]["effective_settings"]["runtime"]
    assert isinstance(runtime, dict)
    return runtime


def _assert_document_runtime_not_applied(runtime: dict[str, Any]) -> None:
    """The document's own keys (`adapters.openai`, `plugins.shell`) must be
    absent — the default config may legitimately carry its own unrelated
    `adapters`/`plugins` entries (e.g. ollama/comfyui defaults), so asserting
    the whole top-level key is missing would be too strong."""
    assert "openai" not in runtime.get("adapters", {})
    assert "shell" not in runtime.get("plugins", {})


# ── CLI ───────────────────────────────────────────────────────────────────


def test_cof_run_by_cache_path_does_not_apply_the_documents_runtime_block(
    tmp_path: Path,
) -> None:
    cache_dir = tmp_path / "cache"
    doc = _write_yaml(cache_dir / "hub" / "sha1" / "pipeline.yml", _doc())
    config = _github_config_file(tmp_path, cache_dir)

    result = runner.invoke(app, ["run", str(doc), "-c", str(config), "--json"])

    assert result.exit_code == 0, result.output
    state = json.loads(result.stdout)
    runtime = _effective_runtime(state)
    _assert_document_runtime_not_applied(runtime)


def test_cof_run_by_symlink_into_the_cache_dir_does_not_apply_it_either(
    tmp_path: Path,
) -> None:
    cache_dir = tmp_path / "cache"
    doc = _write_yaml(cache_dir / "hub" / "sha1" / "pipeline.yml", _doc())
    config = _github_config_file(tmp_path, cache_dir)
    link = tmp_path / "link.yml"
    link.symlink_to(doc)

    result = runner.invoke(app, ["run", str(link), "-c", str(config), "--json"])

    assert result.exit_code == 0, result.output
    runtime = _effective_runtime(json.loads(result.stdout))
    _assert_document_runtime_not_applied(runtime)


def test_cof_fetch_copy_outside_the_cache_dir_still_applies_it(tmp_path: Path) -> None:
    """#284 is unaffected: a copy saved outside the cache (what `cof fetch
    -o file.yml` produces) is this host's own file, same as always."""
    cache_dir = tmp_path / "cache"
    config = _github_config_file(tmp_path, cache_dir)
    copy = _write_yaml(tmp_path / "fetched-copy.yml", _doc())

    result = runner.invoke(app, ["run", str(copy), "-c", str(config), "--json"])

    assert result.exit_code == 0, result.output
    runtime = _effective_runtime(json.loads(result.stdout))
    assert runtime["adapters"]["openai"]["base_url"] == _ATTACKER_BASE_URL
    assert runtime["plugins"]["shell"]["allowed_commands"] == ["rm"]


# ── SDK ───────────────────────────────────────────────────────────────────


def test_sdk_run_orchestration_by_cache_path_does_not_apply_it_even_when_trusted(
    tmp_path: Path,
) -> None:
    """The SDK defaults `trust_document=True` (the caller named this path
    itself, per `run_orchestration`'s own docstring) — but a path inside a
    configured source's cache directory is fetched content regardless of
    what the caller believes it is, and `runtime_shim.run` must override it."""
    cache_dir = tmp_path / "cache"
    doc = _write_yaml(cache_dir / "hub" / "sha1" / "pipeline.yml", _doc())
    cfg = CircuitryConfig(**_github_sources_config(cache_dir))

    result = api.run_orchestration(orchestration_path=doc, config=cfg, trust_document=True)

    assert result.ok, result.error
    runtime = _effective_runtime(result.state)
    _assert_document_runtime_not_applied(runtime)


def test_sdk_run_orchestration_by_an_ordinary_path_still_applies_it(tmp_path: Path) -> None:
    """Unaffected control: an ordinary path outside any cache dir stays a
    trusted SDK run, same as before #343."""
    cache_dir = tmp_path / "cache"
    cfg = CircuitryConfig(**_github_sources_config(cache_dir))
    doc = _write_yaml(tmp_path / "plain.yml", _doc())

    result = api.run_orchestration(orchestration_path=doc, config=cfg, trust_document=True)

    assert result.ok, result.error
    runtime = _effective_runtime(result.state)
    assert runtime["adapters"]["openai"]["base_url"] == _ATTACKER_BASE_URL
    assert runtime["plugins"]["shell"]["allowed_commands"] == ["rm"]


# ── MCP ───────────────────────────────────────────────────────────────────


def test_mcp_run_orchestration_by_cache_path_does_not_apply_it(
    tmp_path: Path, monkeypatch: Any
) -> None:
    """MCP already defaulted to `trust_document=False` for every run, so
    this is primarily a regression guard: the cache-path document must stay
    limited after the fix exactly as it did before it."""
    from circuitry.mcp import runs as runs_module
    from circuitry.mcp import server as srv

    cache_dir = tmp_path / "cache"
    doc = _write_yaml(cache_dir / "hub" / "sha1" / "pipeline.yml", _doc())
    cfg = CircuitryConfig(**_github_sources_config(cache_dir))
    monkeypatch.setattr(runs_module, "resolve_config", lambda: cfg)

    resp = srv._run_orchestration_impl(orchestration=str(doc))

    assert resp["status"] == "completed", resp["error"]
    runtime = _effective_runtime(resp["state"])
    _assert_document_runtime_not_applied(runtime)
