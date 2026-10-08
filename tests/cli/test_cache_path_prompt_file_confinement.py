"""A `{file: ...}` prompt source in a document reached by its cache path, or
by a remote library source's bare name, stays confined to that source's own
fetched tree (#396 second-review finding 6).

`default_project_root`'s ordinary "nearest `circuitry.config.json`/
`config.json`, walking up from the document's own directory" rule, applied
unconditionally to a document living inside `~/.cache/circuitry/...`, would
walk straight out of the cache and pick up whatever project config an
ancestor of the cache directory happens to carry (`$HOME`, a custom
`XDG_CACHE_HOME`) — widening confinement past the one commit this run
actually trusts. `runtime_shim.run()`/`validate()` and `cli.trust.
_trust_document` must all use the fetched tree's own root instead, the same
one `core.library_ref`/`core.use.UseRuntime._confinement_root_for` already
give a `ref:` child pinned to the same source.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import yaml

from circuitry import api
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.library_sources import LibraryRegistry
from circuitry.cli.trust import _trust_document


def _write_yaml(path: Path, orch: dict[str, Any]) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(yaml.safe_dump(orch, sort_keys=False), encoding="utf-8")
    return path


def _write(path: Path, text: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    return path


def _fetched_tree(tmp_path: Path, *, sha: str = "sha1") -> tuple[Path, Path]:
    """A `github` source's cache, with a real `index.json` (so
    `entries_dir`/`cached_tree_root` resolve, unlike the `cache_dir`-only
    fixtures `test_cache_path_trust.py` uses for the simpler `is_cache_path`
    checks) — returns ``(cache_root, entries_dir)``."""
    cache_root = tmp_path / "cache"
    entries_dir = cache_root / "hub" / sha
    entries_dir.mkdir(parents=True, exist_ok=True)
    (cache_root / "hub" / "index.json").write_text(
        json.dumps({"source": "hub", "sha": sha}), encoding="utf-8"
    )
    return cache_root, entries_dir


def _github_config(cache_root: Path) -> CircuitryConfig:
    return CircuitryConfig(
        runtime={
            "library": {
                "sources": [
                    {
                        "type": "github",
                        "name": "hub",
                        "repo": "owner/name",
                        "cache_dir": str(cache_root),
                    }
                ]
            }
        }
    )


def test_an_ancestor_project_config_above_the_cache_does_not_widen_confinement(
    tmp_path: Path,
) -> None:
    """A `circuitry.config.json` sitting above the whole cache directory
    (as `$HOME`/a custom `XDG_CACHE_HOME` could carry one) must not become
    the confinement root for a document fetched into that cache -- a
    `{file: ...}` reference that escapes the fetched tree, even though it
    would resolve inside that wider ancestor-config project, is still a
    compile error."""
    (tmp_path / "circuitry.config.json").write_text("{}", encoding="utf-8")
    cache_root, entries_dir = _fetched_tree(tmp_path)
    _write(tmp_path / "outside.md", "leaked")
    doc = _write_yaml(
        entries_dir / "pipeline.yml",
        {
            "prompts": {"x": {"file": "../../../outside.md"}},
            "effects": [{"type": "yield", "name": "y", "template": "{{> x}}"}],
        },
    )
    cfg = _github_config(cache_root)

    result = api.run_orchestration(
        orchestration_path=doc, config=cfg, trust_document=True, raise_on_error=False
    )

    assert not result.ok
    assert "resolves outside the project" in (result.error or "")
    # The confinement error must name the fetched tree itself, not the
    # ancestor config's wider project -- otherwise this could as easily be
    # pinning a coincidence as the actual fix.
    assert str(entries_dir) in (result.error or "")


def test_dotdot_inside_the_cached_tree_still_works(tmp_path: Path) -> None:
    """The ordinary `../x.md`-inside-the-project rule still holds within
    the fetched tree itself: a document nested one level deeper than its
    source file can reach it."""
    cache_root, entries_dir = _fetched_tree(tmp_path)
    _write(entries_dir / "voice.md", "Plain, direct.")
    doc = _write_yaml(
        entries_dir / "sub" / "pipeline.yml",
        {
            "prompts": {"voice": {"file": "../voice.md"}},
            "effects": [{"type": "yield", "name": "y", "template": "{{> voice}}"}],
        },
    )
    cfg = _github_config(cache_root)

    result = api.run_orchestration(orchestration_path=doc, config=cfg, trust_document=True)

    assert result.ok, result.error
    assert result.state["prime"]["y"]["value"] == "Plain, direct."


def test_validate_also_confines_a_cache_path_document_to_its_own_tree(
    tmp_path: Path,
) -> None:
    """`cof check` (`runtime_shim.validate()`) applies the same confinement
    as an actual run -- a document that would be refused at run time is
    already refused here."""
    from circuitry.cli.runtime_shim import validate

    (tmp_path / "circuitry.config.json").write_text("{}", encoding="utf-8")
    cache_root, entries_dir = _fetched_tree(tmp_path)
    _write(tmp_path / "outside.md", "leaked")
    doc = _write_yaml(
        entries_dir / "pipeline.yml",
        {
            "prompts": {"x": {"file": "../../../outside.md"}},
            "effects": [{"type": "yield", "name": "y", "template": "{{> x}}"}],
        },
    )
    cfg = _github_config(cache_root)

    result = validate(doc, config=cfg, trust_document=True)

    assert result["ok"] is False
    assert any("resolves outside the project" in e for e in result["errors"])


def test_cof_trust_hashes_against_the_cached_tree_not_a_wider_ancestor_config(
    tmp_path: Path,
) -> None:
    """`cof trust <cache path>` must compute its digest the same way a real
    run would -- confined to the fetched tree, not whatever project config
    sits above the whole cache directory."""
    (tmp_path / "circuitry.config.json").write_text("{}", encoding="utf-8")
    cache_root, entries_dir = _fetched_tree(tmp_path)
    _write(entries_dir / "voice.md", "Plain, direct.")
    doc = _write_yaml(
        entries_dir / "pipeline.yml",
        {
            "prompts": {"voice": {"file": "voice.md"}},
            "effects": [
                {
                    "type": "tool",
                    "name": "t",
                    "provider": "shell",
                    "params": {"command": "echo", "args": ["{{> voice}}"]},
                }
            ],
        },
    )
    config_path = tmp_path / "config.json"
    config_path.write_text(
        json.dumps(
            {
                "runtime": {
                    "library": {
                        "sources": [
                            {
                                "type": "github",
                                "name": "hub",
                                "repo": "owner/name",
                                "cache_dir": str(cache_root),
                            }
                        ]
                    }
                }
            }
        ),
        encoding="utf-8",
    )
    store = tmp_path / "trusted.json"

    # Must not raise -- it reads the file confined to the fetched tree,
    # the same project boundary a real run would apply.
    _trust_document(doc, yes=True, store=store, config=config_path)

    assert store.exists()


def test_cached_tree_root_finds_the_specific_commit_not_the_whole_cache() -> None:
    """Unit-level pin for `LibraryRegistry.cached_tree_root`: it names the
    one fetched subtree a path lives in, not every source/commit the cache
    directory as a whole has ever held."""
    import tempfile

    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        cache_root, entries_dir = _fetched_tree(tmp_path)
        doc = _write(entries_dir / "nested" / "pipeline.yml", "x: 1")
        registry = LibraryRegistry.from_runtime(_github_config(cache_root).runtime)

        root = registry.cached_tree_root(doc)

        assert root == entries_dir.resolve()
