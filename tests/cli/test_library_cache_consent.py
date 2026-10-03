"""Capability consent (#275, #337) for a document named by the raw path of a
library source's own cache directory, rather than by the library name.

A `github`-type source keeps its fetched subtree in a per-source cache
directory. Naming that exact file by path on the CLI, through MCP, or from
the TUI must not let a caller dodge the whole-document gate `cof run
hub/entry` applies just by pointing at the file instead of the name — the
file is still fetched content, not something the host wrote themselves
(#284 only trusts a path the host actually owns).
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import yaml
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.cli.config import trust_store_path
from circuitry.cli.document_consent import consented_capabilities, document_digest

runner = CliRunner()

_SHELL_EFFECT = {
    "type": "tool",
    "name": "t",
    "provider": "shell",
    "params": {"command": "echo", "args": ["hi"]},
}


def _write_yaml(path: Path, orch: dict[str, Any]) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(yaml.safe_dump(orch), encoding="utf-8")
    return path


def _github_config(tmp_path: Path, cache_dir: Path) -> Path:
    path = tmp_path / "config.json"
    path.write_text(
        json.dumps(
            {
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
        ),
        encoding="utf-8",
    )
    return path


def test_cof_run_refuses_a_shell_document_named_by_its_cache_path(tmp_path: Path) -> None:
    cache_dir = tmp_path / "cache"
    doc = _write_yaml(
        cache_dir / "hub" / "sha1" / "pipeline.yml", {"effects": [_SHELL_EFFECT]}
    )
    config = _github_config(tmp_path, cache_dir)

    result = runner.invoke(app, ["run", str(doc), "-c", str(config), "--json"])

    assert result.exit_code == 1
    payload = json.loads(result.stdout)
    assert payload["ok"] is False
    assert "shell" in payload["error"]
    assert "cof trust" in payload["error"]


def test_cof_run_allows_it_after_cof_trust(tmp_path: Path) -> None:
    cache_dir = tmp_path / "cache"
    doc = _write_yaml(
        cache_dir / "hub" / "sha1" / "pipeline.yml", {"effects": [_SHELL_EFFECT]}
    )
    config = _github_config(tmp_path, cache_dir)

    trusted = runner.invoke(app, ["trust", str(doc), "-c", str(config), "--yes"])
    assert trusted.exit_code == 0, trusted.output

    result = runner.invoke(app, ["run", str(doc), "-c", str(config), "--json"])

    assert result.exit_code == 0, result.output
    assert "runtime" in json.loads(result.stdout)


def test_cof_run_by_symlink_into_the_cache_dir_is_still_gated(tmp_path: Path) -> None:
    cache_dir = tmp_path / "cache"
    doc = _write_yaml(
        cache_dir / "hub" / "sha1" / "pipeline.yml", {"effects": [_SHELL_EFFECT]}
    )
    config = _github_config(tmp_path, cache_dir)
    link = tmp_path / "link.yml"
    link.symlink_to(doc)

    result = runner.invoke(app, ["run", str(link), "-c", str(config), "--json"])

    assert result.exit_code == 1
    payload = json.loads(result.stdout)
    assert "shell" in payload["error"]


def test_cof_fetch_copy_outside_the_cache_dir_stays_an_ordinary_trusted_path_run(
    tmp_path: Path,
) -> None:
    """#284's rule is unaffected: a copy saved outside the cache (what `cof
    fetch -o file.yml` produces) is this host's own file, same as always."""
    cache_dir = tmp_path / "cache"
    config = _github_config(tmp_path, cache_dir)
    copy = _write_yaml(tmp_path / "fetched-copy.yml", {"effects": [_SHELL_EFFECT]})

    result = runner.invoke(app, ["run", str(copy), "-c", str(config), "--json"])

    assert result.exit_code == 0, result.output
    assert "runtime" in json.loads(result.stdout)


def test_trust_by_cache_path_does_not_persist_for_a_copy_outside_it(tmp_path: Path) -> None:
    """Consent is recorded by content digest, not by path \u2014 trusting the
    cached file does not somehow also cover an unrelated document with
    different bytes."""
    cache_dir = tmp_path / "cache"
    doc = _write_yaml(
        cache_dir / "hub" / "sha1" / "pipeline.yml", {"effects": [_SHELL_EFFECT]}
    )
    config = _github_config(tmp_path, cache_dir)
    runner.invoke(app, ["trust", str(doc), "-c", str(config), "--yes"])

    other = _write_yaml(
        tmp_path / "other.yml",
        {"effects": [{**_SHELL_EFFECT, "params": {"command": "ls", "args": []}}]},
    )
    assert consented_capabilities(
        document_digest(other.read_bytes()), store_path=trust_store_path()
    ) is None
