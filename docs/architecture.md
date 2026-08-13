# Architecture

## Executive Summary

Circuitry is a single-package Python orchestration runtime for deterministic execution of model-driven workflows. The system compiles declarative YAML orchestration definitions into executable runtime definitions and records all execution outputs and metadata into hierarchical state.

## Technology Stack

| Layer | Technology | Notes |
|---|---|---|
| Language | Python | Core runtime and CLI implementation |
| Packaging | setuptools (`pyproject.toml`) | Source layout at `src/` |
| CLI | Typer + Rich | Command UX, validation, inspect, and doctor flows |
| Orchestration Input | YAML (`PyYAML`) | DSL definitions for prompts/dynamics/conditionals/loops/reflectors |
| Template Rendering | Chevron (Mustache) | Prompt interpolation from execution state |
| Model Providers | Ollama/OpenAI/Anthropic/LiteLLM/CyberDiner adapters (29 in-tree) | Configurable adapter boundary |
| Quality Tooling | pytest, ruff, mypy | Dev verification and code quality |

## Architecture Pattern

- Pattern: layered runtime library
- Key layers:
  - `cli`: command handling, operator output, config loading
  - `core compiler/runtime`: compilation and deterministic effect execution
  - `store`: nested state and deterministic writes
  - `adapters`: provider-specific transport/normalization

## Runtime Flow

1. Load orchestration YAML.
2. Resolve effective runtime settings (model/adapter/runtime config).
3. Compile orchestration into runtime definitions.
4. Execute runtime effects deterministically.
5. Persist values and metadata into hierarchical state.

## Runtime Checkpoints (Code Paths)

1. CLI/API entry
   - `src/circuitry/cli/app.py`
   - `src/circuitry/api.py`
2. Request normalization and settings resolution
   - `src/circuitry/cli/runtime_shim.py`
3. Orchestration parse/load
   - `src/circuitry/cli/orchestration_loader.py`
4. Compilation
   - `src/circuitry/core/compiler.py`
5. Effect execution and state writes
   - `src/circuitry/core/runtime.py`
   - `src/circuitry/core/store/store.py`
6. Diagnostics and metadata
   - `src/circuitry/core/diagnostics.py`
   - `runtime.*` sections in emitted state

## Core Components

- `src/circuitry/core/compiler.py`: compiles YAML effects into runtime definition objects.
- `src/circuitry/core/dynamic.py`: executes dynamic effect containers.
- `src/circuitry/core/prompt.py`: executes atomic model invocations with typed decoding.
- `src/circuitry/core/conditional.py`: handles branching (model/CEL condition modes).
- `src/circuitry/core/loop.py`: handles iteration (`while`/`each`).
- `src/circuitry/core/reflector.py`: optional planning loop producing prime dynamics.
- `src/circuitry/core/store/store.py`: hierarchical state store abstraction.

## Adapter Boundary

- Adapter protocol normalizes `generate(model, prompt, timeout_seconds)` semantics.
- Concrete adapters (see `ADAPTER_REGISTRY` in `factory.py` for the full compiled-in set):
  - `ollama.py`
  - `openai.py`
  - `anthropic.py`
  - `litellm.py`
  - `cyberdiner.py` — job-queue broker; submits a job to CyberDiner expo and polls until terminal, so the queue stays behind the synchronous `generate()`. `model` is a capability tier (`cheap`, `fast`, `good`, `good-fast`, `alpha` …), validated by the network rather than the client.
- Adapter factory (`factory.py`) resolves implementation from runtime config.

## State and Auditability

- State is hierarchical and mutable only through controlled store writes.
- Runtime writes include both effect `value` and `meta` with timestamps, model/adapter identity, token fields, and errors.
- The design emphasizes deterministic control flow with explicit effect definitions.

## Source Tree Reference

See `docs/source-tree-analysis.md` for annotated folder and file layout.

## API Reference

See `docs/api-reference.md` for stable integration surface, exported symbols, and update/versioning guidance.

## Development Workflow

See `docs/development-guide.md` for setup, commands, and local verification steps.

## Deployment and Operations

No dedicated deployment manifests (Docker/K8s/Terraform/CI pipelines) are currently present in this repository snapshot.

## Testing Strategy

- Declared tooling: `pytest`, `ruff`, `mypy`.
- Suggested baseline checks for code changes:
  - `pytest`
  - `ruff check .`
  - `mypy src`
- Practical runtime validation via example orchestrations in `orchestrations/` and CLI commands.
