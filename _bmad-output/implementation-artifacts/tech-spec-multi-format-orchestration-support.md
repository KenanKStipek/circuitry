---
title: 'Multi-Format Orchestration Support (JSON, TOON)'
slug: 'multi-format-orchestration-support'
created: '2026-03-02'
status: 'ready-for-dev'
stepsCompleted: [1, 2, 3, 4]
tech_stack:
  - Python >=3.9
  - pytest, typer, rich
  - pyyaml, jsonschema
  - toon-python (git dependency, v0.9.x beta)
files_to_modify:
  - src/circuitry/cli/orchestration_loader.py
  - src/circuitry/cli/runtime_shim.py
  - src/circuitry/cli/app.py
  - src/circuitry/cli/doctor.py
  - src/circuitry/cli/shared_library.py
  - pyproject.toml
  - tests/cli/test_orchestration_loader_formats.py (new)
code_patterns:
  - 'load_orchestration_file() is the single gateway — returns dict[str, Any]'
  - 'All downstream code (compiler, runtime, schema) works on plain dicts'
  - 'CLI help text references "YAML" in multiple places'
  - 'gen command LLM output is YAML text — re-serialization needed for other formats'
  - 'shared_library sidecar uses .json suffix — conflicts with .json orchestrations'
test_patterns:
  - '_write(tmp_path, name, content) helper for creating temp files'
  - 'validate() returns {"ok": bool, "errors": [str]}'
  - 'pytest parametrize for multi-format coverage'
  - 'CliRunner from typer.testing for CLI tests'
---

# Tech-Spec: Multi-Format Orchestration Support (JSON, TOON)

**Created:** 2026-03-02

## Overview

### Problem Statement

Circuitry orchestrations are locked to YAML (`.yml`/`.yaml`). Users authoring orchestrations for LLM workflows may prefer JSON (familiar, tooling-rich) or TOON (token-efficient, purpose-built for LLM contexts). The framework should be format-agnostic at the file boundary.

### Solution

Extend the orchestration file loading, writing, discovery, and CLI to support `.json` and `.toon` alongside `.yml`/`.yaml`, keeping YAML as the default output format. The internal pipeline remains `dict[str, Any]` — only the serialization/deserialization boundary changes.

### Scope

**In Scope:**
- Parsing `.json` and `.toon` files in `orchestration_loader.py`
- `gen` command `--format` flag for output in yaml/json/toon
- Shared library discovery for all three formats
- `inspect` and `validate` support for all formats
- Format-aware error messages (e.g., "Invalid TOON orchestration")
- New dependency: `toon-python` from GitHub
- Tests for all new parsing and serialization paths

**Out of Scope:**
- Converting existing orchestration files between formats
- TOML support
- Changing the internal `dict[str, Any]` pipeline
- LLM-generated YAML parsing in `reflector.py`

## Context for Development

### Codebase Patterns

- `load_orchestration_file()` in `orchestration_loader.py` is the single gateway for all orchestration file reads — checks extension, parses, returns `dict[str, Any]`
- All downstream code (compiler, runtime, schema validation) works on plain dicts — already format-agnostic
- Shared library discovery in `shared_library.py` globs for `.yml`/`.yaml` — needs extension set expanded
- `gen` command in `app.py` writes YAML output — needs format selection via `--format` flag
- `gen` command LLM output is always YAML text — for JSON/TOON output, parse YAML → dict → re-serialize to target format
- `_load_metadata_sidecar()` in `shared_library.py:211` uses `.json` suffix for metadata sidecars — conflicts when orchestration is `.json`; skip sidecar when orchestration extension is `.json`
- CLI help text strings reference "YAML" in `run`, `validate`, `check`, `inspect`, `doctor`, `fetch` commands
- `validate()` error messages are format-agnostic (come from jsonschema and compiler) except `orchestration_loader.py:16` which says "Orchestration YAML must be a mapping"
- Tests use `_write(tmp_path, name, content)` helper, `tmp_path` fixture, `CliRunner`, `pytest.mark.parametrize`

### Files to Reference

| File | Purpose |
| ---- | ------- |
| `src/circuitry/cli/orchestration_loader.py` | Central load function (18 lines) — primary change target |
| `src/circuitry/cli/runtime_shim.py:299-344` | `validate()` and `inspect_orchestration()` — suffix gate at line 328 |
| `src/circuitry/cli/app.py:607-682` | `gen` command — output serialization and `--format` flag target |
| `src/circuitry/cli/app.py:151-159` | `run` command — help text says "orchestration YAML file" |
| `src/circuitry/cli/app.py:557-604` | `validate`, `check`, `inspect` commands — help text says "YAML" |
| `src/circuitry/cli/app.py:340-389` | `fetch` command — help text says "orchestration YAML" |
| `src/circuitry/cli/doctor.py:27-30` | `--orch` option — help text says "orchestration YAML" |
| `src/circuitry/cli/shared_library.py:173-199` | `_resolve_asset_version()` — suffix filter at line 178 |
| `src/circuitry/cli/shared_library.py:210-218` | `_load_metadata_sidecar()` — `.json` sidecar collision |
| `pyproject.toml` | Dependencies list — add `toon-python` |
| `tests/cli/test_validate.py` | Existing validation tests — pattern to follow |
| `tests/cli/test_run_and_inspect.py` | Existing inspect tests — pattern to follow |
| `tests/cli/test_shared_library.py` | Existing shared library tests — pattern to follow |

### Technical Decisions

- **TOON library**: `toon-python` from GitHub (`pip install git+https://github.com/toon-format/toon-python.git`) — community-driven, v0.9.x beta, Python 3.8+
- **JSON**: stdlib `json` module — no new dependency
- **Default output format**: YAML (unchanged)
- **Format detection**: by file extension (`.json`, `.toon`, `.yml`/`.yaml`)
- **Supported extensions constant**: define `ORCHESTRATION_SUFFIXES = {".yml", ".yaml", ".json", ".toon"}` once, reuse everywhere
- **Metadata sidecar for `.json` orchestrations**: skip sidecar loading when orchestration is `.json` (sidecar would be the same file)
- **`gen --format`**: parse LLM-generated YAML → dict → re-serialize to target format; default `yaml`
- **Format-aware error messages**: `"Orchestration {FORMAT} must be a mapping/object at the root."` where FORMAT is derived from extension

## Implementation Plan

### Tasks

- [ ] Task 1: Add `toon-python` dependency to `pyproject.toml`
  - File: `pyproject.toml`
  - Action: Add `"toon-python @ git+https://github.com/toon-format/toon-python.git"` to the `dependencies` list.

- [ ] Task 2: Define shared format constants and extend `load_orchestration_file()`
  - File: `src/circuitry/cli/orchestration_loader.py`
  - Action:
    1. Add `import json` and a try/except import for `toon_python` (with `decode` function).
    2. Define `ORCHESTRATION_SUFFIXES = {".yml", ".yaml", ".json", ".toon"}` as a module-level constant.
    3. Define `FORMAT_LABELS = {".yml": "YAML", ".yaml": "YAML", ".json": "JSON", ".toon": "TOON"}` for error messages.
    4. Expand `load_orchestration_file()` to branch on suffix:
       - `.yml`/`.yaml` → existing `yaml.safe_load()` path
       - `.json` → `json.loads()`
       - `.toon` → `toon_python.decode()`
    5. Update the root-type error message to use format label: `f"Orchestration {label} must be a mapping/object at the root."`
    6. Update the unsupported format error to list all supported extensions.
  - Notes: `toon-python` import should be guarded with try/except like `jsonschema` — raise a clear error at parse time if the library is missing and a `.toon` file is loaded.

- [ ] Task 3: Add serialization helpers for output formats
  - File: `src/circuitry/cli/orchestration_loader.py`
  - Action: Add a `serialize_orchestration(data: dict[str, Any], fmt: str) -> str` function that serializes a dict to the given format string (`"yaml"`, `"json"`, `"toon"`).
    - `"yaml"` → `yaml.dump(data, default_flow_style=False, sort_keys=False)`
    - `"json"` → `json.dumps(data, indent=2, ensure_ascii=False)`
    - `"toon"` → `toon_python.encode(data)`
  - Notes: Used by the `gen` command `--format` flag. Guard `toon_python` import the same way as in Task 2.

- [ ] Task 4: Expand suffix gate in `inspect_orchestration()`
  - File: `src/circuitry/cli/runtime_shim.py`
  - Action:
    1. Import `ORCHESTRATION_SUFFIXES` from `orchestration_loader`.
    2. Replace `if suffix in {".yml", ".yaml"}:` on line 328 with `if suffix in ORCHESTRATION_SUFFIXES:`.
    3. Update the `else` branch message from `"Non-YAML inspection is currently shallow."` to `f"Unsupported format for deep inspection: {suffix}"`.

- [ ] Task 5: Add `--format` flag to `gen` command
  - File: `src/circuitry/cli/app.py`
  - Action:
    1. Import `serialize_orchestration` from `orchestration_loader`.
    2. Add a `--format` option to `gen_cmd` with choices `yaml`, `json`, `toon` and default `yaml`.
    3. After extracting `yaml_text = str(generated)`:
       - If format is `"yaml"`, write `yaml_text` as-is (current behavior).
       - If format is `"json"` or `"toon"`, parse `yaml_text` via `yaml.safe_load()` into a dict, then call `serialize_orchestration(data, fmt)` to get the output string.
    4. When writing to `--out`, use the output string (not raw `yaml_text`).
    5. When printing to stdout (no `--out`), print the output string.

- [ ] Task 6: Update CLI help text across all commands
  - File: `src/circuitry/cli/app.py`
  - Action: Replace "YAML" references with format-agnostic wording in help strings:
    - `run_cmd` orchestration argument help: `"Path to orchestration file."` (was `"Path to orchestration YAML file."`)
    - `validate_cmd` help: `"Validate an orchestration file against the schema."` (was `"Validate orchestration YAML against schema."`)
    - `validate_cmd` orchestration argument help: `"Path to orchestration file."` (was `"Path to orchestration YAML file."`)
    - `check_cmd` help: `"Validate an orchestration file against the schema."` (same change)
    - `check_cmd` orchestration argument help: `"Path to orchestration file."`
    - `inspect_cmd` orchestration argument help: `"Path to orchestration file."`
    - `fetch_cmd` `--out` help: `"Output path for fetched orchestration."` (was `"Output path for fetched orchestration YAML."`)
    - `gen_cmd` `--out` help: `"Write generated orchestration to this file."` (was `"Write generated YAML to this file."`)
  - File: `src/circuitry/cli/doctor.py`
  - Action: Update `--orch` help: `"Optional orchestration file to include in effective settings."` (was `"Optional orchestration YAML to include in effective settings."`)

- [ ] Task 7: Expand shared library file discovery
  - File: `src/circuitry/cli/shared_library.py`
  - Action:
    1. Import `ORCHESTRATION_SUFFIXES` from `orchestration_loader`.
    2. Replace the hardcoded set `{".yml", ".yaml"}` on line 178 with `ORCHESTRATION_SUFFIXES`.

- [ ] Task 8: Fix metadata sidecar collision for `.json` orchestrations
  - File: `src/circuitry/cli/shared_library.py`
  - Action: In `_load_metadata_sidecar()`, add a guard: if `orchestration_file.suffix.lower() == ".json"`, return `{}` immediately (the sidecar would be the orchestration file itself).

- [ ] Task 9: Add unit tests for multi-format loading
  - File: `tests/cli/test_orchestration_loader_formats.py` (new)
  - Action: Create test file with:
    1. `_write(tmp_path, name, content)` helper (follow existing pattern).
    2. Tests for loading valid `.json` orchestration files.
    3. Tests for loading valid `.toon` orchestration files.
    4. Tests for format-aware error messages (non-dict root in JSON, TOON).
    5. Tests for unsupported extension rejection (e.g., `.xml`).
    6. Test that `ORCHESTRATION_SUFFIXES` contains all expected extensions.
    7. Tests for `serialize_orchestration()` round-trip: serialize to each format, parse back, assert equal dicts.
    8. Parametrized test across `.yml`, `.json`, `.toon` for valid orchestration loading.

- [ ] Task 10: Add tests for inspect and validate across formats
  - File: `tests/cli/test_orchestration_loader_formats.py` (same new file)
  - Action:
    1. Test `validate()` with a valid `.json` orchestration.
    2. Test `validate()` with a valid `.toon` orchestration.
    3. Test `inspect_orchestration()` with `.json` and `.toon` files — verify effects are extracted.
    4. Test `validate()` with malformed JSON/TOON content — verify error messages reference the correct format.

- [ ] Task 11: Add tests for shared library format discovery and sidecar collision
  - File: `tests/cli/test_shared_library.py` (extend existing)
  - Action:
    1. Add a test that creates a `.json` orchestration asset version and verifies `fetch_shared_orchestration()` discovers and loads it.
    2. Add a test that creates a `.toon` orchestration asset version and verifies discovery.
    3. Add a test that a `.json` orchestration does not load itself as metadata sidecar (returns `{}`).

- [ ] Task 12: Add tests for `gen --format` flag
  - File: `tests/cli/test_gen_command.py` (extend existing)
  - Action:
    1. Test `gen --format json` produces valid JSON output.
    2. Test `gen --format toon` produces valid TOON output.
    3. Test `gen --format yaml` produces YAML output (default behavior).
    4. Use mocked `run()` to return a known YAML state value, then verify serialization.

### Acceptance Criteria

- [ ] AC 1: Given a valid orchestration file with `.json` extension, when `cof run orch.json` is executed, then the orchestration loads and executes identically to the equivalent `.yml` file.
- [ ] AC 2: Given a valid orchestration file with `.toon` extension, when `cof run orch.toon` is executed, then the orchestration loads and executes identically to the equivalent `.yml` file.
- [ ] AC 3: Given a `.json` file containing a JSON array (not an object) at root, when loaded, then the error message says `"Orchestration JSON must be a mapping/object at the root."`.
- [ ] AC 4: Given a `.toon` file containing invalid TOON syntax, when loaded, then a clear parse error is raised referencing the TOON format.
- [ ] AC 5: Given a file with an unsupported extension (e.g., `.xml`), when loaded, then the error message lists all supported extensions (`.yml`, `.yaml`, `.json`, `.toon`).
- [ ] AC 6: Given `toon-python` is not installed, when a `.toon` file is loaded, then a clear error is raised indicating the missing dependency.
- [ ] AC 7: Given `cof validate orch.json` is run on a valid JSON orchestration, when validation completes, then `{"ok": true, "errors": []}` is returned.
- [ ] AC 8: Given `cof inspect orch.toon` is run, when inspection completes, then effects count and names are extracted correctly.
- [ ] AC 9: Given a shared library directory containing `1.0.json` and `2.0.toon` asset files, when `fetch_shared_orchestration()` is called without a version, then the latest version is resolved regardless of format.
- [ ] AC 10: Given a shared library asset `1.0.json`, when metadata sidecar loading runs, then the sidecar is skipped (returns `{}`) to avoid loading the orchestration as its own metadata.
- [ ] AC 11: Given `cof gen "describe a pipeline" --format json`, when generation completes, then the output is valid JSON representing the orchestration.
- [ ] AC 12: Given `cof gen "describe a pipeline" --format toon`, when generation completes, then the output is valid TOON representing the orchestration.
- [ ] AC 13: Given `cof gen "describe a pipeline"` (no `--format` flag), when generation completes, then the output is YAML (default behavior, unchanged).
- [ ] AC 14: Given all CLI help text for `run`, `validate`, `check`, `inspect`, `fetch`, `gen`, and `doctor --orch`, when `--help` is shown, then no help string mentions "YAML" as the only format — wording is format-agnostic.

## Additional Context

### Dependencies

- `toon-python` — `pip install git+https://github.com/toon-format/toon-python.git` (add to `pyproject.toml` dependencies)

### Testing Strategy

- **New test file**: `tests/cli/test_orchestration_loader_formats.py` — unit tests for loading `.json` and `.toon` files
- **Parametrize across formats**: use `@pytest.mark.parametrize` with `.yml`, `.json`, `.toon` for shared validation/inspect paths
- **Error case tests**: unsupported extensions, malformed JSON, malformed TOON, non-dict root
- **Shared library tests**: extend `test_shared_library.py` with `.json` and `.toon` asset versions
- **Gen command tests**: test `--format` flag output for each format
- **Sidecar collision test**: verify `.json` orchestration does not load itself as metadata sidecar

### Notes

- The `reflector.py` YAML parsing (LLM-generated plan output) is unrelated and untouched.
- `shared_library.py:211` uses `.json` for metadata sidecars — conflicts with `.json` orchestrations; handled by skipping sidecar when orchestration is `.json`.
- `init` command creates `hello.yml` — left as YAML (it's the default format).
- `bundled/orchestrations/meta_orchestrator.yml` and `bundled/docs/` — untouched, YAML stays as default bundled format.
