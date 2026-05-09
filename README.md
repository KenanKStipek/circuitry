# Circuitry

[![PyPI version](https://img.shields.io/pypi/v/circuitry-cof.svg)](https://pypi.org/project/circuitry-cof/)
[![CI](https://github.com/kenankstipek/circuitry/actions/workflows/quality.yml/badge.svg)](https://github.com/kenankstipek/circuitry/actions/workflows/quality.yml)
[![Python](https://img.shields.io/badge/python-3.9%E2%80%933.13-blue.svg)](https://github.com/kenankstipek/circuitry)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

**Circuitry** (CLI: `cof`) is the **C**ybernetic **O**rchestration **F**ramework — a YAML-first runtime for LLM pipelines that observe their own output and adapt. Loops re-evaluate continuation against what the model just produced, conditionals branch on accumulated state, and reflectors plan from observed results. It's a closed-loop control system for model invocations, not a chain of static prompts.

> **Cybernetics** — control theory as it is applied to complex systems. A monitor compares what is happening to a system at various sampling times with some standard of what should be happening, and a controller adjusts the system's behaviour accordingly. — [Britannica](https://www.britannica.com/science/cybernetics)

<!-- DEMO_GIF: replace with an asciinema or terminal-recorded GIF showing `cof run hello` and one looped orchestration -->

## Install

One-line install via pipx (isolated, no virtual env needed):

```bash
curl -fsSL https://raw.githubusercontent.com/kenankstipek/circuitry/main/scripts/install.sh | sh
```

Or install manually:

```bash
pipx install circuitry-cof              # PyPI (once published)
pipx install git+https://github.com/kenankstipek/circuitry.git   # Latest from main
```

Or for development:

```bash
pip install -e .
```

This gives you the `cof` command. The Python import name is still `circuitry` (`from circuitry import run_orchestration`); only the PyPI distribution name is `circuitry-cof` because the unqualified `circuitry` name is taken by an unrelated logic-circuit DSL.

## Quick Start

```bash
# Interactive setup — detects backends, creates config
cof setup

# Browse available orchestrations (organised by category: learn, utilities, patterns, recipes, agents)
cof list

# Run by slash-delimited name (no path needed)
cof run learn/hello -e name=World

# Just the output value
cof run learn/hello -e name=World --tail

# See details for an orchestration
cof info recipes/article_summarizer

# Copy a curation entry into the working directory for editing
cof eject recipes/article_summarizer

# Initialize a new project (creates config + hello.yml)
cof init
```

### Curation library

Pre-built orchestrations live at `src/circuitry/curation/`, organised by category:

- **`learn/`** — single-primitive demonstrations (one concept per file).
- **`utilities/`** — composable, single-output orchestrations called from other orchestrations via `use:`.
- **`patterns/`** — multi-primitive composition templates (kitchen-sink, critique→refine, parallel→judge, classify→route).
- **`recipes/`** — full real-world workflows (article summarizer, comic strip, …).
- **`agents/`** — orchestrations that build or improve other orchestrations.

The `manifest.json` in that directory is the operational registry consumed by `cof list` / `cof run` / `cof info` / `cof eject`. Run `cof list` to see what's available.

## Why Circuitry?

Most LLM frameworks make you express orchestration in code. Circuitry lets you declare it in YAML, and treats every effect as a feedback signal — state is the wire between steps, and downstream effects observe it the same way a controller observes a sensor reading. That makes loops, conditionals, and reflection first-class instead of bolted on.

| Framework | Primary metaphor | When it fits |
| --------- | ---------------- | ------------ |
| **Circuitry** | Cybernetic feedback. YAML effects read each other's state through deterministic paths; loops and conditionals re-evaluate against fresh model output. | You want declarative pipelines that can self-correct, branch on observed state, or call themselves recursively without writing Python glue. |
| LangChain | Python-native chains and agents. Compose by importing classes and wiring callbacks. | You're comfortable in Python and want a large library of pre-built integrations. |
| CrewAI | Agent teams. Roles, tasks, delegation. | You think in terms of multiple cooperating agents rather than a control-flow graph. |
| DSPy | Prompt programs that compile and self-optimize. | You want gradient-style prompt tuning against a metric, not deterministic flow control. |
| BAML | Typed prompts as functions. | You want strict typed I/O for a small, well-defined LLM call surface. |

Pick Circuitry when the orchestration topology — what runs after what, conditional on what — is the thing you want to design and read.

## How It Works

An orchestration is a YAML file declaring **effects** that execute in order. Each effect writes its output to a **deterministic state path** derived from its name. Subsequent effects read that state through **template interpolation**, creating feedback chains.

```yaml
effects:
  - type: prompt
    name: draft
    template: "Write a short intro for {{topic}}."

  - type: prompt
    name: critique
    template: "Critique this draft and suggest improvements: {{prime.draft.value}}"

  - type: prompt
    name: revise
    template: |
      Original: {{prime.draft.value}}
      Feedback: {{prime.critique.value}}
      Write the final version incorporating the feedback.
```

The adapter and model come from your config (see [Configuration](#configuration) below) — orchestrations stay portable.

## CLI Reference

```bash
# Run an orchestration (by name or path)
cof run hello -e name=World
cof run ./my-orch.yml --verbose --out state.json --pretty

# Pipe-friendly: just the final value
cof run hello -e name=World --tail

# Re-run the last orchestration
cof run --last

# JSON output (auto-detected when piped)
cof run hello --json | jq '.prime'

# Live state for external tools
cof run hello -e name=World --live-state ./state.live.json

# Browse, inspect, and eject orchestrations
cof list
cof list --category example
cof info article-summarizer
cof info article-summarizer --json
cof eject comic-strip
cof eject comic-strip --out my-comic.yml

# Validate an orchestration
cof check orchestrations/hello.yml
cof check orchestrations/hello.yml --json

# Generate an orchestration from natural language
cof gen blog_pipeline "Build a pipeline that drafts, critiques, and revises a blog post"
cof gen summarizer "Summarize a PDF" --format toon

# System diagnostics and setup
cof setup              # Interactive backend detection + config wizard
cof setup --json       # Non-interactive detection output
cof doctor             # Check all backends and config
cof doctor --generate  # Also test model connectivity

# Project setup
cof init

# Version
cof version
```

### Auto-Pipe Detection

When stdout is not a TTY (e.g., piped to `jq`), `cof run` automatically switches to `--json` mode with quiet output. `--tail` overrides this when you want just the raw value.

## Configuration

Circuitry uses layered config resolution (highest priority wins):

1. CLI flags (`--config`, inline `-e`)
2. Environment variables (`CIRCUITRY_MODEL`, `CIRCUITRY_ADAPTER`, `CIRCUITRY_ADAPTER_URL`, `CIRCUITRY_COMFYUI_URL`)
3. Project-local config (`circuitry.config.json` or `config.json` in cwd)
4. Global config (`~/.config/circuitry/config.json`)
5. Sane defaults (ollama at localhost:11434, comfyui at localhost:8188)

```json
{
  "default_adapter": "ollama",
  "default_model": "llama3.1:8b",
  "runtime": {
    "adapters": {
      "ollama": {
        "base_url": "http://localhost:11434"
      },
      "openai": {
        "base_url": "https://api.openai.com/v1",
        "default_model": "gpt-4o-mini"
      },
      "anthropic": {
        "base_url": "https://api.anthropic.com",
        "default_model": "claude-sonnet-4-20250514",
        "max_tokens": 4096
      },
      "litellm": {
        "default_model": "openai/gpt-4o-mini"
      }
    }
  }
}
```

Selection precedence per run:
- Model: CLI override > orchestration `model` > config `default_model`
- Adapter: CLI override > orchestration `adapter` > config `default_adapter`

Every run records resolved values in `runtime.effective_settings`.

## Core Primitives

### Prompt

The atomic unit. One model invocation, one typed result, written to state at a deterministic path.

```yaml
- type: prompt
  name: greet_user
  template: "Say hello to {{user_name}} in one sentence."
```

State path: `prime.greet_user.value`

Available types: `text`, `json`, `boolean`, `number`, `array`, `object`

```yaml
- type: prompt
  name: extract_data
  prompt_type: json
  schema:
    type: object
    properties:
      items:
        type: array
        items:
          type: string
    required: [items]
  template: "Extract items as JSON..."
```

### Dynamic

A named scope that composes effects with explicit control flow topology.

```yaml
- type: dynamic
  name: onboarding
  flow: chain
  effects:
    - type: prompt
      name: ask_name
      template: "What is your name?"
    - type: prompt
      name: greet
      template: "Nice to meet you, {{onboarding.ask_name.value}}!"
```

Flow models:
- **chain** (aliases: `chain_of_thought`, `cot`) — sequential, each effect sees prior state
- **tree** (aliases: `tree_of_thought`, `tot`) — parallel execution

### Conditional

Cybernetic branching. The system inspects its own state and selects a path.

```yaml
- type: if
  name: check_role
  if:
    mode: cel
    expr: "state.input.role == 'admin'"
  then:
    - type: prompt
      name: admin_view
      template: "Render admin panel."
  else:
    - type: prompt
      name: user_view
      template: "Render user panel."
```

Evaluation modes:
- **model** — the LLM reads state and decides the branch (cybernetic evaluation)
- **cel** — deterministic evaluation using CEL expressions

### Loop

Cybernetic iteration. The loop body executes, writes state, and the continuation condition evaluates against that new state.

```yaml
- type: loop
  name: refine
  while:
    mode: model
    template: "Does this draft need more improvement? Reply true or false.\n\n{{prime.draft.value}}"
  max_iterations: 5
  body:
    - type: prompt
      name: draft
      template: "Improve this text:\n{{prime.draft.value}}"
```

Loop modes:
- **while** — continue while condition holds (model or CEL evaluated)
- **each** — iterate over a collection, optionally in parallel (`flow: tree`)

Loops support `collect` to aggregate body outputs across iterations:

```yaml
- type: loop
  name: process_items
  each:
    in: state.input.items
    as: item
  collect: process
  body:
    - type: prompt
      name: process
      template: "Process item: {{item}}"
```

Collected at: `prime.process_items.collected.value` (array)

### Use

Composition primitive. Runs another orchestration as an isolated sub-step with explicit input/output mapping.

```yaml
- type: use
  name: summarize_step
  orchestration: article-summarizer
  inputs:
    article_text: "{{prime.fetch.value}}"
    max_words: 50
  outputs:
    summary: prime.summarize.value
```

Resolution order: local file path > bundled orchestration name.

State is fully isolated — the child orchestration runs in its own store. Only mapped inputs are passed in, only mapped outputs are extracted.

Features:
- **`orchestration`** — reference by name (`article-summarizer`) or path (`./my-orch.yml`)
- **`inline`** — Mustache template that renders to orchestration YAML at runtime (for LLM-generated plans)
- **`validate`** — schema validation of inline YAML before execution (default `true`)
- **`on_error`** — `fail` (default), `skip`, or `continue`
- Works inside loops, conditionals, and dynamics

```yaml
# LLM-generated orchestration execution
- type: prompt
  name: generate_plan
  template: "Generate a Circuitry orchestration YAML for: {{goal}}"

- type: use
  name: execute_plan
  inline: "{{prime.generate_plan.value}}"
  validate: true
```

### Reflector

A planning component that reads state and produces execution plans. Reflectors observe the system's current state and generate dynamics — the highest-level cybernetic feedback. Internally delegates to `use(inline)` for validation and execution.

```yaml
- type: reflector
  name: planner
  max_effects: 5
  effects:
    - type: prompt
      name: propose_steps
      template: "Generate next steps based on: {{prime.goal.value}}"
```

### Tool

Extends orchestrations beyond model invocations. Tool effects invoke external systems through plugins.

```yaml
- type: tool
  name: render_video
  provider: ffmpeg
  params:
    input: "{{prime.generate_frames.value}}"
    output: "./output/video.mp4"
```

Built-in providers: `ffmpeg`, `comfyui`

## Interfaces

Orchestrations can declare typed input/output contracts via `interface`. When referenced by a `use` effect, required inputs are validated and output mappings are auto-generated.

```yaml
interface:
  inputs:
    article_text:
      type: string
      required: true
      description: Full article text.
    max_words:
      type: number
      required: false
  outputs:
    summary:
      type: string
      path: prime.summarize.value

effects:
  - type: prompt
    name: summarize
    template: "Summarize in {{max_words}} words: {{article_text}}"
```

When used:

```yaml
# Explicit output mapping
- type: use
  name: sub
  orchestration: article-summarizer
  inputs:
    article_text: "{{prime.fetch.value}}"

# outputs auto-generated from interface:
#   summary → prime.summarize.value
```

## State Paths

Every effect writes to a deterministic path derived from its name. This is what makes interpolation — and therefore cybernetic feedback — reliable.

```
prime.<effect>.value                              # top-level prompt
prime.<dynamic>.<effect>.value                    # inside a dynamic
prime.<loop>.iter_<n>.<effect>.value              # inside a loop iteration
prime.<loop>.collected.value                      # loop collect aggregation
```

Available inside loop body templates:
- `{{_loop_index}}` — zero-based iteration index (both `each` and `while` loops)
- `{{<each.as>}}` — current collection element (`each` loops only)

## Bundled Orchestrations

Circuitry ships with ready-to-run orchestrations. Browse with `cof list`, inspect with `cof info`, and eject with `cof eject`.

```bash
cof list                           # see all
cof list --category example        # filter by category
cof info hello                     # details + source preview
cof eject article-summarizer       # copy to local for editing
```

Categories:
- **example** — hello (simplest orchestration)
- **utility** — article-summarizer
- **creative** — comic-strip (multi-step image generation)
- **tooling** — meta-orchestrator, orchestration-improver, orchestration-improver-judge
- **template** — one per primitive type (prompt, loop, conditional, dynamic, composition, reflector)

## Programmatic API

```python
from circuitry import run_orchestration

result = run_orchestration(
    orchestration_path="hello.yml",
    state={"name": "World"},
    dry_run=True,
)
print(result.ok)                 # True/False
print(result.state["runtime"])   # runtime metadata + outputs
```

Additional embedded API:
- `run_shared_orchestration` — shared-library assets
- `validate_orchestration` — compiler-backed validation
- `inspect_orchestration` — orchestration metadata
- `inspect_divergence_paths` — deterministic failure-path diagnostics

See `docs/api-reference.md` for signatures and integration guidance.

## Architecture

```
Orchestration YAML
       |
    Compiler ──> Definition Objects
       |
    Runtime ──> Effect Execution ──> State Writes
       |              |
    Store        Adapter Layer ──> Model Provider
  (feedback)
```

- **Compiler** — parses YAML into typed definition objects with JSON Schema validation
- **Runtime** — executes definitions, manages state feedback between effects
- **Store** — hierarchical state with deterministic path resolution
- **Adapters** — normalize provider transport (Ollama, OpenAI, Anthropic, LiteLLM)
- **Plugins** — tool effect providers (ffmpeg, ComfyUI) and runtime lifecycle hooks

## Design Principles

1. **Cybernetic Feedback** — effects observe state written by prior effects and adapt; the system steers itself
2. **Deterministic State Paths** — orchestration structure maps to known state keys; feedback is reliable because paths are predictable
3. **Explicit Control Flow** — no implicit branching or hidden reasoning; topology is declared in YAML
4. **Full Auditability** — every effect, branch decision, and iteration is recorded to state
5. **Model Agnostic** — adapters abstract provider differences; orchestrations are portable
6. **Composable** — orchestrations are building blocks; `use` chains them with state isolation and typed interfaces

## Privacy & telemetry

Circuitry collects **no telemetry**. The only outbound calls it makes are to the LLM adapter, tool plugin, and persistence backend you configured. See [`SECURITY.md`](SECURITY.md) and [`docs/threat-model.md`](docs/threat-model.md).

## Stability

The public API surface (Python re-exports, `cof` CLI flags, JSON Schema, state paths) and the versioning policy are spelled out in [`docs/stability.md`](docs/stability.md). Circuitry is in `0.x` alpha; `0.x` minor bumps may include breaking changes, called out in [`CHANGELOG.md`](CHANGELOG.md).

## Community

Questions, ideas, and "show and tell" go in [GitHub Discussions](https://github.com/kenankstipek/circuitry/discussions). Bug reports and feature requests go in [Issues](https://github.com/kenankstipek/circuitry/issues). There is no Discord or Slack — keeping the conversation in one indexable place is intentional for v0.1.

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for setup, conventions, and where to start.

## License

MIT — see [`LICENSE`](LICENSE).
