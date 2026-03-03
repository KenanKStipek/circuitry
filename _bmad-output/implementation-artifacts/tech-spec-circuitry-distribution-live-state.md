---
title: 'Circuitry Distribution & Live State Protocol'
slug: 'circuitry-distribution-live-state'
created: '2026-03-02'
status: 'implementation-complete'
stepsCompleted: [1, 2, 3, 4]
tech_stack:
  - python 3.9+
  - pipx (distribution)
  - pyproject.toml (PEP 621, setuptools backend)
  - typer[all] (CLI framework, wraps Click)
  - rich (terminal output, spinners, tables)
  - pyyaml, jsonschema, chevron, python-dotenv, ollama
files_to_modify:
  - pyproject.toml
  - src/circuitry/cli/app.py
  - src/circuitry/cli/config.py
  - src/circuitry/cli/runtime_shim.py
  - src/circuitry/cli/live_state.py (new)
  - src/circuitry/core/dynamic.py
  - src/circuitry/output.py
  - src/circuitry/api.py
  - src/circuitry/__init__.py
  - scripts/install.sh (new)
  - tests/cli/test_config_resolution.py (new)
  - tests/cli/test_live_state.py (new)
  - tests/cli/test_new_commands.py (new)
  - tests/cli/test_pipe_detection.py (new)
  - tests/cli/test_inline_state.py (new)
code_patterns:
  - typer @app.command() with Click options underneath
  - frozen dataclass config (CircuitryConfig)
  - Store with on_write callback — propagated to all child stores but NEVER triggered (effects bypass store.set())
  - Effects mutate state via store.ensure_dict() + direct dict assignment
  - Rich Console global singleton in output.py with THEME
  - RunRequest/RunResult dataclasses in runtime_shim.py
  - _write_state_json() helper for file output
  - console.print_json() for stdout JSON output
  - Spinner suppressed when quiet/json/verbose via nullcontext()
test_patterns:
  - pytest with tmp_path fixture for isolated file creation
  - typer.testing.CliRunner for CLI command tests
  - Direct function calls to runtime shims (validate, run, inspect_orchestration)
  - monkeypatch.setattr for module-level mocking
  - In-memory EchoAdapter for deterministic tests
  - RecordingPlugin/FailingPlugin fixtures in tests/plugin_fixtures.py
  - conftest.py with optional --trace-state Store instrumentation
  - 151 existing tests passing
---

# Tech-Spec: Circuitry Distribution & Live State Protocol

**Created:** 2026-03-02

## Overview

### Problem Statement

Circuitry requires cloning the git repo and manually managing a virtualenv to use. There is no installable package, no global CLI command, and no mechanism for external tools (like Perceptron) to observe orchestration state during execution — state is only written once at completion.

### Solution

1. Package circuitry as a pipx-installable Python package with a `cof` CLI entry point and a curl-based install script.
2. Implement a layered config system: CLI flags > env vars > project-local config > global config > sane defaults.
3. Add developer QoL flags for piping, inline state variables, live state output, and command replay.
4. Wire up incremental atomic state file writes during execution so external tools can file-watch live state.

### Scope

**In Scope:**
- Fix `pyproject.toml` — declare real dependencies, add `[project.scripts] cof = ...` entry point, proper metadata
- Curl install script (`install.sh`) — installs pipx if needed, then `pipx install` from GitHub
- `cof` as the global command name, preserving all existing commands and flags
- Layered config with env var support (`CIRCUITRY_MODEL`, `CIRCUITRY_ADAPTER`, `CIRCUITRY_ADAPTER_URL`)
- Global config at `~/.config/circuitry/config.json`
- Sane defaults when no config exists (adapter: ollama, base_url: localhost:11434, model: llama3.1:8b)
- Incremental state writes — atomic (write-to-tmp + `os.rename`) after each effect completes, via `--live-state <path>`
- New QoL: `-e KEY=VALUE`, `--tail`, `--last`, auto-pipe detection, `cof init`
- `cof gen "<prompt>"` — generate orchestrations from natural language via meta_orchestrator
- `cof check` — intuitive alias for `validate`

**Out of Scope:**
- Built-in SSE/WebSocket server in circuitry
- Diff-based state streaming (full snapshots only for now)
- Perceptron-side changes (it already file-watches)
- PyPI publishing (install from GitHub URL for now)
- Shared library server infrastructure

## Context for Development

### Codebase Patterns

- **CLI framework:** Typer (wraps Click). All commands in `src/circuitry/cli/app.py`. 6 commands: `run`, `validate`, `inspect`, `fetch`, `run-library`, `version`, plus `doctor` (registered dynamically via `register_doctor(app)`).
- **Config:** Frozen dataclass `CircuitryConfig` with fields: `default_model`, `default_adapter`, `plugins: list[str]`, `runtime: dict`. Loaded from JSON via `CircuitryConfig.from_dict()`. Resolution: `--config` flag > `CIRCUITRY_CONFIG` env > CWD filenames (`circuitry.config.json`, `config.json`).
- **State store:** `Store` dataclass wrapping `dict[str, Any]`. Has `on_write: Optional[Callable]` field that is propagated to all child stores (dynamic, loop, conditional, reflector create child stores with `on_write=store.on_write`). **However, `on_write` is never triggered** because effects write via `store.ensure_dict()` + direct dict mutation, not `store.set()`.
- **Output:** Global Rich `Console` singleton in `output.py` with themed styles. CLI output controlled by `--json` (suppress logs, output JSON), `--quiet` (suppress non-essential), `--verbose` (add detail). Spinner via `console.status()`, suppressed when quiet/json/verbose. No explicit TTY detection — Rich handles it internally.
- **Runtime flow:** `app.py:run_cmd()` → `runtime_shim.run(RunRequest)` → `Store(state)` created (line 202, no on_write) → `runtime.execute(store=store)` → state mutated in-place → `RunResult` returned → `--out` writes file, `--print` writes stdout.
- **Scripts wrapper:** `scripts/circuitry` auto-injects `--config <project-root>/config.json` and defaults `--verbose` on `run`. Delegates to `python -m circuitry.cli.app`. This behavior will NOT carry over to `cof` (verbose should be opt-in for installed usage).

### Files to Reference

| File | Purpose |
| ---- | ------- |
| `pyproject.toml` | Package metadata — currently minimal: no deps, no entry point, no metadata |
| `requirements.txt` | 7 runtime deps — needs migrating into pyproject.toml `dependencies` |
| `scripts/circuitry` | Wrapper script — reference for config injection behavior |
| `src/circuitry/cli/app.py` | Typer CLI — all commands, flags, output routing. 375 lines. |
| `src/circuitry/cli/config.py` | `CircuitryConfig` dataclass (lines 13-37), `find_config_path()` (lines 47-67), `load_config()` (lines 70-77) |
| `src/circuitry/cli/runtime_shim.py` | `run()` function, `RunRequest`/`RunResult` dataclasses. Store created at line 202. |
| `src/circuitry/core/store/store.py` | `Store` class (63 lines). `set()` at line 43 triggers `on_write`. `ensure_dict()` at line 26 does not. |
| `src/circuitry/core/dynamic.py` | Effect dispatch loop. Child store created at line 99. Effect completion points after each `.execute()` call. **Primary hook point for live-state writes.** |
| `src/circuitry/core/prompt.py` | Prompt completion: success at line 277, error at line 313, dry-run at line 232. Direct dict mutation. |
| `src/circuitry/core/loop.py` | Loop iteration completion. Child store at line 142, iter store at line 487. Completion at line 321. |
| `src/circuitry/core/tool.py` | Tool completion: success at line 242, error at line 272. Direct dict mutation. |
| `src/circuitry/output.py` | Rich Console singleton + THEME definition |
| `src/circuitry/api.py` | Public Python API: `run_orchestration()`, `validate_orchestration()`, etc. |
| `src/circuitry/__init__.py` | Package exports (6 public names) |
| `src/circuitry/cli/doctor.py` | Doctor command: checks ollama connectivity, model availability, config resolution |
| `orchestrations/meta_orchestrator.yml` | Meta-orchestrator for generating orchestrations — to be bundled as package data |

### Technical Decisions

1. **pipx over pip** — CLI tools should be isolated. pipx creates a dedicated venv per tool and symlinks the binary globally. Standard pattern for Python CLI distribution.
2. **`cof` command name** — short, memorable, no known collisions.
3. **Layered config (5 tiers):** CLI flags > env vars > project-local `circuitry.config.json` > global `~/.config/circuitry/config.json` > sane defaults. Each tier can partially override — they merge, not replace.
4. **Atomic file writes for live state** — write to `.tmp` sibling, then `os.rename()` (POSIX atomic on same filesystem). Prevents partial-read race conditions with Perceptron's file watcher.
5. **`--live-state` as opt-in** — avoids I/O overhead when not needed.
6. **Auto-pipe detection** — `sys.stdout.isatty()` check. When piped, suppress Rich spinners/markup and auto-enable `--json` output for machine-readable piping.
7. **Live-state hook point: effect completion in runtime dispatch, not Store** — since effects bypass `store.set()` and mutate dicts directly, the hook goes in the runtime execution loop (after each effect's `.execute()` returns), not in the Store's `on_write` callback.
8. **No default verbose for `cof`** — the `scripts/circuitry` wrapper defaults `--verbose` on `run` for dev convenience; the installed `cof` command should not. Verbose is opt-in.
9. **Sane defaults** — `adapter: ollama`, `base_url: http://localhost:11434`, `model: llama3.1:8b`. No config file needed for basic local usage with ollama.
10. **`cof gen` wraps meta_orchestrator** — the `gen` command passes the user's prompt as input state to `meta_orchestrator.yml` (bundled with the package), runs it, and outputs the generated YAML. Keeps the meta-orchestrator as an internal implementation detail.
11. **`cof check` aliases `validate`** — shorter, more intuitive. Both names remain available.

### Deferred Concerns (noted for follow-up)

1. **Partial write stress testing** — Atomic rename is POSIX-safe but worth stress-testing with Perceptron under sustained high-frequency writes.
2. **Memory / performance on large orchestrations** — Full-state JSON after every effect could be expensive for very large orchestrations. May need debouncing (`--live-state-interval <ms>`) or diff-based writes later.

### Complete Flag & Command Inventory

#### Existing `run` flags (unchanged)

| Flag | Type | Default |
|------|------|---------|
| `--config` / `-c` | Path | Auto-discovered |
| `--state` / `-s` | Path | None |
| `--out` / `-o` | Path | None |
| `--pretty` | bool | False |
| `--print` | bool | False |
| `--dry-run` | bool | False |
| `--json` | bool | False |
| `--quiet` | bool | False |
| `--verbose` / `-v` | bool | False |

#### New `run` flags

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--live-state` | Path | None | Atomic state write after each effect completes. Perceptron file-watches this path. |
| `-e KEY=VALUE` | Repeatable | [] | Inline state variables merged into initial state. Avoids needing a JSON file. |
| `--tail` | bool | False | Print only the final effect's `.value` as plain text after completion. Ideal for piping. |
| `--last` | bool | False | Re-run the most recent orchestration with same args. Stashed in `~/.config/circuitry/last-run.json`. |
| Auto-pipe | Implicit | — | When `!sys.stdout.isatty()`: suppress Rich, auto-enable `--json` behavior. |

#### Full command surface

| Command | Description |
|---------|-------------|
| `cof run <path>` | Execute an orchestration. Primary command. All existing + new flags apply. |
| `cof gen "<prompt>"` | Generate an orchestration from a natural language prompt. Wraps `meta_orchestrator.yml` under the hood. |
| `cof check <path>` | Validate orchestration YAML against schema. Alias for `validate`. |
| `cof init` | Scaffold `circuitry.config.json` + example orchestration in CWD. Interactive prompts for adapter URL, model. |
| `cof inspect <path>` | Show orchestration metadata (model, adapter, effects). Existing command. |
| `cof doctor` | Diagnose setup: config resolution, adapter connectivity, model availability. Existing command. |
| `cof version` | Print version. Existing command. |
| `cof -h` / `cof --help` | List all commands with descriptions. Typer provides this, but help text must be polished. |
| `cof <cmd> -h` | Per-command help with flag descriptions and usage examples. |

**Help text requirements:** Every command and flag must have a clear `help=` string in its Typer definition. Top-level app help should show a one-liner per command. `cof run -h` should include a usage example section (via `rich_help_panel` or epilog).

#### New env vars

| Env Var | Overrides | Priority |
|---------|-----------|----------|
| `CIRCUITRY_CONFIG` | Config file path | Already exists |
| `CIRCUITRY_MODEL` | `default_model` | Above config file, below CLI flag |
| `CIRCUITRY_ADAPTER` | `default_adapter` | Same |
| `CIRCUITRY_ADAPTER_URL` | `runtime.adapters.<adapter>.base_url` | Same |

#### Config resolution order

```
CLI flags (highest)
  ↓
Environment variables (CIRCUITRY_MODEL, CIRCUITRY_ADAPTER, CIRCUITRY_ADAPTER_URL)
  ↓
Project-local: ./circuitry.config.json
  ↓
Global: ~/.config/circuitry/config.json
  ↓
Sane defaults (lowest): adapter=ollama, base_url=http://localhost:11434, model=llama3.1:8b
```

## Implementation Plan

### Tasks

- [x] Task 1: Update `pyproject.toml` with full package metadata
  - File: `pyproject.toml`
  - Action: Add `[project]` metadata (description, readme, license, requires-python, classifiers). Move all 7 runtime dependencies from `requirements.txt` into `[project] dependencies`. Add `[project.scripts] cof = "circuitry.cli.app:main"`. Add `[tool.setuptools.package-data]` to bundle `orchestrations/meta_orchestrator.yml` and `docs/orchestration-reference.md` as package data.
  - Notes: Keep `requirements.txt` for dev convenience but pyproject.toml is the source of truth. Entry point calls existing `main()` in app.py.

- [x] Task 2: Extend config resolution with env vars, global config, and sane defaults
  - File: `src/circuitry/cli/config.py`
  - Action: (a) Add `SANE_DEFAULTS` dict: `{"default_model": "llama3.1:8b", "default_adapter": "ollama", "runtime": {"adapters": {"ollama": {"base_url": "http://localhost:11434"}}}}`. (b) Extend `find_config_path()` to also check `~/.config/circuitry/config.json` after CWD and before returning None. (c) Add new function `resolve_config(*, explicit_path, cwd) -> CircuitryConfig` that: loads sane defaults → deep-merges global config (if exists) → deep-merges project config (if exists) → overlays env vars (`CIRCUITRY_MODEL` → `default_model`, `CIRCUITRY_ADAPTER` → `default_adapter`, `CIRCUITRY_ADAPTER_URL` → `runtime.adapters.<adapter>.base_url`). (d) Add a `_deep_merge(base: dict, overlay: dict) -> dict` helper — overlay keys win, nested dicts recurse. (e) Update `CircuitryConfig.from_dict()` to accept the merged dict.
  - Notes: `resolve_config()` replaces direct `load_config()` calls in app.py. The explicit `--config` flag still takes priority by being the only file loaded (skips global/CWD discovery). Env vars always overlay on top.

- [x] Task 3: Update CLI commands to use new config resolution
  - File: `src/circuitry/cli/app.py`
  - Action: (a) Replace all `cfg_path = find_config_path(...); cfg = load_config(cfg_path)` call sites with `cfg = resolve_config(explicit_path=config, cwd=None)`. Affects `run_cmd` (line 73-74), `fetch_cmd`, `run_library_cmd`. (b) Update `_print_header()` to show resolved config sources (which tier each setting came from) when `--verbose`.
  - Notes: Existing `find_config_path` and `load_config` remain available for backward compatibility but are no longer the primary path.

- [x] Task 4: Add auto-pipe detection
  - File: `src/circuitry/cli/app.py`
  - Action: At the top of `run_cmd()` (and `run_library_cmd`), add: `if not sys.stdout.isatty(): json_out = True; quiet = True`. This auto-enables machine-readable output when stdout is piped. User can still override with explicit `--no-json` if needed (though this is unlikely).
  - Notes: Rich Console already suppresses ANSI when not a TTY, but we need to also suppress the header/status/warning prints that are gated on `quiet` and `json_out` flags. This makes `cof run gen.yml | jq .` work seamlessly.

- [x] Task 5: Add `-e KEY=VALUE` inline state flag
  - File: `src/circuitry/cli/app.py`
  - Action: (a) Add new parameter to `run_cmd`: `env_vars: Optional[list[str]] = typer.Option(None, "-e", help="Inline state variable (KEY=VALUE). Repeatable.")`. (b) After loading `--state` file (or starting with empty dict), parse each `-e` entry: split on first `=`, set `state[key] = value`. Values are strings; if the value looks like JSON (`{`, `[`, `true`, `false`, digit), parse it. (c) `-e` values override `--state` file values (last writer wins). (d) Apply same parameter to `run_library_cmd`.
  - Notes: Enables `cof run gen.yml -e topic=cats -e tone=casual` without needing a JSON file. JSON-detection for values keeps it simple — strings by default, structured when obvious.

- [x] Task 6: Create atomic live-state writer module
  - File: `src/circuitry/cli/live_state.py` (new)
  - Action: Create module with: (a) `write_live_state(path: Path, state: dict) -> None` — serializes state to JSON, writes to `path.with_suffix('.tmp')`, then `os.rename()` to `path`. (b) `make_live_state_callback(path: Path) -> Callable[[dict], None]` — returns a closure that calls `write_live_state`. (c) Import `json`, `os`, `Path` only.
  - Notes: Kept as a separate module for testability. The atomic rename pattern (write tmp, rename) is POSIX-safe on the same filesystem. The callback signature `(dict) -> None` matches what the runtime hook will call with.

- [x] Task 7: Wire live-state hook into runtime execution
  - File: `src/circuitry/cli/runtime_shim.py`
  - Action: (a) Add `live_state_path: Optional[Path] = None` field to `RunRequest` dataclass. (b) In `run()`, after creating `Store(state)` at line 202: if `req.live_state_path` is set, create callback via `make_live_state_callback(req.live_state_path)` and assign to `store.on_write`. (c) Write an initial state snapshot before execution begins (so Perceptron sees the "pending" state).
  - File: `src/circuitry/core/dynamic.py`
  - Action: After each effect's `.execute()` call returns (in the chain dispatch loop and after tree parallel completion), call `if store.on_write: store.on_write(store.state)`. This is the effect-completion hook point. Add the call after the execute in the chain loop (around line 171) and after the ThreadPoolExecutor results are assembled in tree mode.
  - Notes: This approach fires `on_write` at the effect-dispatch level rather than inside each effect's internals — fewer touch points, consistent behavior. The callback receives the entire root state dict (Store propagates the root reference through child stores). Since child stores hold references into the same root dict, `store.state` at any nesting level reflects the full current state.

- [x] Task 8: Add `--live-state` flag to CLI
  - File: `src/circuitry/cli/app.py`
  - Action: (a) Add `live_state: Optional[Path] = typer.Option(None, "--live-state", help="Write state atomically to this file after each effect completes. For live monitoring tools.")` to `run_cmd` and `run_library_cmd`. (b) Pass through to `RunRequest(live_state_path=live_state)`. (c) If `--live-state` is set, print the path in the header section.
  - Notes: Perceptron's backend sets `STATE_FILE_PATH` env var pointing at this file. User runs: `cof run orch.yml --live-state ./state.json` and in another terminal starts Perceptron with `STATE_FILE_PATH=./state.json`.

- [x] Task 9: Add `--tail` flag
  - File: `src/circuitry/cli/app.py`
  - Action: (a) Add `tail: bool = typer.Option(False, "--tail", help="Print only the final effect's value as plain text. Ideal for piping.")` to `run_cmd` and `run_library_cmd`. (b) After successful run, if `--tail` is set: walk `result.state["prime"]` to find the last effect (deepest completed effect in chain order), extract its `"value"`, and print it as plain text via `print()` (not `console.print()` — no markup). (c) `--tail` is mutually exclusive with `--print` and `--json`. If both are set, error with a clear message.
  - Notes: Enables `cof run gen.yml -e topic=cats --tail | pbcopy`. The "last effect" is the final effect in the root dynamic's chain order whose `value` is set.

- [x] Task 10: Add `--last` flag with run stash
  - File: `src/circuitry/cli/app.py`
  - Action: (a) Add `last: bool = typer.Option(False, "--last", help="Re-run the most recent orchestration with the same arguments.")` to `run_cmd`. (b) Define stash path: `~/.config/circuitry/last-run.json`. (c) On every successful `run_cmd` invocation, write the resolved args (orchestration path, config path, state path, all flags, `-e` values) to the stash file as JSON. (d) When `--last` is set: load the stash, reconstruct the args, and execute. The `orchestration` argument becomes optional when `--last` is used. (e) If stash doesn't exist, error with "No previous run found."
  - Notes: Stash is a simple JSON file. Only stores the most recent run. Does not store `--last` itself (prevents infinite recursion).

- [x] Task 11: Add `cof check` command (alias for validate)
  - File: `src/circuitry/cli/app.py`
  - Action: (a) Add `@app.command("check")` that accepts the same `orchestration: Path` argument and `--json` flag as `validate_cmd`. (b) Implementation: call the existing `validate_cmd` function body directly (extract shared logic into a helper `_do_validate(orchestration, json_out)` called by both `validate_cmd` and `check_cmd`).
  - Notes: Both `cof validate` and `cof check` remain available. Same behavior, different name.

- [x] Task 12: Add `cof gen` command
  - File: `src/circuitry/cli/app.py`
  - Action: (a) Add `@app.command("gen")` with parameter `prompt: str = typer.Argument(help="Natural language description of the orchestration to generate.")` and optional `--out/-o` path, `--config/-c`, `--verbose/-v`. (b) Implementation: resolve config, locate bundled `meta_orchestrator.yml` via `importlib.resources` (or `pkg_resources`), call `run(RunRequest(orchestration=meta_orch_path, state={"prompt": prompt}, config=cfg))`, extract the generated YAML from the final effect's value. (c) If `--out` is set, write YAML to file. Otherwise print to stdout. (d) On error, print the error and exit 1.
  - File: `pyproject.toml` (already handled in Task 1)
  - Action: Ensure `orchestrations/meta_orchestrator.yml` is included as package data.
  - Notes: The meta_orchestrator expects a `prompt` input key. The generated YAML is in `prime.generate.final_yaml.value`. `cof gen "a 3-step summarizer pipeline" --out summarizer.yml` is the intended UX.

- [x] Task 13: Add `cof init` command
  - File: `src/circuitry/cli/app.py`
  - Action: (a) Add `@app.command("init")` with no required args. (b) Implementation: check if `circuitry.config.json` already exists in CWD — if so, warn and exit. (c) Prompt user interactively (via `typer.prompt()`) for: adapter (default: ollama), adapter URL (default: http://localhost:11434), model (default: llama3.1:8b). (d) Write `circuitry.config.json` with the provided values. (e) Write an example orchestration file `hello.yml` with a single prompt effect that uses `{{name}}` as input. (f) Print: "Created circuitry.config.json and hello.yml. Try: cof run hello.yml -e name=World"
  - Notes: Minimal scaffolding. Gets the user from zero to a working `cof run` in one command.

- [x] Task 14: Polish all help text
  - File: `src/circuitry/cli/app.py`
  - Action: (a) Set `app = typer.Typer(add_completion=False, help="Circuitry — YAML-based LLM orchestration framework.", rich_markup_mode="rich")`. (b) Add `help=` strings to every `@app.command()` decorator: `run` → "Execute an orchestration", `gen` → "Generate an orchestration from a prompt", `check` → "Validate orchestration YAML", `init` → "Initialize a new circuitry project", `inspect` → "Show orchestration metadata", `doctor` → "Diagnose environment setup", `version` → "Print version". (c) Add `help=` strings to every `typer.Option` and `typer.Argument` that doesn't already have one. (d) Add epilog/usage examples to `run_cmd` via the `@app.command(epilog=...)` parameter showing: `cof run ./my-orch.yml`, `cof run ./my-orch.yml -e topic=cats --tail`, `cof run ./my-orch.yml --live-state ./state.json`.
  - Notes: Typer renders help text via Rich when `rich_markup_mode="rich"` is set.

- [x] Task 15: Update public Python API with live_state support
  - File: `src/circuitry/api.py`
  - Action: (a) Add `live_state_path: str | Path | None = None` parameter to `run_orchestration()` and `run_shared_orchestration()`. (b) Pass through to `RunRequest(live_state_path=...)`. (c) Update docstrings.
  - Notes: Allows embedded users to enable live-state writes programmatically, not just via CLI.

- [x] Task 16: Create install script
  - File: `scripts/install.sh` (new)
  - Action: Create a POSIX-compatible shell script that: (a) Checks for Python 3.9+ (`python3 --version`). (b) Checks for pipx; if missing, installs via `python3 -m pip install --user pipx && python3 -m pipx ensurepath`. (c) Runs `pipx install git+https://github.com/<owner>/circuitry.git`. (d) Verifies installation with `cof version`. (e) Prints success message with next steps: "Run `cof init` to get started or `cof doctor` to check your setup." (f) Handles errors at each step with clear messages and exit codes.
  - Notes: Invoked via `curl -fsSL https://raw.githubusercontent.com/<owner>/circuitry/main/scripts/install.sh | sh`. Script must be idempotent (re-running upgrades cleanly via `pipx upgrade`). No sudo required.

- [x] Task 17: Write tests for new functionality
  - File: `tests/cli/test_config_resolution.py` (new)
  - Action: Test the layered config resolution: (a) sane defaults when no config exists, (b) global config at `~/.config/circuitry/config.json` is loaded, (c) project-local config overrides global, (d) env vars (`CIRCUITRY_MODEL`, `CIRCUITRY_ADAPTER`, `CIRCUITRY_ADAPTER_URL`) override config files, (e) explicit `--config` skips discovery and uses only that file + env vars, (f) `_deep_merge` correctly handles nested dicts.
  - File: `tests/cli/test_live_state.py` (new)
  - Action: Test the live-state writer: (a) `write_live_state` writes valid JSON and atomically renames, (b) the `.tmp` file does not persist after rename, (c) callback created by `make_live_state_callback` produces valid state files, (d) concurrent calls don't corrupt output.
  - File: `tests/cli/test_new_commands.py` (new)
  - Action: Test new commands via CliRunner: (a) `cof check` produces same output as `cof validate`, (b) `cof init` creates config and example files, (c) `cof init` warns if config already exists, (d) `cof version` still works.
  - File: `tests/cli/test_pipe_detection.py` (new)
  - Action: Test auto-pipe detection: (a) mock `sys.stdout.isatty()` to return False, verify `--json` behavior is auto-enabled.
  - File: `tests/cli/test_inline_state.py` (new)
  - Action: Test `-e` flag: (a) single `-e key=value` sets state, (b) multiple `-e` flags accumulate, (c) `-e` overrides `--state` file values, (d) JSON-like values are parsed (e.g., `-e count=3` → int, `-e items=[1,2]` → list).
  - Notes: Use existing test patterns — `tmp_path`, `CliRunner`, `monkeypatch`. Mock adapter calls where needed.

- [x] Task 18: Verify all existing tests still pass
  - Action: Run `python -m pytest tests/ -v` and confirm all 151 existing tests pass with no regressions. Fix any breakage caused by config resolution changes (most likely in tests that import `load_config` or `find_config_path` directly).
  - Notes: The config resolution change (Task 2-3) is the highest regression risk since it changes the default behavior when no config file is found (previously returned empty `CircuitryConfig`, now returns defaults-populated one).

### Acceptance Criteria

- [x] AC 1: Given a machine with Python 3.9+ and no circuitry installed, when `curl -fsSL .../install.sh | sh` is run, then `cof version` prints the version and exits 0.
- [x] AC 2: Given no config file exists anywhere, when `cof run hello.yml -e name=World` is run, then circuitry uses sane defaults (adapter: ollama, model: llama3.1:8b, base_url: http://localhost:11434).
- [x] AC 3: Given `CIRCUITRY_MODEL=mistral:7b` is set, when `cof run orch.yml` is run, then the effective model is `mistral:7b` (overriding config file and defaults).
- [x] AC 4: Given `~/.config/circuitry/config.json` has `default_model: "phi3"` and project-local `circuitry.config.json` has `default_model: "llama3.1:8b"`, when `cof run orch.yml` is run, then the effective model is `llama3.1:8b` (project-local wins).
- [x] AC 5: Given an orchestration with 3 prompt effects, when `cof run orch.yml --live-state ./state.json` is run, then `./state.json` is updated atomically after each effect completes (4 writes total: initial + 3 effects), and each write is valid parseable JSON.
- [x] AC 6: Given `cof run gen.yml -e topic=cats --tail` is run, when the orchestration completes, then only the final effect's value is printed to stdout as plain text (no JSON wrapping, no Rich markup).
- [x] AC 7: Given `cof run orch.yml` was previously run successfully, when `cof run --last` is run, then the same orchestration is re-executed with the same arguments.
- [x] AC 8: Given an orchestration file, when `cof check orch.yml` is run, then output is identical to `cof validate orch.yml`.
- [x] AC 9: Given an empty directory, when `cof init` is run, then `circuitry.config.json` and `hello.yml` are created, and `cof run hello.yml -e name=World` succeeds.
- [x] AC 10: Given `cof run orch.yml | jq .` is run (stdout is a pipe), then output is valid JSON with no Rich markup or spinner characters.
- [x] AC 11: Given a valid prompt, when `cof gen "a 3-step summarizer pipeline"` is run, then valid orchestration YAML is printed to stdout.
- [x] AC 12: Given any command, when `cof <cmd> -h` is run, then helpful usage text with flag descriptions is displayed.
- [x] AC 13: Given all changes are complete, when `python -m pytest tests/ -v` is run, then all existing 151 tests plus new tests pass with zero failures.

## Additional Context

### Dependencies

**Runtime (to declare in pyproject.toml):**
- `typer[all]>=0.12`
- `rich>=13.7`
- `pyyaml>=6.0`
- `jsonschema>=4.0`
- `chevron>=0.14`
- `python-dotenv>=1.0`
- `ollama>=0.1.8`

**Install script requires:**
- Python 3.9+
- pipx (installed automatically if missing)

**Package data to bundle:**
- `orchestrations/meta_orchestrator.yml`
- `docs/orchestration-reference.md` (for `{{rules}}` injection)

### Testing Strategy

**Unit tests (new files):**
- `tests/cli/test_config_resolution.py` — layered config: defaults, global, project, env vars, explicit path, deep merge
- `tests/cli/test_live_state.py` — atomic writer, tmp+rename, callback, valid JSON
- `tests/cli/test_new_commands.py` — `check`, `init`, `gen` via CliRunner
- `tests/cli/test_pipe_detection.py` — isatty mock, auto-json behavior
- `tests/cli/test_inline_state.py` — `-e` parsing, override behavior, JSON value detection

**Integration tests (manual):**
- Install from GitHub via `scripts/install.sh` on a clean machine
- Run `cof init` → `cof run hello.yml -e name=World` end-to-end
- Run `cof run orch.yml --live-state ./state.json` while Perceptron file-watches
- Pipe test: `cof run orch.yml --tail | wc -c` produces expected byte count
- `cof gen "summarizer pipeline" --out test.yml && cof check test.yml` round-trips cleanly

**Regression:**
- All 151 existing tests must continue to pass

### Notes

- Perceptron currently file-watches `STATE_FILE_PATH` env var, retries 3x at 150ms, broadcasts via SSE. No changes needed on perceptron side — `--live-state` writes to whatever path perceptron is configured to watch.
- The existing `scripts/circuitry` wrapper is preserved for in-repo dev use but superseded by `cof` for installed usage.
- `store.on_write` callback infrastructure exists but is never triggered because effects mutate state dicts directly. The live-state feature hooks into effect completion in the runtime dispatch loop, not the store.
- Doctor command already works well for post-install diagnostics — checks ollama, model availability, config resolution.
- The `run` command's `_write_state_json()` helper (app.py lines 33-41) can be extracted and reused for the atomic live-state writer.
- `cof gen` depends on the meta_orchestrator being bundled as package data. If the meta_orchestrator evolves, the bundled version auto-updates on `pipx upgrade`.
