# Architecture

## Executive Summary

Circuitry is a single-package Python orchestration runtime for deterministic execution of model-driven workflows. The system compiles declarative YAML orchestration definitions into executable runtime definitions and records all execution outputs and metadata into hierarchical state.

## Overview

```
Orchestration YAML
       |
    Compiler ──> Definition Objects ──> Allowlist + Preflight Gates
       |
    Runtime ──> Effect Execution ──> State Writes ──> on_effect_complete hooks
       |              |
    Store        Adapter Layer ──> Model Provider
  (feedback)        Tool Plugins ──> external systems
                    Runtime Plugins ──> persistence / observability
```

- **Compiler** — parses YAML into typed definition objects; validates against a Draft-07 JSON Schema and the static rules (namespace-rooted paths, unique names, `use` cycles).
- **Allowlist + Preflight** — gates every referenced extension before any model call.
- **Runtime** — executes definitions, manages state feedback between effects, fires lifecycle hooks; the complexity layer (scoring, routing, decomposition) sits here.
- **Store** — hierarchical state with deterministic path resolution.

## Design principles

1. **Cybernetic feedback** — effects observe state written by prior effects and adapt; the system steers itself.
2. **Deterministic state paths** — orchestration structure maps to known state keys; feedback is reliable because paths are predictable.
3. **Explicit control flow** — no implicit branching or hidden reasoning; topology is declared in YAML.
4. **Full auditability** — every effect, branch decision, iteration, and model-routing choice is recorded in the shadow state.
5. **Model agnostic** — adapters abstract provider differences; orchestrations are portable.
6. **Composable** — orchestrations are building blocks; `use` chains them with state isolation and typed interfaces.

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

- State is a **shadow state**: a tree with the same keys as the orchestration (one node per named effect), mutable only through controlled store writes.
- Runtime writes include both effect `value` and `meta` with timestamps, model/adapter identity, token fields, and errors.
- The design emphasizes deterministic control flow with explicit effect definitions.

## Source Tree Reference

```
circuitry/
├── src/circuitry/            the package (import circuitry; CLI cof)
│   ├── core/                 compiler, runtime, and the seven effects
│   │   ├── compiler.py       YAML → typed definitions, Draft-07 schema validation, static rules
│   │   ├── prompt.py · dynamic.py · conditional.py · loop.py · reflector.py · use.py · tool.py
│   │   ├── store/            hierarchical state with deterministic path resolution
│   │   ├── complexity.py · router.py · decompose.py    the complexity layer
│   │   └── primes.py         the planning directives (reflector, wizard, decomposition)
│   ├── schema/               orchestration.schema.json, profile.schema.json
│   ├── adapters/             29 model providers behind one Adapter protocol
│   ├── plugins/              70+ tool providers (fs, http, ffmpeg, github, slack, mcp, …)
│   ├── runtime_plugins/      31 observers: persistence backends, pub/sub, telemetry exporters
│   ├── curation/             the bundled library — learn/ utilities/ patterns/ recipes/ agents/
│   ├── bundled/              the authoring rules and docs injected into cof gen / cof wizard
│   ├── cli/                  cof (Typer + Rich): config, profiles, doctor, score, library sources
│   ├── tui/                  cof tui (Textual)
│   ├── mcp/                  circuitry-mcp — the MCP server
│   ├── service/              REST trigger and scheduler
│   └── api.py                run_orchestration and the rest of the public SDK
├── tests/                    pytest, mirroring src/ (core, cli, adapters, plugins, docs, integration)
├── docs/
│   ├── guidebook/            the Guidebook — fourteen chapters, the grammar, PDF and EPUB builds
│   ├── orchestration-reference.md    the field-by-field reference for every effect
│   ├── assets/               figures
│   └── examples/             runnable routing and profile examples
├── editor/                   VS Code syntax highlighting for orchestration YAML
├── scripts/                  install.sh, the curation smoke test, the changelog compiler and checker, the guidebook build
├── changelog.d/              one changelog fragment per change, compiled at release
├── .claude/                  the /cof slash command and agent settings
├── CHANGELOG.md · CONTRIBUTING.md · RELEASING.md · SECURITY.md · CODE_OF_CONDUCT.md
└── pyproject.toml · requirements-dev.txt
```

The library under `src/circuitry/curation/` is what `cof list` shows: **`learn/`** is single-primitive demonstrations, one concept per file; **`utilities/`** are composable single-output orchestrations with typed interfaces, called via `use:`; **`patterns/`** are multi-primitive templates (critique → refine, parallel → judge, classify → route); **`recipes/`** are full workflows; **`agents/`** are orchestrations that build or improve orchestrations — the wizard, the meta-orchestrator, the decomposition planner. `cof eject <name>` copies any of them into your project.

## API Reference

See `docs/api-reference.md` for stable integration surface, exported symbols, and update/versioning guidance.

## Development Workflow

See [CONTRIBUTING.md](../CONTRIBUTING.md) for setup, commands, and local verification steps.

## Deployment and Operations

No dedicated deployment manifests (Docker/K8s/Terraform/CI pipelines) are currently present in this repository snapshot.

## Testing Strategy

- Declared tooling: `pytest`, `ruff`, `mypy`.
- Suggested baseline checks for code changes:
  - `pytest`
  - `ruff check .`
  - `mypy src`
- Practical runtime validation via the curation library (`bash scripts/smoke-curation.sh`) and CLI commands.
