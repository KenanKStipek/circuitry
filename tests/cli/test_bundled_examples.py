"""Tests for the bundled example config files (#346).

``src/circuitry/bundled/examples/config.example.json`` and ``.env.example``
are the only copies of these files in the repo — the docs link the GitHub
path rather than duplicating content that could drift. These tests are what
keeps them in step with the adapters and config loader they document.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

from dotenv import dotenv_values

from circuitry.adapters.factory import build_adapter
from circuitry.cli.config import CONFIG_ENV_VARS, CircuitryConfig
from circuitry.cli.setup import examples_dir
from circuitry.core.store.persistence import build_persistence_backend
from circuitry.plugins.factory import build_plugin

_REPO_ROOT = Path(__file__).resolve().parents[2]
_ADAPTERS_DIR = Path(__file__).resolve().parents[2] / "src" / "circuitry" / "adapters"

#: Credential/endpoint env vars hardcoded in the adapters — scraped from
#: source rather than duplicated here, so a new adapter's env var that is
#: missing from `.env.example` fails this test instead of going unnoticed.
_ENV_VAR_PATTERNS = (
    re.compile(r'os\.environ\.get\(\s*["\'](\w+)["\']'),
    re.compile(r'api_key_env=["\'](\w+)["\']'),
    re.compile(r'api_key_env:\s*str\s*=\s*["\'](\w+)["\']'),
)


def _adapter_credential_env_vars() -> set[str]:
    names: set[str] = set()
    for py_file in _ADAPTERS_DIR.glob("*.py"):
        text = py_file.read_text(encoding="utf-8")
        for pattern in _ENV_VAR_PATTERNS:
            names.update(pattern.findall(text))
    return names


def _env_example_path() -> Path:
    return examples_dir() / ".env.example"


def _config_example_path() -> Path:
    return examples_dir() / "config.example.json"


# ---------------------------------------------------------------------------
# config.example.json
# ---------------------------------------------------------------------------


def test_config_example_is_valid_json() -> None:
    json.loads(_config_example_path().read_text(encoding="utf-8"))


def test_config_example_loads_through_the_real_config_loader() -> None:
    data = json.loads(_config_example_path().read_text(encoding="utf-8"))
    cfg = CircuitryConfig.from_dict(data)

    assert cfg.default_adapter == "ollama"
    assert cfg.default_model
    assert cfg.enabled_adapters
    assert cfg.enabled_tools


def test_config_example_has_no_api_keys() -> None:
    """Keys belong in `.env` only — never in config.json (#308)."""
    text = _config_example_path().read_text(encoding="utf-8")
    credential_names = _adapter_credential_env_vars()
    for name in credential_names:
        assert name not in text, f"{name} must not appear in config.example.json"


def test_config_example_every_top_level_key_is_known() -> None:
    data = json.loads(_config_example_path().read_text(encoding="utf-8"))
    known_top_level = {
        "default_model",
        "default_adapter",
        "plugins",
        "enabled_adapters",
        "enabled_plugins",
        "enabled_tools",
        "environment",
        "runtime",
        "trust_orchestration_runtime",
    }
    assert set(data.keys()) <= known_top_level

    known_runtime = {
        "adapters",
        "plugins",
        "persistence",
        "library",
        "mcp",
        "max_concurrency",
        "concurrency_groups",
        "tools",
        "complexity",
        "state",
    }
    assert set(data["runtime"].keys()) <= known_runtime


def test_config_example_adapters_build_through_the_real_factory() -> None:
    data = json.loads(_config_example_path().read_text(encoding="utf-8"))
    cfg = CircuitryConfig.from_dict(data)

    for name in cfg.enabled_adapters or []:
        build_adapter(adapter_name=name, runtime=cfg.runtime)


def test_config_example_shell_tool_builds_with_its_pinned_allowlist() -> None:
    data = json.loads(_config_example_path().read_text(encoding="utf-8"))
    cfg = CircuitryConfig.from_dict(data)

    plugin = build_plugin(plugin_name="shell", runtime=cfg.runtime)
    assert plugin.pinned_allowed_commands == ("ls", "cat", "echo", "pwd")  # type: ignore[union-attr]


def test_config_example_persistence_backend_builds() -> None:
    data = json.loads(_config_example_path().read_text(encoding="utf-8"))
    cfg = CircuitryConfig.from_dict(data)

    backend = build_persistence_backend(cfg.runtime)
    assert backend is not None
    assert backend.backend_name == "jsonl-file"


# ---------------------------------------------------------------------------
# .env.example
# ---------------------------------------------------------------------------


def test_env_example_parses_with_no_errors() -> None:
    values = dotenv_values(_env_example_path())
    assert values  # at least the active (uncommented) credential lines


def test_env_example_handles_comments_and_blank_lines() -> None:
    """The example file leans on `#` comments and blank separators for
    readability — confirms the parser Circuitry ships (python-dotenv)
    tolerates both rather than choking on or silently misreading them."""
    text = _env_example_path().read_text(encoding="utf-8")
    assert "\n\n" in text
    assert re.search(r"^#", text, re.MULTILINE)

    values = dotenv_values(_env_example_path())
    assert "WATSONX_REGION" not in values, (
        "commented-out optional settings must not parse as active keys"
    )


def test_every_adapter_credential_env_var_appears_in_env_example() -> None:
    """Drift test: a new adapter's hardcoded credential/base-url env var
    must be added to .env.example (#346)."""
    text = _env_example_path().read_text(encoding="utf-8")
    missing = [
        name
        for name in _adapter_credential_env_vars()
        if not re.search(rf"^#?\s*{re.escape(name)}=", text, re.MULTILINE)
    ]
    assert not missing, f"Missing from .env.example: {sorted(missing)}"


def test_every_circuitry_config_env_var_appears_in_env_example() -> None:
    text = _env_example_path().read_text(encoding="utf-8")
    missing = [
        name
        for name in CONFIG_ENV_VARS
        if not re.search(rf"^#?\s*{re.escape(name)}=", text, re.MULTILINE)
    ]
    assert not missing, f"Missing from .env.example: {sorted(missing)}"


def test_env_example_values_are_placeholders_not_real_secrets() -> None:
    values = dotenv_values(_env_example_path())
    for name, value in values.items():
        assert value is not None
        looks_like_url = value.startswith("http")
        assert "your" in value.lower() or looks_like_url, (
            f"{name}={value!r} doesn't read as an obvious placeholder"
        )
    # URLs are placeholders too, just shaped like endpoints, not secrets.
    for name, value in values.items():
        if value and value.startswith("http"):
            assert "your-" in value or "your." in value, (
                f"{name}={value!r} doesn't read as an obvious placeholder URL"
            )


# ---------------------------------------------------------------------------
# Packaging — both files must ship in the wheel (dotfiles don't match a `*`
# glob in setuptools' package-data, so they need a literal entry).
# ---------------------------------------------------------------------------


def test_pyproject_package_data_lists_both_example_files_literally() -> None:
    """Dotfiles don't match a `*` glob in setuptools' package-data, so both
    need a literal entry — regex rather than ``tomllib`` to parse, since
    that's stdlib only from Python 3.11 and CI tests 3.10 too (see
    ``tests/test_py_typed.py`` for the same pattern).
    """
    pyproject = (_REPO_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    package_data_match = re.search(
        r"\[tool\.setuptools\.package-data\]\s*circuitry\s*=\s*\[(.*?)\]",
        pyproject,
        re.DOTALL,
    )
    assert package_data_match is not None, (
        "pyproject.toml's [tool.setuptools.package-data] circuitry list not found"
    )
    package_data = package_data_match.group(1)

    assert '"bundled/examples/.env.example"' in package_data
    assert '"bundled/examples/config.example.json"' in package_data


def test_examples_dir_resolves_to_both_installed_files() -> None:
    assert (examples_dir() / ".env.example").is_file()
    assert (examples_dir() / "config.example.json").is_file()
