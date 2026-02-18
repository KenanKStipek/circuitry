# Source Tree Analysis

Project root: `/Users/kenanstipek/src/circuitry`
Scan profile: `exhaustive` (excluded: `.venv/`, `.claude/`, `.codex/`, `_bmad/`, `_bmad-output/`)

## Annotated Tree

```text
circuitry/
├── README.md                            # Primary user-facing project overview and usage
├── pyproject.toml                       # Python packaging/build metadata (setuptools backend)
├── requirements.txt                     # Runtime deps (CLI, YAML, templating)
├── requirements-dev.txt                 # Dev tool deps (pytest, ruff, mypy)
├── requirements.lock.txt                # Locked dependency snapshot
├── config.json                          # Runtime defaults (adapter/model/provider config)
├── scripts/
│   └── circuitry                        # CLI launcher wrapper (entrypoint shortcut)
├── examples/                            # Executable orchestration examples
│   ├── hello.yml
│   ├── dynamic_hello.yml
│   ├── conditional_example.yml
│   ├── loop_example.yml
│   ├── typed_prompt_example.yml
│   └── reflector_v1.yml
├── src/
│   └── circuitry/
│       ├── adapters/                    # Provider abstraction boundary (Ollama/OpenAI/Anthropic/LiteLLM)
│       │   ├── base.py                  # Adapter protocol + generate result types
│       │   ├── factory.py               # Adapter construction from runtime config
│       │   ├── ollama.py                # Local Ollama transport implementation
│       │   ├── openai.py                # OpenAI chat-completions implementation
│       │   ├── anthropic.py             # Anthropic messages API implementation
│       │   └── litellm.py               # LiteLLM provider abstraction implementation
│       ├── cli/                         # Operator-facing command surface
│       │   ├── app.py                   # Main Typer app: run/validate/inspect/version
│       │   ├── doctor.py                # Environment and adapter health checks
│       │   ├── config.py                # Config discovery/loading logic
│       │   ├── runtime_shim.py          # Connects CLI to compiler/runtime/store
│       │   ├── effective_settings.py    # Config + orchestration settings resolution
│       │   └── orchestration_loader.py  # YAML orchestration loading helper
│       └── core/                        # Deterministic orchestration engine
│           ├── compiler.py              # YAML->definition compilation, flow normalization
│           ├── dynamic.py               # Dynamic runtime orchestration executor
│           ├── prompt.py                # Prompt runtime + typed output handling
│           ├── conditional.py           # If/then/else evaluation and branch execution
│           ├── loop.py                  # each/while iteration runtime logic
│           ├── reflector.py             # Reflector planning/runtime loop
│           ├── primes.py                # Reflector prime template constants
│           ├── types.py                 # Shared execution type definitions
│           └── store/
│               └── store.py             # Hierarchical state store abstraction
├── docs/                                # Domain/spec docs + generated scan artifacts
└── readme/                              # Legacy source copy of domain docs
```

## Critical Folders Summary

- `src/circuitry/core/`: Highest-leverage implementation area; contains deterministic runtime semantics and effect executors.
- `src/circuitry/adapters/`: External-provider integration boundary and portability layer.
- `src/circuitry/cli/`: Operational entrypoint for users and scripts; governs run/validate/inspect experience.
- `examples/`: Practical spec fixtures for validating orchestration features and expected behavior.
- `docs/`: Canonical concept/design intent used to align implementation and future planning docs.

## Entry Points

- `scripts/circuitry`
- `src/circuitry/cli/app.py`
- `python -m circuitry.cli.app`
