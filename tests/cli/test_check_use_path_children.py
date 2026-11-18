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

import pytest

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

    text = _cof(tmp_path, "check", str(root))
    assert text.returncode == 1
    assert "children/child.yml: " in text.stdout
    assert "'template' is a required property" in text.stdout

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

    text = _cof(tmp_path, "check", str(root))
    assert text.returncode == 1
    assert "grandchildren/grandchild.yml: " in text.stdout

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


def test_check_warns_instead_of_failing_on_a_path_child_with_on_error_skip(
    tmp_path: Path,
) -> None:
    """A `use: path:` child that fails to compile is only a warning, not a
    failure, when its own `use` effect sets `on_error: skip` -- the run
    swallows that same failure rather than stopping on it."""
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
    on_error: skip
""",
    )

    report = validate(root, trust_document=True)
    assert report["ok"] is True, report["errors"]
    assert report["errors"] == []
    assert len(report["warnings"]) == 1
    warning = report["warnings"][0]
    assert warning.startswith("children/child.yml: ")
    assert "validation failed" in warning

    text = _cof(tmp_path, "check", str(root))
    assert text.returncode == 0, text.stdout + text.stderr
    assert "children/child.yml: " in text.stdout

    payload = json.loads(_cof(tmp_path, "check", str(root), "--json").stdout)
    assert payload["ok"] is True
    assert payload["warnings"] == report["warnings"]


def test_check_fails_on_a_path_child_that_does_not_exist(tmp_path: Path) -> None:
    """A `use: path:` child naming a file that resolves nowhere on disk
    fails the root's check, named by the document that holds the bad
    reference -- here, the root itself."""
    root = _write(
        tmp_path,
        "root.yml",
        """
effects:
  - type: use
    name: child
    path: missing.yml
""",
    )

    report = validate(root, trust_document=True)
    assert report["ok"] is False
    assert len(report["errors"]) == 1
    error = report["errors"][0]
    assert error.startswith("root.yml: ")
    assert "path 'missing.yml' not found" in error

    payload = json.loads(_cof(tmp_path, "check", str(root), "--json").stdout)
    assert payload["ok"] is False
    assert payload["errors"] == report["errors"]


def test_check_skips_the_schema_check_but_still_compiles_when_validate_is_false(
    tmp_path: Path,
) -> None:
    """`validate: false` on the `use` effect turns off the child's own
    structural check -- but the child still has to compile, and a child
    that can't fails the root's check with the compiler's own error, not
    the schema error `validate: true` would have raised for the same
    file."""
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
    validate: false
""",
    )

    report = validate(root, trust_document=True)
    assert report["ok"] is False
    assert len(report["errors"]) == 1
    error = report["errors"][0]
    assert error.startswith("children/child.yml: ")
    assert "validation failed" not in error
    assert "'template' is a required property" not in error
    assert "must have 'template' or 'messages'" in error

    payload = json.loads(_cof(tmp_path, "check", str(root), "--json").stdout)
    assert payload["ok"] is False
    assert payload["errors"] == report["errors"]


def test_check_follows_a_filesystem_resolved_legacy_orchestration_child(
    tmp_path: Path,
) -> None:
    """The deprecated `use: orchestration:` field, resolved straight from
    the filesystem (never falling back to the library), is followed and
    checked the same way `path:` is -- inheriting the root's own trust,
    since it was never resolved through the library."""
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
    orchestration: children/child.yml
""",
    )

    report = validate(root, trust_document=True)
    assert report["ok"] is False
    assert len(report["errors"]) == 1
    error = report["errors"][0]
    assert error.startswith("children/child.yml: ")
    assert "'file' '../prompts/p.txt' resolves outside the project" in error

    payload = json.loads(_cof(tmp_path, "check", str(root), "--json").stdout)
    assert payload["ok"] is False
    assert payload["errors"] == report["errors"]


def test_check_fails_on_a_child_naming_an_unknown_concurrency_group(
    tmp_path: Path,
) -> None:
    """A `use: path:` child that names a `group:` the root's own
    `runtime.concurrency_groups` doesn't define fails the root's check,
    the same ambient group table a run would check it against."""
    _write(
        tmp_path,
        "children/child.yml",
        """
effects:
  - type: tool
    name: t
    provider: python_eval
    group: unknown_group
    params: {expr: "1"}
""",
    )
    root = _write(
        tmp_path,
        "root.yml",
        """
runtime:
  concurrency_groups:
    known_group: 1
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
    assert "group 'unknown_group' is not defined in runtime.concurrency_groups" in error

    payload = json.loads(_cof(tmp_path, "check", str(root), "--json").stdout)
    assert payload["ok"] is False
    assert payload["errors"] == report["errors"]


def test_check_confines_a_legacy_orchestration_fallback_to_its_own_cache_path(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A legacy `orchestration:` child resolved through the library from a
    remote (github) source is confined to that source's own cached tree,
    not the project containing the cache -- the same boundary the run
    itself gives a remote-pinned child (`UseRuntime._confinement_root_for`)."""
    from circuitry.core.library_ref import ResolvedRef

    (tmp_path / "circuitry.config.json").write_text("{}\n", encoding="utf-8")
    cache_dir = tmp_path / "cache"
    cache_dir.mkdir()
    (cache_dir / "child.yml").write_text(
        "effects:\n"
        "  - type: prompt\n"
        "    name: p\n"
        "    template: {file: ../outside.txt}\n",
        encoding="utf-8",
    )
    (tmp_path / "outside.txt").write_text("hello\n", encoding="utf-8")

    def fake_resolve_ref(ref: str, **_kwargs: object) -> ResolvedRef:
        return ResolvedRef(
            ref=ref,
            source="github",
            path=cache_dir / "child.yml",
            sha="deadbeef",
            cache_path=str(cache_dir),
        )

    monkeypatch.setattr("circuitry.core.library_ref.resolve_ref", fake_resolve_ref)

    root = _write(
        tmp_path,
        "root.yml",
        """
effects:
  - type: use
    name: child
    orchestration: some/pinned_entry
""",
    )

    report = validate(root, trust_document=True)
    assert report["ok"] is False
    assert len(report["errors"]) == 1
    error = report["errors"][0]
    assert "resolves outside the project" in error


def _run_exit_code(tmp_path: Path, root: Path) -> int:
    """``cof run``'s own exit code for *root*, the same way the existing
    CLI tests invoke it -- the outcome `cof check` must agree with."""
    return _cof(tmp_path, "run", str(root), "--skip-preflight").returncode


def _write_boundary_violation_child(tmp_path: Path) -> None:
    """``children/child.yml``, a `{file: ...}` prompt source that escapes
    its own project boundary -- a failure that fails a run exactly the way
    it fails this check's own compile of it, shared by every absorbing-
    ancestor test below."""
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


def test_check_warns_when_a_root_use_continue_absorbs_a_middle_documents_own_use_failure(
    tmp_path: Path,
) -> None:
    """A root `use` with `on_error: continue` over a middle document whose
    own `use` of a broken child has no `on_error` of its own: the run
    still exits 0 (the root's `on_error: continue` absorbs the failure the
    middle document's `use` does not), and `cof check` must agree."""
    _write_boundary_violation_child(tmp_path)
    _write(
        tmp_path,
        "middle.yml",
        """
effects:
  - type: use
    name: child
    path: children/child.yml
""",
    )
    root = _write(
        tmp_path,
        "root.yml",
        """
effects:
  - type: use
    name: mid
    path: middle.yml
    on_error: continue
""",
    )

    assert _run_exit_code(tmp_path, root) == 0

    report = validate(root, trust_document=True)
    assert report["ok"] is True, report["errors"]
    assert report["errors"] == []
    assert len(report["warnings"]) == 1
    assert report["warnings"][0].startswith("children/child.yml: ")

    text = _cof(tmp_path, "check", str(root))
    assert text.returncode == 0, text.stdout + text.stderr


def test_check_warns_when_a_loop_on_error_continue_absorbs_its_bodys_use_failure(
    tmp_path: Path,
) -> None:
    """A `use` with no `on_error` of its own in the `body` of a `loop` with
    `on_error: continue`: the run still exits 0, and `cof check` must
    agree."""
    _write_boundary_violation_child(tmp_path)
    root = _write(
        tmp_path,
        "root.yml",
        """
interface:
  inputs:
    xs:
      type: array
      default: [1, 2]
effects:
  - type: loop
    name: l
    on_error: continue
    each:
      in: input.xs
      as: x
    body:
      - type: use
        name: child
        path: children/child.yml
""",
    )

    assert _run_exit_code(tmp_path, root) == 0

    report = validate(root, trust_document=True)
    assert report["ok"] is True, report["errors"]
    assert report["errors"] == []
    assert len(report["warnings"]) == 1
    assert report["warnings"][0].startswith("children/child.yml: ")

    text = _cof(tmp_path, "check", str(root))
    assert text.returncode == 0, text.stdout + text.stderr


def test_check_warns_when_a_loop_on_error_break_absorbs_its_bodys_use_failure(
    tmp_path: Path,
) -> None:
    """Same as `on_error: continue` above, but the loop's own `on_error`
    is `break`: the run still exits 0, and `cof check` must agree."""
    _write_boundary_violation_child(tmp_path)
    root = _write(
        tmp_path,
        "root.yml",
        """
interface:
  inputs:
    xs:
      type: array
      default: [1, 2]
effects:
  - type: loop
    name: l
    on_error: break
    each:
      in: input.xs
      as: x
    body:
      - type: use
        name: child
        path: children/child.yml
""",
    )

    assert _run_exit_code(tmp_path, root) == 0

    report = validate(root, trust_document=True)
    assert report["ok"] is True, report["errors"]
    assert report["errors"] == []
    assert len(report["warnings"]) == 1
    assert report["warnings"][0].startswith("children/child.yml: ")

    text = _cof(tmp_path, "check", str(root))
    assert text.returncode == 0, text.stdout + text.stderr


def test_check_warns_when_a_dynamic_on_error_continue_absorbs_its_own_use_failure(
    tmp_path: Path,
) -> None:
    """A `use` with no `on_error` of its own inside a `dynamic` with
    `on_error: continue`: the run still exits 0, and `cof check` must
    agree."""
    _write_boundary_violation_child(tmp_path)
    root = _write(
        tmp_path,
        "root.yml",
        """
effects:
  - type: dynamic
    name: d
    on_error: continue
    effects:
      - type: use
        name: child
        path: children/child.yml
""",
    )

    assert _run_exit_code(tmp_path, root) == 0

    report = validate(root, trust_document=True)
    assert report["ok"] is True, report["errors"]
    assert report["errors"] == []
    assert len(report["warnings"]) == 1
    assert report["warnings"][0].startswith("children/child.yml: ")

    text = _cof(tmp_path, "check", str(root))
    assert text.returncode == 0, text.stdout + text.stderr


def test_check_still_fails_when_an_if_on_error_continue_wraps_the_taken_branchs_use_failure(
    tmp_path: Path,
) -> None:
    """A `use` with no `on_error` of its own in the taken branch of an `if`
    with `on_error: continue`: an `if`'s own `on_error` only ever covers a
    failed condition, never the branch it takes, so the run still exits
    nonzero -- and `cof check` must still fail, naming the child, rather
    than treat the `if`'s `on_error: continue` as absorbing it."""
    _write_boundary_violation_child(tmp_path)
    root = _write(
        tmp_path,
        "root.yml",
        """
effects:
  - type: if
    name: cond
    if:
      mode: cel
      expr: "true"
    on_error: continue
    then:
      - type: use
        name: child
        path: children/child.yml
    else: []
""",
    )

    assert _run_exit_code(tmp_path, root) != 0

    report = validate(root, trust_document=True)
    assert report["ok"] is False
    assert len(report["errors"]) == 1
    assert report["errors"][0].startswith("children/child.yml: ")

    text = _cof(tmp_path, "check", str(root))
    assert text.returncode == 1
    assert "children/child.yml: " in text.stdout


def test_check_warns_when_an_outer_use_on_error_continue_absorbs_an_inner_use_on_error_fail(
    tmp_path: Path,
) -> None:
    """An inner `use` with its own, explicit `on_error: fail`, nested
    under an outer `use` with `on_error: continue`: the outer `use`'s own
    `try`/`except` is wide enough to absorb the inner one's failure
    regardless -- a warning, not a failure, matching the run."""
    _write_boundary_violation_child(tmp_path)
    _write(
        tmp_path,
        "middle.yml",
        """
effects:
  - type: use
    name: child
    path: children/child.yml
    on_error: fail
""",
    )
    root = _write(
        tmp_path,
        "root.yml",
        """
effects:
  - type: use
    name: mid
    path: middle.yml
    on_error: continue
""",
    )

    assert _run_exit_code(tmp_path, root) == 0

    report = validate(root, trust_document=True)
    assert report["ok"] is True, report["errors"]
    assert report["errors"] == []
    assert len(report["warnings"]) == 1
    assert report["warnings"][0].startswith("children/child.yml: ")

    payload = json.loads(_cof(tmp_path, "check", str(root), "--json").stdout)
    assert payload["ok"] is True
    assert payload["errors"] == []
    assert payload["warnings"] == report["warnings"]


def test_check_fails_a_two_level_use_path_chain_where_nothing_absorbs(
    tmp_path: Path,
) -> None:
    """A two-level `use: path:` chain where neither `use` sets an
    absorbing `on_error`: the run still exits nonzero, and `cof check`
    still fails, naming the grandchild."""
    _write_boundary_violation_child(tmp_path)
    _write(
        tmp_path,
        "middle.yml",
        """
effects:
  - type: use
    name: child
    path: children/child.yml
""",
    )
    root = _write(
        tmp_path,
        "root.yml",
        """
effects:
  - type: use
    name: mid
    path: middle.yml
""",
    )

    assert _run_exit_code(tmp_path, root) != 0

    report = validate(root, trust_document=True)
    assert report["ok"] is False
    assert len(report["errors"]) == 1
    assert report["errors"][0].startswith("children/child.yml: ")

    payload = json.loads(_cof(tmp_path, "check", str(root), "--json").stdout)
    assert payload["ok"] is False
    assert payload["errors"] == report["errors"]


def test_check_fails_when_the_same_child_is_also_reached_without_the_tolerance(
    tmp_path: Path,
) -> None:
    """The same broken child, reached once through a `use` with
    `on_error: continue` and once through a sibling `use` with no
    `on_error` of its own: the run fails at the second, strict reference
    (each `use` resolves and compiles its own child independently, so the
    first reference succeeding at being tolerated never excuses the
    second) -- `cof check` must fail too, and the failure is reported
    once, as an error, never also as a warning."""
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
    name: a
    path: children/child.yml
    on_error: continue
  - type: use
    name: b
    path: children/child.yml
""",
    )

    assert _run_exit_code(tmp_path, root) != 0

    report = validate(root, trust_document=True)
    assert report["ok"] is False
    assert len(report["errors"]) == 1
    assert report["errors"][0].startswith("children/child.yml: ")
    assert report["warnings"] == []

    payload = json.loads(_cof(tmp_path, "check", str(root), "--json").stdout)
    assert payload["ok"] is False
    assert payload["errors"] == report["errors"]
    assert payload["warnings"] == []


def test_check_fails_when_a_strict_reference_to_an_already_tolerated_child_comes_first(
    tmp_path: Path,
) -> None:
    """The same pair of sibling `use` effects as above, with the strict
    one listed first and the tolerant one second: the outcome does not
    depend on which one the walk reaches first -- still an error, never
    also a warning."""
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
    name: a
    path: children/child.yml
  - type: use
    name: b
    path: children/child.yml
    on_error: continue
""",
    )

    assert _run_exit_code(tmp_path, root) != 0

    report = validate(root, trust_document=True)
    assert report["ok"] is False
    assert len(report["errors"]) == 1
    assert report["errors"][0].startswith("children/child.yml: ")
    assert report["warnings"] == []

    payload = json.loads(_cof(tmp_path, "check", str(root), "--json").stdout)
    assert payload["ok"] is False
    assert payload["errors"] == report["errors"]
    assert payload["warnings"] == []


def test_check_fails_when_a_child_already_tolerated_through_a_middle_document_is_also_reached_directly(
    tmp_path: Path,
) -> None:
    """The same broken child, reached first through a middle document's
    own `use` (inheriting the outer `use`'s `on_error: continue`), then
    again directly from the root with no `on_error`: the run fails at the
    second, direct reference, and `cof check` must agree."""
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
    _write(
        tmp_path,
        "middle.yml",
        """
effects:
  - type: use
    name: inner
    path: children/child.yml
""",
    )
    root = _write(
        tmp_path,
        "root.yml",
        """
effects:
  - type: use
    name: a
    path: middle.yml
    on_error: continue
  - type: use
    name: b
    path: children/child.yml
""",
    )

    assert _run_exit_code(tmp_path, root) != 0

    report = validate(root, trust_document=True)
    assert report["ok"] is False
    assert len(report["errors"]) == 1
    assert report["errors"][0].startswith("children/child.yml: ")
    assert report["warnings"] == []

    payload = json.loads(_cof(tmp_path, "check", str(root), "--json").stdout)
    assert payload["ok"] is False
    assert payload["errors"] == report["errors"]
    assert payload["warnings"] == []


def test_check_fails_when_the_same_child_is_also_reached_with_validation_turned_back_on(
    tmp_path: Path,
) -> None:
    """The same broken child, reached first with `validate: false` under
    an `on_error: continue` `use` (so only the compiler's own error would
    apply, tolerated), then again with the schema check back on and no
    `on_error`: the run fails at the second, strict reference's own
    schema check, and `cof check` must agree."""
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
    name: a
    path: children/child.yml
    on_error: continue
    validate: false
  - type: use
    name: b
    path: children/child.yml
""",
    )

    assert _run_exit_code(tmp_path, root) != 0

    report = validate(root, trust_document=True)
    assert report["ok"] is False
    assert len(report["errors"]) == 1
    error = report["errors"][0]
    assert error.startswith("children/child.yml: ")
    assert "'template' is a required property" in error

    payload = json.loads(_cof(tmp_path, "check", str(root), "--json").stdout)
    assert payload["ok"] is False
    assert payload["errors"] == report["errors"]
