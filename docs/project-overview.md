# Project Overview

## Summary

Circuitry is a deterministic orchestration framework for AI model invocations. It executes declarative YAML workflows composed from prompts, dynamics, conditionals, loops, and reflectors, while recording outputs and execution metadata into hierarchical state.

**Status:** MVP complete (2026-03-02). All 6 epics, 24 stories delivered. 151 tests passing.

## Repository Classification

- Type: monolith (single-part)
- Part ID: `core`
- Project type profile: `library`
- Primary language: Python

## Core Capabilities

- Compile orchestration YAML into runtime definitions with JSON Schema validation (Draft-07).
- Execute explicit control-flow topologies (`chain`, `tree` with parallel execution).
- Support typed prompt outputs with optional schema handling.
- Provide deterministic runtime semantics for conditional and loop constructs (including `collect` and `flow: tree`).
- Integrate with multiple model providers through pluggable adapters (Ollama, OpenAI-compatible, LiteLLM).
- Trigger orchestrations via REST endpoints and cron-based scheduling.
- Persist runtime state through Postgres-backed tooling.
- Extend runtime via tool effect plugins (ffmpeg, ComfyUI) and runtime lifecycle plugins.
- Retrieve, execute, and contribute shared-library orchestration assets.

## Technology Snapshot

- Python package (`setuptools`, `src/` layout)
- CLI: Typer + Rich
- Orchestration parsing: PyYAML + jsonschema (Draft-07)
- Template rendering: Chevron
- Adapters: Ollama, OpenAI-compatible, LiteLLM
- Tool plugins: ffmpeg, ComfyUI (img2img, FLUX models)
- Dev tooling: pytest, ruff, mypy
- CI: GitHub Actions (`.github/workflows/quality.yml`)

## Architecture Shape

Layered design with clear boundaries:
- `cli/` for command surface, operator workflow, and runtime shim
- `core/` for compiler, runtime, store semantics, and runtime plugins
- `adapters/` for provider transport normalization
- `plugins/` for tool effect providers (ffmpeg, ComfyUI)
- `schema/` for orchestration JSON Schema validation

## Key Entry Points

- `scripts/circuitry` (auto-injects config, defaults `--verbose`)
- `python -m circuitry.cli.app`
- `src/circuitry/cli/app.py`
- `src/circuitry/cli/runtime_shim.py` (embedded Python API: `validate()`, `run()`)

## Documentation Map

- `docs/architecture.md` — architecture and runtime flow
- `docs/orchestration-reference.md` — canonical YAML reference + LLM Authoring Rules
- `docs/api-reference.md` — embedded Python API documentation
- `docs/source-tree-analysis.md` — annotated tree and critical folders
- `docs/development-guide.md` — setup and local commands
- `docs/project-scan-sections.md` — trace of generated workflow sections

## Post-MVP Roadmap

- **Perceptron** — real-time state visualization GUI for orchestration execution
- **Postgres integration testing** — CI-accessible real database tests
- **Shared library CI** — external repo with automated validation for contributions
- **Additional tool plugins** — expand beyond ffmpeg and ComfyUI
- **Self-building orchestrations** — system processes and operationalizes submitted orchestrations
