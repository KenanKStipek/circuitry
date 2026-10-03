"""`cof --help`/`version` shouldn't pay for the whole runtime stack (#269 item 1).

`circuitry.core.compiler` pulls in `core.cel_eval` (celpy/lark, a full CEL
grammar parser) and `circuitry.cli.app` pulls in every adapter SDK via
`runtime_shim`/`score`/`doctor`. None of that is needed to print help text or
a version string, so importing the CLI module alone must not import those
modules — run in a subprocess so `sys.modules` isn't polluted by whatever
else this test session already imported.
"""

from __future__ import annotations

import os
import subprocess
import sys


def _modules_after_import(expr: str) -> set[str]:
    result = subprocess.run(
        [sys.executable, "-c", f"{expr}\nimport sys; print('\\n'.join(sorted(sys.modules)))"],
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    return set(result.stdout.splitlines())


def test_importing_cli_app_does_not_load_the_cel_grammar_parser() -> None:
    modules = _modules_after_import("import circuitry.cli.app")
    assert "circuitry.core.cel_eval" not in modules
    assert "celpy" not in modules
    assert "lark" not in modules


def test_importing_cli_app_does_not_build_the_adapter_registry() -> None:
    modules = _modules_after_import("import circuitry.cli.app")
    assert "circuitry.adapters" not in modules


def test_bare_circuitry_import_does_not_load_the_api_module() -> None:
    """`import circuitry` (which any `circuitry.cli.*` submodule import
    triggers as a parent-package init) must not cascade into `circuitry.api`
    -> `cli.runtime_shim` -> `core.compiler` -> `core.cel_eval`."""
    modules = _modules_after_import("import circuitry")
    assert "circuitry.api" not in modules
    assert "circuitry.core.cel_eval" not in modules


def test_cof_check_still_validates_correctly_with_lazy_imports(tmp_path) -> None:
    """The deferred imports must still resolve correctly once a command that
    actually needs them runs."""
    orch = tmp_path / "minimal.yml"
    orch.write_text(
        "effects:\n  - type: prompt\n    name: hello\n    template: Hi\n",
        encoding="utf-8",
    )
    # A fake $HOME keeps this off the real ~/.config/circuitry/config.json
    # (CLAUDE.md hermeticity) — CIRCUITRY_CACHE_DIR alone (set by conftest)
    # isn't enough, since config discovery still reads the real HOME.
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    result = subprocess.run(
        [sys.executable, "-m", "circuitry.cli.app", "check", str(orch)],
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
        env={**os.environ, "HOME": str(home)},
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert "Valid" in result.stdout
