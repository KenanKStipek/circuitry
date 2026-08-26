# Circuitry

[![PyPI version](https://img.shields.io/pypi/v/circuitry-cof.svg)](https://pypi.org/project/circuitry-cof/)
[![CI](https://github.com/kenankstipek/circuitry/actions/workflows/quality.yml/badge.svg)](https://github.com/kenankstipek/circuitry/actions/workflows/quality.yml)
[![Python](https://img.shields.io/badge/python-3.10%E2%80%933.13-blue.svg)](https://github.com/kenankstipek/circuitry)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

**Circuitry** (CLI: `cof`) is the **C**ybernetic **O**rchestration **F**ramework — a YAML-first runtime for LLM pipelines that observe their own output and adapt. Loops re-evaluate continuation against what the model just produced, conditionals branch on accumulated state, and reflectors plan from observed results. It's a closed-loop control system for model invocations, not a chain of static prompts. The design draws on the cybernetics of [Gordon Pask](https://en.wikipedia.org/wiki/Gordon_Pask): a run is effects in conversation with each other through state — a system in conversation with itself about what to do next.

> "Control mechanisms that lay their own plans." — Gordon Pask, *An Approach to Cybernetics* (1961)

An orchestration is exactly that: a declared control mechanism which, through reflectors and decomposition, lays its own plans.

This README is the short tour — install, first run, the mental model, and where things live. The **[Guidebook](docs/guidebook/README.md)** is the long one: every primitive, every state path, every switch, in fourteen chapters with one running example. It is also available as a [PDF](docs/guidebook/circuitry-guidebook.pdf) and an [EPUB](docs/guidebook/circuitry-guidebook.epub).

## Getting started

### Install

```bash
curl -fsSL https://raw.githubusercontent.com/kenankstipek/circuitry/main/scripts/install.sh | sh
```

Or manually — `pipx install circuitry-cof` from PyPI, `pipx install git+https://github.com/kenankstipek/circuitry.git` for the latest `main`, or `pip install -e .` from a checkout for development. Any of these gives you the `cof` command. The Python import name is `circuitry`; the PyPI distribution is `circuitry-cof`.

### First run

```bash
cof setup                                   # detect local backends, pick a model, write config
cof list                                    # browse the bundled library (learn, utilities, patterns, recipes, agents)
cof run learn/hello -e name=World           # run one by slash name
cof run learn/hello -e name=World --tail    # just the final value
cof tui                                     # or drive everything from the terminal UI
```

`cof setup` defaults to a local [Ollama](https://ollama.com/) — no keys required. Hosted providers read their API keys from the environment (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, …); `cof doctor` tells you what is reachable.

### Your first orchestration

An orchestration is one YAML document: a list of effects that read each other's output through state.

```yaml
# dinner.yml
effects:
  - type: prompt
    name: dish
    template: "Suggest one main course for {{input.occasion}}."
  - type: prompt
    name: shopping_list
    template: "List the ingredients to buy for: {{prime.dish.value}}"
```

```bash
cof check dinner.yml
cof run dinner.yml -e occasion="anniversary dinner" --tail
```

`dish` writes to `prime.dish.value`; `shopping_list` reads it there. Every path is deterministic, derived from the effect's name — that one rule is what makes the feedback loops in the guidebook reliable.

### From Python, or from a Claude session

```python
from circuitry import run_orchestration

result = run_orchestration(orchestration_path="dinner.yml", state={"occasion": "anniversary dinner"})
print(result.ok)                              # True / False
print(result.state["prime"]["shopping_list"]["value"])
```

The same runtime is reachable as an MCP server — `circuitry-mcp` — where a Claude Code or Claude Desktop chat drives the orchestration and the host session *is* the model. See [Surfaces](docs/guidebook/12-surfaces.md).

## Why Circuitry?

Every framework can call a model in a loop. Here, the loop is the language: `if`, `loop`, and `reflector` are declared in the same YAML grammar as the prompts they steer, and the model can be the sensor as well as the actuator. Because the control loop is declared rather than coded, it is validated before it runs, recorded while it runs, and portable across providers after it runs — and the runtime steers itself the same way, scoring, routing, and decomposing prompts. Steering isn't a feature bolted onto a pipeline runner; it's the organizing idea.

Pick Circuitry when the topology — what runs after what, conditional on what — is the thing you want to design and read.

## Mental model

![Circuitry — the shape of the language](docs/assets/shape-of-the-language.svg)

The language has exactly seven effects, and they fall into three kinds:

- **Leaf effects** — `prompt`, `tool`, `use`. They act: one model call, one plugin call, one child orchestration. They contain no other effects.
- **Structure** — `dynamic`. It always contains effects and never decides anything; it is pure composition, sequential (`chain`) or parallel (`tree`).
- **Cybernetic effects** — `if`, `loop`, `reflector`. They read state and steer: which branch, another pass, a new plan.

Two of the seven are the base monads everything else is built from: **prompt** (one model call wrapped with its trace — `f(state) → (state', value)`) and **dynamic** (composition of effects). The rest of the language is those two ideas reused at every scale, and every effect, whatever its kind, communicates the same way: it writes a value to a deterministic state path, and later effects read it there. State has three root namespaces — `input` (what the caller supplied), `prime` (what effects produced), `runtime` (what the framework recorded) — and two reading languages: Mustache in templates (`{{prime.dish.value}}`), CEL in conditions (`state.prime.dish.value != ""`).

Capability lives outside the document. Adapters, models, tool allowlists, persistence, and the optional complexity layer — score every prompt, route it to the cheapest capable model, decompose what is too big — are configuration, so an orchestration written against a local 8B model runs unchanged against a hosted one.

The guidebook walks this in three acts: **[Basics](docs/guidebook/README.md#part-i--basics-the-machine)** is the machine, **[Cybernetics](docs/guidebook/README.md#part-ii--cybernetics-the-machine-steering-itself)** is the machine steering itself, **[The machine in the world](docs/guidebook/README.md#part-iii--the-machine-in-the-world)** is composition, complexity, and every surface.

## Repository layout

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
│   ├── runtime_plugins/      30 observers: persistence backends, pub/sub, telemetry exporters
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
├── scripts/                  install.sh, the curation smoke test, the changelog compiler
├── changelog.d/              one changelog fragment per change, compiled at release
├── .claude/                  the /cof slash command and agent settings
├── CHANGELOG.md · CONTRIBUTING.md · RELEASING.md · SECURITY.md · CODE_OF_CONDUCT.md
└── pyproject.toml · requirements-dev.txt
```

The library under `src/circuitry/curation/` is what `cof list` shows: **`learn/`** is single-primitive demonstrations, one concept per file; **`utilities/`** are composable single-output orchestrations with typed interfaces, called via `use:`; **`patterns/`** are multi-primitive templates (critique → refine, parallel → judge, classify → route); **`recipes/`** are full workflows; **`agents/`** are orchestrations that build or improve orchestrations — the wizard, the meta-orchestrator, the decomposition planner. `cof eject <name>` copies any of them into your project.

## Architecture

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

[docs/architecture.md](docs/architecture.md) maps these to code paths.

## Design principles

1. **Cybernetic feedback** — effects observe state written by prior effects and adapt; the system steers itself.
2. **Deterministic state paths** — orchestration structure maps to known state keys; feedback is reliable because paths are predictable.
3. **Explicit control flow** — no implicit branching or hidden reasoning; topology is declared in YAML.
4. **Full auditability** — every effect, branch decision, iteration, and model-routing choice is recorded to state.
5. **Model agnostic** — adapters abstract provider differences; orchestrations are portable.
6. **Composable** — orchestrations are building blocks; `use` chains them with state isolation and typed interfaces.

## Documentation

| Read | For |
| --- | --- |
| [The Guidebook](docs/guidebook/README.md) | the full tour: Prompt · Dynamic · State · Configuration · Errors · If · Loop · Reflector · Composition · Complexity · Decomposition · Surfaces · Tools & persistence · The whole meal |
| [Orchestration Reference](docs/orchestration-reference.md) | every field of every effect, in tables |
| [Grammar](docs/guidebook/grammar.md) | the formal grammar of the language |
| [docs/index.md](docs/index.md) | the map of everything else — routing, profiles, the TUI, the wizard, plugins, persistence, stability, the threat model |

## Stability

The public API surface (Python re-exports, `cof` CLI flags, JSON Schema, state paths) and the versioning policy are spelled out in [`docs/stability.md`](docs/stability.md). Circuitry is in `0.x` alpha; `0.x` minor bumps may include breaking changes, called out in [`CHANGELOG.md`](CHANGELOG.md).

## Privacy & telemetry

Circuitry collects **no telemetry**. The only outbound calls it makes are to the LLM adapter, tool plugin, and persistence backend you configured. See [`SECURITY.md`](SECURITY.md) and [`docs/threat-model.md`](docs/threat-model.md).

## Community

Questions, ideas, and "show and tell" go in [GitHub Discussions](https://github.com/kenankstipek/circuitry/discussions). Bug reports and feature requests go in [Issues](https://github.com/kenankstipek/circuitry/issues). There is no Discord or Slack — keeping the conversation in one indexable place is intentional for v0.1.

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for setup, conventions, and where to start.

## License

MIT — see [`LICENSE`](LICENSE).
