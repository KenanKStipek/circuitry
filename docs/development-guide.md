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
