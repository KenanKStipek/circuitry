# Development Guide

## Prerequisites

- Python 3.x
- `pip`
- Local Ollama (default runtime target) if running live model calls

## Setup

```bash
pip install -e .
pip install -r requirements-dev.txt
```

Optional environment variables for non-default adapters:
- `OPENAI_API_KEY`
- `ANTHROPIC_API_KEY`

Optional dependency for Postgres state persistence:
- `psycopg[binary]`

## Local Commands

Run CLI via script wrapper:
```bash
./scripts/circuitry --help
./scripts/circuitry run examples/hello.yml
./scripts/circuitry run examples/hello.yml --dry-run
./scripts/circuitry run examples/hello.yml --verbose
```

Run CLI via module:
```bash
python -m circuitry.cli.app run examples/hello.yml
python -m circuitry.cli.app validate examples/hello.yml
python -m circuitry.cli.app inspect examples/hello.yml
python -m circuitry.cli.app doctor --generate
```

## Configuration

Default configuration file:
- `config.json`

Key defaults in this repo:
- Adapter: `ollama`
- Model: `phi3:mini`
- Ollama base URL: `http://localhost:11434`

Adapter/provider precedence:
- Adapter: CLI > orchestration > config > unset
- Model: CLI > orchestration > config > unset

When configured, each run persists resolved values and sources to:
- `runtime.effective_settings.adapter`
- `runtime.effective_settings.model`
- `runtime.effective_settings.sources`

## Build and Packaging

- Build backend: `setuptools.build_meta`
- Source package root: `src/`

## Quality and Testing

Tooling is declared in dev requirements:
- `pytest`
- `ruff`
- `mypy`

Typical local usage:
```bash
pytest
ruff check .
mypy src
```

Adapter conformance suite:
```bash
.venv/bin/pytest -q tests/adapters/test_conformance.py
```

Postgres persistence tests:
```bash
.venv/bin/pytest -q tests/cli/test_postgres_persistence.py tests/core/test_postgres_persistence_config.py
```

Integration test (SQLite persistence + local Ollama inference):
```bash
CIRCUITRY_RUN_INTEGRATION=1 CIRCUITRY_INTEGRATION_MODEL=smollm2:135m \
  .venv/bin/pytest -q tests/integration/test_sqlite_persistence_integration.py -m integration
```

Plugin runtime tests:
```bash
.venv/bin/pytest -q tests/cli/test_plugins_runtime.py
```

## Story Implementation Rule

For every implementation story, tests are part of the solution, not a follow-up task.

Minimum rule set:
- Add or update automated tests for every behavior change.
- If behavior is state-path related, include deterministic path assertions.
- Do not mark a story complete without passing:
  - `pytest`
  - `ruff check .`
  - `mypy src`

Use `docs/testing-policy.md` as the story-level checklist and Definition of Done.
