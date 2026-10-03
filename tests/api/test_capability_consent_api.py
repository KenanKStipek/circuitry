"""Capability consent (#275) for the SDK: `run_orchestration`'s `use: ref:`
children, and `run_shared_orchestration`'s own fetched asset, go through the
same gate `cof run`/`cof run-library` apply (#334) — refused unless the
document's digest is already consented (`cof trust`), or pre-approved for
this one call with `allow_capabilities=`, since the embedding program is the
host.
"""

from __future__ import annotations

from pathlib import Path

import yaml

from circuitry import run_orchestration, run_shared_orchestration
from circuitry.cli.config import CircuitryConfig, trust_store_path
from circuitry.cli.document_consent import document_digest, record_consent

# A document whose only `shell` use is in `finally:` (#332 review: the
# static walk must cover `finally:`, not just `effects:`).
_SHELL_IN_FINALLY = {
    "effects": [],
    "finally": [
        {"type": "tool", "name": "cleanup", "provider": "shell", "params": {"command": "echo"}}
    ],
}


def _write_yaml(path: Path, orch: dict) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(yaml.safe_dump(orch), encoding="utf-8")
    return path


# -- run_orchestration: a `use: ref:` child, independent of the root's own trust


def test_run_orchestration_refuses_a_ref_child_whose_shell_use_is_unconsented(
    tmp_path: Path,
) -> None:
    lib_dir = tmp_path / "lib"
    _write_yaml(lib_dir / "helper.yml", _SHELL_IN_FINALLY)
    root_path = _write_yaml(
        tmp_path / "root.yml", {"effects": [{"type": "use", "name": "u", "ref": "helper"}]}
    )
    cfg = CircuitryConfig(
        runtime={"library": {"sources": [{"type": "folder", "name": "local", "path": str(lib_dir)}]}}
    )

    result = run_orchestration(
        orchestration_path=root_path, config=cfg, dry_run=False, raise_on_error=False
    )

    assert result.ok is False
    assert "shell" in (result.error or "")
    assert "cof trust" in (result.error or "")


def test_run_orchestration_runs_the_ref_child_once_it_is_trusted(tmp_path: Path) -> None:
    lib_dir = tmp_path / "lib"
    helper_path = _write_yaml(lib_dir / "helper.yml", _SHELL_IN_FINALLY)
    root_path = _write_yaml(
        tmp_path / "root.yml", {"effects": [{"type": "use", "name": "u", "ref": "helper"}]}
    )
    cfg = CircuitryConfig(
        default_adapter="ollama",
        default_model="llama3.1:8b",
        runtime={"library": {"sources": [{"type": "folder", "name": "local", "path": str(lib_dir)}]}},
    )
    digest = document_digest(helper_path.read_bytes())
    record_consent(digest, frozenset({"shell"}), store_path=trust_store_path())

    result = run_orchestration(orchestration_path=root_path, config=cfg, dry_run=False)

    assert result.ok is True, result.error


def test_run_orchestration_allow_capabilities_unblocks_the_ref_child(tmp_path: Path) -> None:
    """The embedding host's own consent for this one call — never persisted
    to `cof trust`'s store."""
    lib_dir = tmp_path / "lib"
    helper_path = _write_yaml(lib_dir / "helper.yml", _SHELL_IN_FINALLY)
    root_path = _write_yaml(
        tmp_path / "root.yml", {"effects": [{"type": "use", "name": "u", "ref": "helper"}]}
    )
    cfg = CircuitryConfig(
        default_adapter="ollama",
        default_model="llama3.1:8b",
        runtime={"library": {"sources": [{"type": "folder", "name": "local", "path": str(lib_dir)}]}},
    )

    result = run_orchestration(
        orchestration_path=root_path,
        config=cfg,
        dry_run=False,
        allow_capabilities=["shell"],
    )

    assert result.ok is True, result.error
    digest = document_digest(helper_path.read_bytes())
    # Never persisted (#275): a plain re-run with no allow_capabilities and
    # no prior `cof trust` refuses again.
    from circuitry.cli.document_consent import consented_capabilities

    assert consented_capabilities(digest, store_path=trust_store_path()) is None


def test_run_orchestration_allow_capabilities_rejects_a_bare_string(tmp_path: Path) -> None:
    """`frozenset("shell")` would silently become `{'s', 'h', 'e', 'l'}` and
    then refuse with a confusing error; reject the mistake outright instead.
    """
    root_path = _write_yaml(tmp_path / "root.yml", {"effects": []})

    try:
        run_orchestration(
            orchestration_path=root_path,
            allow_capabilities="shell",
        )
    except TypeError as exc:
        assert "shell" in str(exc)
    else:
        raise AssertionError("expected TypeError for a bare string")


# -- run_shared_orchestration: the fetched asset's own whole-document gate


def _library_config(tmp_path: Path, asset_yaml: dict) -> tuple[CircuitryConfig, Path]:
    lib_root = tmp_path / "library"
    asset_path = lib_root / "welcome" / "1.0.0.yml"
    _write_yaml(asset_path, asset_yaml)
    cfg = CircuitryConfig(
        runtime={"library": {"backend": "filesystem", "local_root": str(lib_root)}}
    )
    return cfg, asset_path


def test_run_shared_orchestration_refuses_an_unconsented_shell_asset(tmp_path: Path) -> None:
    cfg, _asset_path = _library_config(tmp_path, _SHELL_IN_FINALLY)

    result = run_shared_orchestration(
        asset_id="welcome", version="1.0.0", config=cfg, dry_run=False, raise_on_error=False
    )

    assert result.ok is False
    assert "shell" in (result.error or "")
    assert "cof trust" in (result.error or "")


def test_run_shared_orchestration_runs_once_the_asset_is_trusted(tmp_path: Path) -> None:
    cfg, asset_path = _library_config(tmp_path, _SHELL_IN_FINALLY)
    digest = document_digest(asset_path.read_bytes())
    record_consent(digest, frozenset({"shell"}), store_path=trust_store_path())

    result = run_shared_orchestration(asset_id="welcome", version="1.0.0", config=cfg, dry_run=False)

    assert result.ok is True, result.error


def test_run_shared_orchestration_allow_capabilities_unblocks_the_asset(tmp_path: Path) -> None:
    cfg, _asset_path = _library_config(tmp_path, _SHELL_IN_FINALLY)

    result = run_shared_orchestration(
        asset_id="welcome",
        version="1.0.0",
        config=cfg,
        dry_run=False,
        allow_capabilities=["shell"],
    )

    assert result.ok is True, result.error
