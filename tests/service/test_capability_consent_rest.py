"""Capability consent (#275) for the REST trigger: `orchestration_path` is
always a file under `orchestration_root`, never a name resolved against a
library source, so the top-level document stays a run by path (#284) on
every request. A `use: ref:` child it reaches is independently gated
regardless (#332) — this is REST's own version of the #334 fix: proving the
surface can't skip that gate, and that nothing in the request can grant
capabilities on the caller's behalf.
"""

from __future__ import annotations

import json
from pathlib import Path

import yaml

from circuitry.cli.config import CircuitryConfig, trust_store_path
from circuitry.cli.document_consent import document_digest, record_consent
from circuitry.service import RestTriggerService

# A `use: ref:` child whose only `shell` use is in `finally:` (#332 review:
# the static walk must cover `finally:`, not just `effects:`).
_HELPER_SHELL_IN_FINALLY = {
    "effects": [],
    "finally": [
        {"type": "tool", "name": "cleanup", "provider": "shell", "params": {"command": "echo"}}
    ],
}


def _write_yaml(path: Path, orch: dict) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(yaml.safe_dump(orch), encoding="utf-8")
    return path


def _service(tmp_path: Path, lib_dir: Path) -> RestTriggerService:
    cfg = CircuitryConfig(
        default_adapter="ollama",
        default_model="llama3.1:8b",
        runtime={"library": {"sources": [{"type": "folder", "name": "local", "path": str(lib_dir)}]}},
    )
    return RestTriggerService(allow_unauthenticated=True, orchestration_root=tmp_path, config=cfg)


def test_rest_refuses_a_use_ref_child_whose_shell_use_is_uncented(tmp_path: Path) -> None:
    lib_dir = tmp_path / "lib"
    _write_yaml(lib_dir / "helper.yml", _HELPER_SHELL_IN_FINALLY)
    root_path = _write_yaml(
        tmp_path / "root.yml", {"effects": [{"type": "use", "name": "u", "ref": "helper"}]}
    )

    svc = _service(tmp_path, lib_dir)
    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps({"orchestration_path": str(root_path), "dry_run": False}),
    )

    assert response.status_code == 500
    assert response.body["ok"] is False
    assert "shell" in response.body["error"]
    assert "cof trust" in response.body["error"]


def test_rest_ignores_a_caller_supplied_capability_grant_in_the_payload(tmp_path: Path) -> None:
    """The REST payload schema has no field that grants capabilities; an
    extra `allow_capabilities`-shaped key is simply ignored, not threaded
    into the consent gate."""
    lib_dir = tmp_path / "lib"
    _write_yaml(lib_dir / "helper.yml", _HELPER_SHELL_IN_FINALLY)
    root_path = _write_yaml(
        tmp_path / "root.yml", {"effects": [{"type": "use", "name": "u", "ref": "helper"}]}
    )

    svc = _service(tmp_path, lib_dir)
    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps(
            {
                "orchestration_path": str(root_path),
                "dry_run": False,
                "allow_capabilities": ["shell", "network"],
                "state": {"allow_capabilities": ["shell"]},
            }
        ),
    )

    assert response.status_code == 500
    assert "shell" in response.body["error"]


def test_rest_runs_the_ref_child_once_it_is_trusted(tmp_path: Path) -> None:
    lib_dir = tmp_path / "lib"
    helper_path = _write_yaml(lib_dir / "helper.yml", _HELPER_SHELL_IN_FINALLY)
    root_path = _write_yaml(
        tmp_path / "root.yml", {"effects": [{"type": "use", "name": "u", "ref": "helper"}]}
    )
    digest = document_digest(helper_path.read_bytes())
    record_consent(digest, frozenset({"shell"}), store_path=trust_store_path())

    svc = _service(tmp_path, lib_dir)
    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps({"orchestration_path": str(root_path), "dry_run": False}),
    )

    assert response.status_code == 200
    assert response.body["ok"] is True


def test_rest_a_path_run_top_level_document_stays_unaffected(tmp_path: Path) -> None:
    """#284/#334: naming a document by path is never itself gated, with or
    without a `use: ref:` child inside it — only the child is."""
    orch_path = _write_yaml(
        tmp_path / "root.yml",
        {
            "effects": [
                {"type": "tool", "name": "step", "provider": "shell", "params": {"command": "echo"}}
            ]
        },
    )

    svc = RestTriggerService(allow_unauthenticated=True, orchestration_root=tmp_path)
    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps({"orchestration_path": str(orch_path), "dry_run": True}),
    )

    assert response.status_code == 200
    assert response.body["ok"] is True
