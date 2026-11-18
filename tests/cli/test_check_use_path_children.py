"""`cof check` compiles a `use: path:` child the same way a run would (#832).

A child that a run would refuse -- a `{file: ...}` prompt source outside its
project boundary, a schema violation, a failing grandchild two `use: path:`
levels down -- must fail the root's own check, named by its own path
relative to the root, with the same error message the run would raise
reaching it. #611's own secrets line (`tests/plugins/test_http_secrets.py`)
must keep working unchanged.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

from circuitry.cli.runtime_shim import validate

_CREDENTIAL_ENV_VARS = ("OPENAI_API_KEY", "CYBERDINER_TOKEN", "CYBERDINER_EXPO_URL")


def _write(tmp_path: Path, name: str, content: str) -> Path:
    path = tmp_path / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content.lstrip(), encoding="utf-8")
    return path


def _cli_env(home: Path) -> dict[str, str]:
    env = {
        key: value for key, value in os.environ.items() if key not in _CREDENTIAL_ENV_VARS
    }
    env["HOME"] = str(home)
    env["COLUMNS"] = "200"
    return env


def _cof(tmp_path: Path, *args: str) -> subprocess.CompletedProcess[str]:
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    return subprocess.run(
        [sys.executable, "-m", "circuitry.cli.app", *args],
        capture_output=True,
        text=True,
        env=_cli_env(home),
        cwd=tmp_path,
        timeout=120,
        check=False,
    )


def test_check_fails_on_a_path_child_file_outside_its_project(tmp_path: Path) -> None:
    """A `use: path:` child whose `{file: ...}` escapes its own project
    fails the root's check, the same error a run of the root raises
    reaching it."""
    _write(tmp_path, "prompts/p.txt", "hello\n")
    _write(
        tmp_path,
        "children/child.yml",
        """
effects:
  - type: prompt
    name: p
    template: {file: ../prompts/p.txt}
""",
    )
    root = _write(
        tmp_path,
        "root.yml",
        """
effects:
  - type: use
    name: child
    path: children/child.yml
""",
    )

    report = validate(root, trust_document=True)
    assert report["ok"] is False
    assert len(report["errors"]) == 1
    error = report["errors"][0]
    assert error.startswith("children/child.yml: ")
    assert "'file' '../prompts/p.txt' resolves outside the project" in error

    text = _cof(tmp_path, "check", str(root))
    assert text.returncode == 1
    assert "children/child.yml: " in text.stdout
    assert "resolves outside the project" in text.stdout

    payload = json.loads(_cof(tmp_path, "check", str(root), "--json").stdout)
    assert payload["ok"] is False
    assert payload["errors"] == report["errors"]


def test_check_fails_on_a_path_child_schema_error(tmp_path: Path) -> None:
    """A `use: path:` child with a schema violation fails the root's check
    with the same structural error a run would raise loading it."""
    _write(
        tmp_path,
        "children/child.yml",
        """
effects:
  - type: prompt
    name: p
    not_a_real_field: true
""",
    )
    root = _write(
        tmp_path,
        "root.yml",
        """
effects:
  - type: use
    name: child
    path: children/child.yml
""",
    )

    report = validate(root, trust_document=True)
    assert report["ok"] is False
    assert len(report["errors"]) == 1
    error = report["errors"][0]
    assert error.startswith("children/child.yml: ")
    assert "validation failed" in error
    assert "'template' is a required property" in error

    payload = json.loads(_cof(tmp_path, "check", str(root), "--json").stdout)
    assert payload["ok"] is False
    assert payload["errors"] == report["errors"]


def test_check_follows_a_two_level_use_path_chain_to_a_failing_grandchild(
    tmp_path: Path,
) -> None:
    """A two-level `use: path:` chain where only the grandchild fails to
    compile still fails the root's check, naming the grandchild's own path
    relative to the root."""
    _write(
        tmp_path,
        "grandchildren/grandchild.yml",
        """
effects:
  - type: prompt
    name: p
    not_a_real_field: true
""",
    )
    _write(
        tmp_path,
        "children/child.yml",
        """
effects:
  - type: use
    name: grandchild
    path: ../grandchildren/grandchild.yml
""",
    )
    root = _write(
        tmp_path,
        "root.yml",
        """
effects:
  - type: use
    name: child
    path: children/child.yml
""",
    )

    report = validate(root, trust_document=True)
    assert report["ok"] is False
    assert len(report["errors"]) == 1
    error = report["errors"][0]
    assert error.startswith("grandchildren/grandchild.yml: ")
    assert "validation failed" in error

    payload = json.loads(_cof(tmp_path, "check", str(root), "--json").stdout)
    assert payload["ok"] is False
    assert payload["errors"] == report["errors"]


def test_check_passes_a_valid_use_path_tree_with_secrets_line_unchanged(
    tmp_path: Path,
) -> None:
    """A valid `use: path:` tree still passes, and #611's secrets listing
    for a child-named secret keeps working unchanged alongside this check."""
    _write(
        tmp_path,
        "children/child.yml",
        """
effects:
  - type: tool
    name: call
    provider: http
    params:
      url: "https://example.com/"
      headers: {Authorization: {secret: x}}
""",
    )
    root = _write(
        tmp_path,
        "root.yml",
        """
effects:
  - type: use
    name: child
    path: children/child.yml
""",
    )

    report = validate(root, trust_document=True)
    assert report["ok"] is True, report["errors"]
    assert report["errors"] == []
    assert report["secrets"] == ["x"]
    assert report["secret_sources"] == {"x": "not defined"}
    assert report["secret_children"] == {
        "x": [str((tmp_path / "children" / "child.yml").resolve())]
    }

    text = _cof(tmp_path, "check", str(root))
    assert text.returncode == 0, text.stdout + text.stderr
    assert "Secrets: x (not defined; named by children/child.yml)" in text.stdout

    payload = json.loads(_cof(tmp_path, "check", str(root), "--json").stdout)
    assert payload["ok"] is True
    assert payload["secret_children"] == report["secret_children"]
