# Project Overview

## Summary

Circuitry is a deterministic orchestration framework for AI model invocations. It executes declarative YAML workflows composed from prompts, dynamics, conditionals, loops, and reflectors, while recording outputs and execution metadata into hierarchical state.

## Repository Classification

- Type: monolith (single-part)
- Part ID: `core`
- Project type profile: `library`
- Primary language: Python

## Core Capabilities

- Compile orchestration YAML into runtime definitions.
- Execute explicit control-flow topologies (`chain`, `tree` aliases).
- Support typed prompt outputs with optional schema handling.
- Provide deterministic runtime semantics for conditional and loop constructs.
- Integrate with multiple model providers through pluggable adapters.

## Technology Snapshot

- Python package (`setuptools`, `src/` layout)
- CLI: Typer + Rich
- Orchestration parsing: PyYAML
- Template rendering: Chevron
- Runtime defaults: Ollama adapter (`phi3:mini` in `config.json`)
- Dev tooling: pytest, ruff, mypy

## Architecture Shape

Layered design with clear boundaries:
- `cli/` for command surface and operator workflow
- `core/` for compiler/runtime/store semantics
- `adapters/` for provider transport normalization

## Key Entry Points

- `scripts/circuitry`
- `python -m circuitry.cli.app`
- `src/circuitry/cli/app.py`

## Documentation Map

- `docs/architecture.md` — architecture and runtime flow
- `docs/source-tree-analysis.md` — annotated tree and critical folders
- `docs/development-guide.md` — setup and local commands
- `docs/project-scan-sections.md` — trace of generated workflow sections
