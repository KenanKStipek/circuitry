#!/usr/bin/env python3
"""electricity-config's golden corpus (issue #431's Test strategy
section, lane D1): ``resolve_config``'s defaults/deep-merge/env-overlay
port, every one of its own error texts, ``_merge_runtime``'s ceiling
re-intersection, and ``resolve_effective_settings``'s ``sources`` order
and warnings -- each generated from Circuitry's own real functions
(``circuitry.cli.config``, ``circuitry.cli.effective_settings``,
``circuitry.cli.complexity_config``, ``circuitry.core.store.
persistence``), narrowed to electricity's own explicit-path/
always-trusted shape (no global/project config discovery, no `cli`/
`profile`/`resume` precedence tier -- see `electricity-config`'s own
crate docs).

Every case's config file is written into a fresh temporary directory;
every text field is checked for -- and has -- that directory's own path
replaced with the literal ``<root>`` before being written out, so the
committed corpus carries no local path (``LANE-CONTRACT.md``).

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI, same as every other generator in this directory). Usage:
    python3 generate_config_corpus.py [--check]
"""

from __future__ import annotations

import json
import os
import re
import sys
import tempfile
from contextlib import contextmanager
from pathlib import Path
from typing import Any

# Generators may import Circuitry itself so every expectation comes from
# its own real code, never a re-implementation (see
# .github/workflows/electricity-generated.yml).
from circuitry.cli.complexity_config import (
    ComplexityConfigError,
    resolve_complexity_settings,
)
from circuitry.cli.config import CircuitryConfig, ConfigError, resolve_config
from circuitry.cli.effective_settings import resolve_effective_settings
from circuitry.core.store.persistence import build_persistence_backend

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-config"
    / "tests"
    / "golden"
    / "config_corpus.json"
)

#: Every `CONFIG_ENV_VARS` entry plus `CIRCUITRY_CONFIG`/
#: `CIRCUITRY_TRUST_ORCHESTRATION_RUNTIME` (not electricity's own
#: concern, but still cleared so a stray export in the generator's own
#: shell can never leak into a case) -- cleared before every case, then
#: a case's own `env` overlaid on top.
_ALL_CIRCUITRY_ENV_VARS = (
    "CIRCUITRY_CONFIG",
    "CIRCUITRY_MODEL",
    "CIRCUITRY_ADAPTER",
    "CIRCUITRY_ADAPTER_URL",
    "CIRCUITRY_COMFYUI_URL",
    "CIRCUITRY_ENABLED_ADAPTERS",
    "CIRCUITRY_ENABLED_PLUGINS",
    "CIRCUITRY_ENABLED_TOOLS",
    "CIRCUITRY_ENVIRONMENT",
    "CIRCUITRY_TRUST_ORCHESTRATION_RUNTIME",
)

_LEAKED_PATH_PATTERNS = [
    re.compile(re.escape(str(Path.home()))),
    re.compile(re.escape(tempfile.gettempdir())),
    re.compile(r"/private/"),
]


@contextmanager
def _clean_env(overlay: dict[str, str] | None = None):
    saved = {k: os.environ.get(k) for k in _ALL_CIRCUITRY_ENV_VARS}
    try:
        for key in _ALL_CIRCUITRY_ENV_VARS:
            os.environ.pop(key, None)
        for key, value in (overlay or {}).items():
            os.environ[key] = value
        yield
    finally:
        for key, value in saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


def _redact(text: str, root: str) -> str:
    return text.replace(root, "<root>")


def encode_config(cfg: CircuitryConfig) -> dict[str, Any]:
    return {
        "default_model": cfg.default_model,
        "default_adapter": cfg.default_adapter,
        "plugins": list(cfg.plugins),
        "enabled_adapters": cfg.enabled_adapters,
        "enabled_plugins": cfg.enabled_plugins,
        "enabled_tools": cfg.enabled_tools,
        "environment": cfg.environment,
        "runtime": cfg.runtime,
    }


def resolve_config_case(
    name: str,
    *,
    root: Path,
    file_text: str | None,
    is_directory: bool = False,
    env: dict[str, str] | None = None,
) -> dict[str, Any]:
    """One `resolve_config` case: *file_text* (`None` -> no file at all)
    written to a fresh `config.json` inside its own case directory (so
    one case's file can never collide with another's), then resolved
    with *env* as the only `CIRCUITRY_*` variables set."""
    case_dir = root / name
    case_dir.mkdir(parents=True)
    if is_directory:
        path = case_dir / "config.json"
        path.mkdir()
    elif file_text is not None:
        path = case_dir / "config.json"
        path.write_text(file_text, encoding="utf-8")
    else:
        path = case_dir / "config.json"  # never written -- FileNotFoundError

    input_ = {
        "file_text": file_text,
        "is_directory": is_directory,
        "env": env or {},
    }
    with _clean_env(env):
        try:
            cfg = resolve_config(explicit_path=path, cwd=case_dir)
        except ConfigError as exc:
            return {
                "name": name,
                "kind": "resolve_config",
                "input": input_,
                "expect": {"ok": False, "error": _redact(str(exc), str(root))},
            }
    return {
        "name": name,
        "kind": "resolve_config",
        "input": input_,
        "expect": {"ok": True, "config": encode_config(cfg)},
    }


def effective_settings_case(
    name: str,
    *,
    config: dict[str, Any],
    orch: dict[str, Any],
) -> dict[str, Any]:
    """One `resolve_effective_settings` case -- *config* is a raw dict
    turned into a `CircuitryConfig` directly (bypassing `resolve_config`:
    this exercises the merge/sources/warnings logic on an
    already-resolved config, same as `electricity_config::
    effective_settings`'s own signature takes one already-resolved).
    `trust_document=True` always -- electricity's own narrowing (every
    document it runs is named by path, see that crate's doc comment)."""
    cfg = CircuitryConfig.from_dict(config)
    try:
        settings = resolve_effective_settings(cfg=cfg, orch=orch, trust_document=True)
    except (ValueError, ComplexityConfigError) as exc:
        return {
            "name": name,
            "kind": "effective_settings",
            "input": {"config": config, "orch": orch},
            "expect": {"ok": False, "error": str(exc)},
        }
    return {
        "name": name,
        "kind": "effective_settings",
        "input": {"config": config, "orch": orch},
        "expect": {
            "ok": True,
            "model": settings.model,
            "adapter": settings.adapter,
            "plugins": settings.plugins,
            "runtime": settings.runtime,
            "sources": list(settings.sources.items()),
            "warnings": list(settings.warnings),
        },
    }


def complexity_case(name: str, *, runtime: dict[str, Any]) -> dict[str, Any]:
    try:
        resolve_complexity_settings(runtime)
    except ComplexityConfigError as exc:
        return {
            "name": name,
            "kind": "complexity",
            "input": {"runtime": runtime},
            "expect": {"ok": False, "error": str(exc)},
        }
    return {
        "name": name,
        "kind": "complexity",
        "input": {"runtime": runtime},
        "expect": {"ok": True},
    }


def persistence_case(name: str, *, runtime: dict[str, Any]) -> dict[str, Any]:
    try:
        build_persistence_backend(runtime)
    except ValueError as exc:
        return {
            "name": name,
            "kind": "persistence",
            "input": {"runtime": runtime},
            "expect": {"ok": False, "error": str(exc)},
        }
    return {
        "name": name,
        "kind": "persistence",
        "input": {"runtime": runtime},
        "expect": {"ok": True},
    }


def build_cases(root: Path) -> list[dict[str, Any]]:
    cases: list[dict[str, Any]] = []

    # --- resolve_config: defaults, deep merge, env overlays, every error text ---
    cases.append(resolve_config_case("defaults_only", root=root, file_text="{}"))
    cases.append(
        resolve_config_case(
            "user_file_deep_merges_over_defaults",
            root=root,
            file_text=json.dumps(
                {
                    "default_model": "gpt-4",
                    "runtime": {"adapters": {"openai": {"base_url": "https://api.openai.com"}}},
                }
            ),
        )
    )
    cases.append(
        resolve_config_case(
            "env_overlays_win_over_the_file",
            root=root,
            file_text=json.dumps({"default_model": "gpt-4"}),
            env={"CIRCUITRY_MODEL": "llama3.3", "CIRCUITRY_ENABLED_TOOLS": "json, shell"},
        )
    )
    cases.append(
        resolve_config_case(
            "circuitry_adapter_url_targets_the_resolved_default_adapter",
            root=root,
            file_text=json.dumps({"default_adapter": "openai"}),
            env={"CIRCUITRY_ADAPTER_URL": "https://example.test"},
        )
    )
    cases.append(
        resolve_config_case(
            "an_unknown_environment_falls_back_to_dev",
            root=root,
            file_text=json.dumps({"environment": "staging"}),
        )
    )
    cases.append(resolve_config_case("missing_file", root=root, file_text=None))
    cases.append(resolve_config_case("config_path_is_a_directory", root=root, file_text=None, is_directory=True))
    cases.append(resolve_config_case("invalid_json_syntax", root=root, file_text="{\n  \"a\": ,\n}\n"))
    for label, text in [
        ("non_object_root_array", "[1, 2, 3]"),
        ("non_object_root_string", '"hello"'),
        ("non_object_root_bool", "true"),
        ("non_object_root_number", "5"),
        ("non_object_root_null", "null"),
    ]:
        cases.append(resolve_config_case(label, root=root, file_text=text))

    # --- _merge_runtime ceiling re-intersection ---
    cases.append(
        effective_settings_case(
            "shell_allowlist_ceiling_narrows_but_never_widens",
            config={
                "runtime": {
                    "plugins": {"shell": {"allowed_commands": ["ls", "cat"]}},
                }
            },
            orch={
                "runtime": {
                    "plugins": {"shell": {"allowed_commands": ["cat", "rm", "curl"]}},
                }
            },
        )
    )
    cases.append(
        effective_settings_case(
            "shell_allowlist_ceiling_survives_a_document_with_no_shell_block_at_all",
            config={"runtime": {"plugins": {"shell": {"allowed_commands": ["ls"]}}}},
            orch={"runtime": {"adapters": {}}},
        )
    )

    # --- resolve_effective_settings: sources order, warnings, router precedence ---
    cases.append(
        effective_settings_case(
            "sources_in_insertion_order_with_no_persistence_entry",
            config={"default_model": "llama3.1:8b", "default_adapter": "ollama"},
            orch={},
        )
    )
    cases.append(
        effective_settings_case(
            "a_document_model_outranks_the_config_default",
            config={"default_model": "llama3.1:8b"},
            orch={"model": "gpt-4"},
        )
    )
    cases.append(
        effective_settings_case(
            "applied_host_settings_notice_names_dotted_runtime_paths",
            config={},
            orch={"runtime": {"max_concurrency": 2}},
        )
    )
    cases.append(
        effective_settings_case(
            "persistence_source_is_orchestration_when_the_document_configures_it",
            config={},
            orch={
                "runtime": {
                    "persistence": {"enabled": True, "backend": "sqlite", "db_path": "a.db"}
                }
            },
        )
    )
    cases.append(
        effective_settings_case(
            "non_string_plugin_entry_is_rejected",
            config={},
            orch={"plugins": [1]},
        )
    )
    cases.append(
        effective_settings_case(
            "a_malformed_complexity_block_surfaces_the_complexity_config_error",
            config={},
            orch={"runtime": {"complexity": {"routing": {"enabled": True}}}},
        )
    )
    cases.append(
        effective_settings_case(
            "a_catch_all_band_wins_the_model_when_routing_is_enabled_and_none_is_pinned",
            config={},
            orch={
                "runtime": {
                    "complexity": {
                        "scoring": {"enabled": True},
                        "routing": {
                            "enabled": True,
                            "bands": [{"model": "router-model"}],
                        },
                    }
                }
            },
        )
    )
    cases.append(
        effective_settings_case(
            "adapter_timeout_seconds_source_is_orchestration",
            config={"default_adapter": "ollama"},
            orch={"runtime": {"adapters": {"ollama": {"timeout_seconds": 30}}}},
        )
    )

    # --- resolve_complexity_settings: every validation message exercised ---
    cases.append(complexity_case("complexity_absent_resolves_to_defaults", runtime={}))
    cases.append(
        complexity_case(
            "complexity_routing_enabled_without_bands_is_an_error",
            runtime={"complexity": {"routing": {"enabled": True}}},
        )
    )
    cases.append(
        complexity_case(
            "complexity_routing_without_scoring_is_a_prerequisite_error",
            runtime={
                "complexity": {
                    "routing": {"enabled": True, "bands": [{"model": "gpt-4"}]}
                }
            },
        )
    )
    cases.append(
        complexity_case(
            "complexity_non_catch_all_last_band_is_rejected",
            runtime={
                "complexity": {
                    "scoring": {"enabled": True},
                    "routing": {
                        "enabled": True,
                        "bands": [{"model": "gpt-4", "max": 10}],
                    },
                }
            },
        )
    )
    cases.append(
        complexity_case(
            "complexity_score_out_of_range_names_the_bound",
            runtime={"complexity": {"decomposition": {"threshold": 150}}},
        )
    )
    cases.append(
        complexity_case(
            "complexity_unknown_key_is_rejected",
            runtime={"complexity": {"bogus": True}},
        )
    )
    cases.append(
        complexity_case(
            "complexity_unknown_scoring_weight_signal_is_rejected",
            runtime={"complexity": {"scoring": {"weights": {"bogus_signal": 1.0}}}},
        )
    )

    # --- build_persistence_backend: the alias lookup and per-backend validation ---
    cases.append(persistence_case("persistence_disabled_is_never_validated", runtime={"persistence": {"backend": "nonsense"}}))
    cases.append(
        persistence_case(
            "persistence_unsupported_backend_name",
            runtime={"persistence": {"enabled": True, "backend": "dynamodb"}},
        )
    )
    cases.append(
        persistence_case(
            "persistence_postgres_requires_a_dsn",
            runtime={"persistence": {"enabled": True, "backend": "postgres"}},
        )
    )
    cases.append(
        persistence_case(
            "persistence_postgres_rejects_an_insecure_sslmode_without_the_opt_out",
            runtime={
                "persistence": {
                    "enabled": True,
                    "backend": "postgres",
                    "dsn": "postgres://localhost/db",
                    "sslmode": "disable",
                }
            },
        )
    )
    cases.append(
        persistence_case(
            "persistence_malformed_table_name_is_rejected",
            runtime={
                "persistence": {
                    "enabled": True,
                    "backend": "sqlite",
                    "db_path": "a.db",
                    "table": "bad table!",
                }
            },
        )
    )
    cases.append(
        persistence_case(
            "persistence_valid_sqlite_block_passes",
            runtime={"persistence": {"enabled": True, "backend": "sqlite", "db_path": "a.db"}},
        )
    )
    cases.append(
        persistence_case(
            "persistence_jsonl_file_requires_a_path",
            runtime={"persistence": {"enabled": True, "backend": "jsonl-file"}},
        )
    )
    cases.append(
        persistence_case(
            "persistence_mongodb_requires_a_uri",
            runtime={"persistence": {"enabled": True, "backend": "mongodb"}},
        )
    )

    return cases


def render(cases: list[dict[str, Any]], root: str) -> str:
    # `sort_keys=False`: several cases' own dict *insertion order* is part
    # of what they check (`sources`, a dotted-path warning's own key
    # order) -- sorting keys here would silently reorder the very inputs
    # those cases exist to pin.
    text = json.dumps(cases, indent=2, ensure_ascii=False, sort_keys=False) + "\n"
    text = text.replace(root, "<root>")
    for pattern in _LEAKED_PATH_PATTERNS:
        if pattern.search(text):
            raise AssertionError(
                f"generated config corpus leaks a local path (matched {pattern.pattern!r})"
            )
    return text


def main() -> int:
    check = "--check" in sys.argv[1:]
    with tempfile.TemporaryDirectory(prefix="electricity-config-corpus-") as tmp:
        root = Path(tmp).resolve()
        cases = build_cases(root)
        text = render(cases, str(root))

    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    if check:
        current = OUTPUT.read_text(encoding="utf-8") if OUTPUT.exists() else ""
        if current != text:
            print(f"{OUTPUT} is stale; run without --check to regenerate", file=sys.stderr)
            return 1
        return 0
    OUTPUT.write_text(text, encoding="utf-8")
    print(f"wrote {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
