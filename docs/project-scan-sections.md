# Project Scan Section Outputs

## project_structure

- Project root: `/Users/kenanstipek/src/circuitry`
- Repository type: `monolith`
- Scan level: `exhaustive`
- Scan exclusions: `.venv/`, `.claude/`, `.codex/`, `_bmad/`, `_bmad-output/`
- High-level directories scanned: `src/`, `scripts/`, `examples/`, `docs/`, `readme/`
- Classification rationale:
  - Python package layout via `pyproject.toml` + `src/` package dir
  - Reusable library core under `src/circuitry/core/`
  - CLI surface under `src/circuitry/cli/` and `scripts/circuitry`

## project_parts_metadata

- Parts detected: `1`
- Part ID: `core`
- Part name: `circuitry`
- Part root: `/Users/kenanstipek/src/circuitry`
- Project type ID: `library`
- Primary language: `Python`
- Architecture shape: `single-package monolith with CLI + core runtime + adapter layer`
- Approx source inventory:
  - Python source files: `27` (`src/circuitry/**/*.py`)
  - Example orchestrations: `6` (`examples/*.yml`)
- Key entry points:
  - `scripts/circuitry`
  - `src/circuitry/cli/app.py`

## existing_documentation_inventory

- `README.md` — project-level readme
- `docs/Circuitry 2c34435ec2e080c89fc0f253880c2612.md` — domain overview
- `docs/Circuitry Terminology 2c94435ec2e0808daeeff76f7ed1ed25.md` — canonical terminology
- `docs/Circuitry Type System 2f34435ec2e0808394e7ddbb86d14a89.md` — type system and object shapes
- `docs/Conditional Cybernetic 2f34435ec2e08022864fd77c7f2d5b20.md` — conditional behavior
- `docs/Dynamic 2c94435ec2e0809d87eff08463c9ca97.md` — dynamic semantics
- `docs/Example Orchestration 2f34435ec2e080ba995dcc11dd09047c.md` — end-to-end orchestration example
- `docs/Loop Cybernetic 2f34435ec2e080ea9024ca18becd5df1.md` — loop behavior
- `docs/Prompt 2c94435ec2e080feb508d739b3272408.md` — prompt semantics
- `docs/Reflector 2c94435ec2e080468aa4ebb005c4c635.md` — reflector/planning semantics
- `docs/project-scan-sections.md` — generated workflow section outputs (current run)

Summary: Existing docs are architecture/spec heavy and sufficient as primary source of intent for the current documentation pass.

## user_provided_context

- User guidance: "start with these documents to understand what is being built"
- Interpretation for scan execution:
  - Prioritize `docs/*.md` and `README.md` as canonical intent sources.
  - Reconcile implementation findings against these docs during later sections.

## technology_stack

| Category | Technology | Version | Justification |
|---|---|---|---|
| Language | Python | 3.x (project-targeted) | Source code and packaging are Python (`src/circuitry/**/*.py`, `pyproject.toml`). |
| Packaging | setuptools | `>=68` | Build backend in `pyproject.toml`. |
| CLI Framework | Typer | `>=0.12` (`0.21.0` locked) | Command surface in `src/circuitry/cli/app.py` built with Typer decorators/options. |
| Terminal UI | Rich | `>=13.7` (`14.2.0` locked) | Console tables/panels/status used in CLI and doctor commands. |
| YAML Parsing | PyYAML | `>=6.0` (`6.0.3` locked) | Orchestration files loaded as YAML. |
| Template Rendering | Chevron (Mustache) | `>=0.14` (`0.14.0` locked) | Prompt interpolation via `chevron.render(...)` in prompt runtime. |
| Config/Env | python-dotenv | `>=1.0` (`1.2.1` locked) | Environment variable support for runtime/provider settings. |
| Runtime Model Access | Ollama adapter | config-driven (`phi3:mini` default) | Default adapter/model defined in `config.json`; local model endpoint configured. |
| Optional Providers | OpenAI, Anthropic, LiteLLM | adapter-defined defaults | Pluggable adapter layer in `src/circuitry/adapters/`. |
| Testing | pytest | `>=8.0` (`9.0.2` locked) | Test toolchain dependency declared. |
| Linting | Ruff | `>=0.6` (`0.14.10` locked) | Lint/static style checks declared. |
| Type Checking | mypy | `>=1.10` (`1.19.1` locked) | Static typing checks declared. |

## architecture_patterns

- Primary pattern: `Layered runtime library`
- Pattern details:
  - `CLI layer`: command parsing and operator UX (`src/circuitry/cli/*`)
  - `Configuration/effective-settings layer`: resolves config + orchestration defaults
  - `Core execution layer`: compiler + runtimes for prompt/dynamic/conditional/loop/reflector (`src/circuitry/core/*`)
  - `State layer`: deterministic nested store abstraction (`src/circuitry/core/store/store.py`)
  - `Adapter boundary`: provider-specific model integration (`src/circuitry/adapters/*`)
- Control-flow style: explicit declarative orchestration compiled into runtime definitions, with deterministic execution paths and auditable state writes.
- Repository architecture classification: single-part Python library with first-party CLI entrypoints.

## comprehensive_analysis_core

- Part: `core` (`project_type_id=library`)
- Scan mode: `exhaustive`
- Conditional requirement evaluation from documentation requirements:
  - `requires_api_scan=false` → API contract generation skipped for this part.
  - `requires_data_models=false` → data model generation skipped for this part.
  - `requires_state_management=false` → frontend state-management inventory not required.
  - `requires_ui_components=false` → UI component inventory not required.
  - `requires_deployment_config=false` → deployment docs optional, not required by part profile.

Exhaustive findings:
- Core code surface: `src/circuitry/{adapters,cli,core}` with deterministic runtime orchestration engine.
- Entry points detected:
  - `scripts/circuitry` (shell wrapper)
  - `src/circuitry/cli/app.py` (Typer application)
  - package module roots via `__init__.py` under adapters/cli/core/store.
- Configuration management:
  - `config.json` (runtime defaults: adapter/model/provider settings)
  - `pyproject.toml` (build + package metadata)
  - `requirements*.txt` (runtime/dev/locked dependencies)
- Shared code patterns:
  - `src/circuitry/core/*` contains shared execution/compiler/store primitives reused by CLI runtime shim.
  - adapter abstractions in `src/circuitry/adapters/*` used across runtime execution.
- Auth/security patterns:
  - Provider API key usage in environment variables (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`) inside adapter implementations.
  - No internal user-auth/session/authorization subsystem detected.
- Async/event patterns:
  - No queue/worker/pub-sub/event-bus architecture detected in implementation.
- CI/CD patterns:
  - No `.github/workflows/`, `.gitlab-ci.yml`, `Jenkinsfile`, or equivalent CI manifests detected.
- Test/code-quality tooling:
  - `pytest`, `ruff`, `mypy` configured via dependency files; explicit test tree not present in current repository snapshot.

Batch completion summaries (exhaustive mode):
- Batch 1: `src/circuitry/` — scanned all Python implementation files; extracted runtime, adapter, and CLI architecture.
- Batch 2: project root configs (`pyproject.toml`, `requirements*.txt`, `config.json`) — extracted build/runtime defaults and toolchain.
- Batch 3: `examples/` + `docs/` + `README.md` — extracted orchestration examples and intended domain semantics for cross-checking.

## source_tree_analysis

- Generated full annotated tree at `docs/source-tree-analysis.md`.
- Captures root layout, code ownership by layer (cli/core/adapters), entrypoints, and documentation/example placement.

## critical_folders_summary

- `src/circuitry/core/` — deterministic execution and effect semantics.
- `src/circuitry/adapters/` — model provider abstraction layer.
- `src/circuitry/cli/` — user-facing command surface and runtime bridge.
- `examples/` — behavior fixtures for orchestration features.
- `docs/` — authoritative conceptual specs and generated scan artifacts.

## development_instructions

- Created `docs/development-guide.md` with prerequisites, setup, CLI commands, config handling, and quality tooling.
- Installation baseline from README and requirements:
  - `pip install -e .`
  - `pip install -r requirements-dev.txt`
- Runtime defaults documented from `config.json` (`ollama` + `phi3:mini`).

## deployment_configuration

- Deployment/operations scan findings:
  - No Dockerfile, compose file, Kubernetes manifests, Terraform, or CI workflow manifests detected.
  - No explicit release pipeline files detected in repository.
- Operational guidance currently centers on local CLI execution and adapter connectivity checks (`doctor` command).

## contribution_guidelines

- No `CONTRIBUTING.md` or equivalent contribution policy file detected.
- Recommended baseline (derived from tooling in repo):
  - Run `pytest`, `ruff check .`, and `mypy src` before merge.
  - Validate at least one example orchestration via CLI before changes to runtime/compiler/adapter paths.

## architecture_document

- Generated `docs/architecture.md` (single-part architecture file).
- Includes: executive summary, stack, layered architecture pattern, runtime flow, component map, adapter boundary, state/auditability model, testing and ops notes.

## supporting_documentation

Generated/updated supporting documentation:
- `docs/project-overview.md`
- `docs/source-tree-analysis.md` (already generated in Step 5)
- `docs/development-guide.md` (already generated in Step 6)
- `docs/architecture.md` (generated in Step 8)

Not generated because not required for current project profile (`library`) or no source artifacts detected:
- `component-inventory.md`
- `api-contracts.md`
- `data-models.md`
- `deployment-guide.md`
- `contribution-guide.md`
- `integration-architecture.md`
- `project-parts.json`

## index

- Generated `docs/index.md` as master retrieval/navigation document.
- Verified links target currently existing generated and source domain docs.
