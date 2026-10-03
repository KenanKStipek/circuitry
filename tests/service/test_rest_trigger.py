from __future__ import annotations

import json
import uuid
from pathlib import Path

import pytest

from circuitry.cli import config as config_module
from circuitry.service import RestTriggerService


def _write_orchestration(path: Path) -> None:
    path.write_text(
        """
name: hello_root
adapter: openai
model: gpt-4o-mini
effects:
  - type: prompt
    name: hello
    template: "hi"
""".strip()
        + "\n",
        encoding="utf-8",
    )


def test_construction_loads_user_env(tmp_path: Path) -> None:
    """`RestTriggerService` is a host entry point like the CLI and the MCP
    server — the `.env` `cof setup` writes must reach a run it triggers
    (#348)."""
    import os

    canary = "sk-canary-rest-9f3a1c"
    config_module.GLOBAL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    env_path = config_module.GLOBAL_CONFIG_DIR / ".env"
    env_path.write_text(f"OPENAI_API_KEY={canary}\n", encoding="utf-8")
    env_path.chmod(0o600)
    os.environ.pop("OPENAI_API_KEY", None)

    try:
        RestTriggerService(allow_unauthenticated=True, orchestration_root=tmp_path)
        assert os.environ.get("OPENAI_API_KEY") == canary
    finally:
        os.environ.pop("OPENAI_API_KEY", None)


def test_rest_trigger_success_returns_request_tracking_metadata(tmp_path: Path) -> None:
    orch_path = tmp_path / "hello.yml"
    _write_orchestration(orch_path)

    svc = RestTriggerService(auth_token="secret", orchestration_root=tmp_path)
    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={
            "Authorization": "Bearer secret",
            "X-Request-ID": "req-123",
        },
        body=json.dumps(
            {
                "orchestration_path": str(orch_path),
                "dry_run": True,
                "state": {"input": {"name": "Elena"}},
            }
        ),
    )

    assert response.status_code == 200
    assert response.headers["x-request-id"] == "req-123"
    assert response.body["ok"] is True
    assert response.body["status"] == "succeeded"
    assert response.body["request_id"] == "req-123"

    trigger = response.body["runtime"]["trigger"]
    assert trigger["request_id"] == "req-123"
    assert trigger["interface"] == "rest"
    assert trigger["status"] == "succeeded"
    assert trigger["completed_at"] is not None

    last_run = response.body["runtime"]["last_run"]
    assert isinstance(last_run, dict)
    assert last_run["orchestration_path"] == str(orch_path)


def test_rest_trigger_requires_auth_token_or_explicit_opt_out() -> None:
    with pytest.raises(ValueError, match="allow_unauthenticated"):
        RestTriggerService()


def test_rest_trigger_allow_unauthenticated_opt_out_constructs() -> None:
    RestTriggerService(allow_unauthenticated=True)


def test_rest_trigger_rejects_missing_or_invalid_token() -> None:
    svc = RestTriggerService(auth_token="secret")
    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps({"orchestration_path": "src/circuitry/curation/learn/dynamic_chain.yml"}),
    )

    assert response.status_code == 401
    assert response.body["ok"] is False
    assert "Unauthorized" in response.body["error"]


def test_rest_trigger_rejects_invalid_payload_shape() -> None:
    svc = RestTriggerService(allow_unauthenticated=True)
    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps({"dry_run": True}),
    )

    assert response.status_code == 400
    assert response.body["ok"] is False
    assert "orchestration_path" in response.body["error"]


def test_rest_trigger_rejects_orchestration_path_outside_root(tmp_path: Path) -> None:
    svc = RestTriggerService(allow_unauthenticated=True, orchestration_root=tmp_path / "root")
    (tmp_path / "root").mkdir()
    outside = tmp_path / "outside.yml"
    _write_orchestration(outside)

    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps({"orchestration_path": str(outside)}),
    )

    assert response.status_code == 400
    assert response.body["ok"] is False
    assert "orchestration_path" in response.body["error"]
    assert "orchestration root" in response.body["error"]


def test_rest_trigger_rejects_nonexistent_orchestration_path_as_400(tmp_path: Path) -> None:
    """A caller-supplied path that doesn't resolve is a 400 caller error, the
    same as the missing-field case — not the 500 a FileNotFoundError used to
    surface through the generic run-failure path (#265 part 5)."""
    svc = RestTriggerService(allow_unauthenticated=True, orchestration_root=tmp_path)

    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps({"orchestration_path": "does/not/exist.yaml"}),
    )

    assert response.status_code == 400
    assert response.body["ok"] is False
    assert "does not exist" in response.body["error"]


def test_rest_trigger_rejects_out_path_outside_root(tmp_path: Path) -> None:
    root = tmp_path / "root"
    root.mkdir()
    orch_path = root / "hello.yml"
    _write_orchestration(orch_path)
    svc = RestTriggerService(allow_unauthenticated=True, orchestration_root=root)

    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps(
            {
                "orchestration_path": str(orch_path),
                "out_path": str(tmp_path / "escape.json"),
            }
        ),
    )

    assert response.status_code == 400
    assert response.body["ok"] is False
    assert "out_path" in response.body["error"]
    assert "orchestration root" in response.body["error"]


def test_rest_trigger_accepts_relative_path_inside_root(tmp_path: Path) -> None:
    orch_path = tmp_path / "hello.yml"
    _write_orchestration(orch_path)
    svc = RestTriggerService(allow_unauthenticated=True, orchestration_root=tmp_path)

    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps({"orchestration_path": "hello.yml", "dry_run": True}),
    )

    assert response.status_code == 200, response.body


def test_rest_trigger_without_explicit_config_applies_host_shell_pin(tmp_path: Path) -> None:
    """config=None (the default) no longer means a bare, allowlist-open
    CircuitryConfig() — the service resolves the host config the same way
    `cof run` would, so a host-level `runtime.plugins.shell.allowed_commands`
    pin applies to a REST run even though the request never names it."""
    config_module.GLOBAL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    config_module.GLOBAL_CONFIG_PATH.write_text(
        json.dumps({"runtime": {"plugins": {"shell": {"allowed_commands": ["echo"]}}}}),
        encoding="utf-8",
    )

    orch_path = tmp_path / "shell.yml"
    orch_path.write_text(
        """
effects:
  - type: tool
    name: step
    provider: shell
    params:
      command: uname
      allowed_commands: [uname]
      args: ["-s"]
""".strip()
        + "\n",
        encoding="utf-8",
    )

    svc = RestTriggerService(allow_unauthenticated=True, orchestration_root=tmp_path)
    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps({"orchestration_path": str(orch_path), "dry_run": False}),
    )

    assert response.status_code == 500
    assert response.body["ok"] is False
    assert "allowed_commands" in response.body["error"]
    assert "pin" in response.body["error"]


def test_rest_trigger_rejects_relative_dotdot_escape(tmp_path: Path) -> None:
    root = tmp_path / "root"
    root.mkdir()
    outside = tmp_path / "outside.yml"
    _write_orchestration(outside)
    svc = RestTriggerService(allow_unauthenticated=True, orchestration_root=root)

    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps({"orchestration_path": "../outside.yml"}),
    )

    assert response.status_code == 400
    assert response.body["ok"] is False
    assert "orchestration root" in response.body["error"]


def test_rest_trigger_rejects_symlink_escape(tmp_path: Path) -> None:
    root = tmp_path / "root"
    root.mkdir()
    outside = tmp_path / "outside.yml"
    _write_orchestration(outside)
    link = root / "link.yml"
    link.symlink_to(outside)
    svc = RestTriggerService(allow_unauthenticated=True, orchestration_root=root)

    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps({"orchestration_path": "link.yml"}),
    )

    assert response.status_code == 400
    assert response.body["ok"] is False
    assert "orchestration root" in response.body["error"]


def test_rest_trigger_rejects_nul_byte_path_as_400_not_raise(tmp_path: Path) -> None:
    svc = RestTriggerService(allow_unauthenticated=True, orchestration_root=tmp_path)

    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps({"orchestration_path": "evil\x00.yml"}),
    )

    assert response.status_code == 400
    assert response.body["ok"] is False
    assert "orchestration_path" in response.body["error"]


def test_rest_trigger_returns_runtime_failure_details(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A path that exists but fails at run time (here: preflight, missing
    adapter credentials) is still a 500 — only a caller-supplied nonexistent
    path is a 400 (#265 part 5)."""
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    orch_path = tmp_path / "hello.yml"
    _write_orchestration(orch_path)
    svc = RestTriggerService(allow_unauthenticated=True, orchestration_root=tmp_path)
    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={"X-Request-ID": "req-fail"},
        body=json.dumps(
            {
                "orchestration_path": str(orch_path),
                "dry_run": False,
            }
        ),
    )

    assert response.status_code == 500
    assert response.body["ok"] is False
    assert response.body["status"] == "failed"
    assert response.body["request_id"] == "req-fail"
    assert response.body["error"]

    trigger = response.body["runtime"]["trigger"]
    assert trigger["status"] == "failed"
    assert trigger["error"] == response.body["error"]


@pytest.mark.parametrize(
    "bad_request_id",
    [
        "a" * 129,  # over the length cap
        "has spaces",
        "line\ninjection",
        "has/slash",
        "\x00null",
        "",
    ],
)
def test_rest_trigger_replaces_an_invalid_request_id_with_a_fresh_uuid(
    tmp_path: Path, bad_request_id: str
) -> None:
    """An out-of-shape client-supplied x-request-id (#269 item 10) must not
    be echoed back or persisted verbatim — a fresh UUID replaces it."""
    svc = RestTriggerService(allow_unauthenticated=True, orchestration_root=tmp_path)
    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={"X-Request-ID": bad_request_id},
        body=json.dumps(
            {"orchestration_path": str(tmp_path / "missing.yml"), "dry_run": True}
        ),
    )

    assert response.body["request_id"] != bad_request_id
    assert response.headers["x-request-id"] != bad_request_id
    uuid.UUID(response.body["request_id"])  # does not raise


def test_rest_trigger_accepts_a_well_formed_request_id(tmp_path: Path) -> None:
    svc = RestTriggerService(allow_unauthenticated=True, orchestration_root=tmp_path)
    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={"X-Request-ID": "a-fine_ID-123"},
        body=json.dumps(
            {"orchestration_path": str(tmp_path / "missing.yml"), "dry_run": True}
        ),
    )

    assert response.body["request_id"] == "a-fine_ID-123"
