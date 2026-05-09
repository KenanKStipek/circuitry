# Circuitry

[![PyPI version](https://img.shields.io/pypi/v/circuitry-cof.svg)](https://pypi.org/project/circuitry-cof/)
[![CI](https://github.com/kenankstipek/circuitry/actions/workflows/quality.yml/badge.svg)](https://github.com/kenankstipek/circuitry/actions/workflows/quality.yml)
[![Python](https://img.shields.io/badge/python-3.10%E2%80%933.13-blue.svg)](https://github.com/kenankstipek/circuitry)
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

## Mental Model

Circuitry stacks five abstractions. Each one earns its place by solving a problem the previous one couldn't. The framework's name — Cybernetic Orchestration Framework — describes what it becomes once the third and fourth are layered on top of the first two. Concrete YAML for each primitive lives in [Core Primitives](#core-primitives) below; this section is about why the shape is what it is.

### 1. Prompt — a value in a context

The simplest unit is a single model invocation. Most frameworks model this as just a function call: input goes in, output comes out. Circuitry models it as a *computation that returns a value alongside a transformed context*. The value is the model's response. The context is everything that happened around it — which adapter served the call, how many tokens were spent, when it started and finished, whether it errored, what the rendered prompt actually looked like.

Both pieces matter. The value is what downstream code needs. The context is what makes the run legible after the fact. A prompt isn't `f(x) → y`; it's `f(state) → (state', y)`.

This shape — a computation that produces a value alongside a threaded context — is a monad. Specifically, the state monad. The prompt is the framework's `unit`: the smallest monadic value, a single model call wrapped with its trace.

### 2. Composition — the state object as monadic bind

The moment you have two prompts, you have to talk about what the second knows about the first. Procedural answers (assign to a variable, pass it as an argument) collapse the moment branching, parallelism, or recursion enter the picture. Circuitry's answer is a single state object that every effect writes to and reads from.

That sounds like a global variable, but it isn't. Each effect's output is addressable through a deterministic path derived from its name. The path is part of the contract: an effect called `draft` always writes to `prime.draft.value`, and any future effect that wants to read it knows where to look. Determinism here isn't aesthetic — it's what makes the chaining well-defined. If you couldn't reliably refer to *that specific result of that specific computation*, you couldn't compose.

Putting two prompts in sequence is monadic bind in the state monad: take a value-in-context and feed it to a continuation. The state object is the carried context. The deterministic paths are how the continuation addresses what came before.

### 3. Dynamic — composition as a first-class operation

A pair of effects is a chain. Three is also a chain. At some point you stop wanting to think about which effect sees which one's output and start wanting to reason about the *whole composition* as a unit you can name, scope, and reuse.

A dynamic is exactly that: a higher-order operator that takes a list of effects and produces a single composite effect by composing them. It's the framework's binary operator promoted to a first-class noun.

There are two natural ways to compose a list:

- **Sequential** — each effect sees the prior's output. Monadic in the strict sense; this is the chain-of-thought topology.
- **Parallel** — all effects launch against the same input snapshot. Applicative rather than monadic (no effect depends on a sibling). This is the tree-of-thought topology.

A dynamic is the same structure either way; only the composition strategy changes. The implicit root of every orchestration is a dynamic. So is the body of a loop, each branch of a conditional, and the inner of a reflector. Once you see dynamics as the composition operator, the rest of the language collapses into one idea reused at every scale.

### 4. Loops and conditionals — closing the loop

Everything to this point is *open-loop*: the orchestration is a static directed graph. State flows forward, decisions were made at authoring time, and the runtime just executes them. A long chain or a wide tree can't surprise itself.

What turns an open-loop pipeline into a *closed-loop* control system is two ideas: **branching on observed state** and **iterating on observed state**.

A conditional is a predicate over state plus two continuations. The predicate reads what has happened and decides which path the system takes next. The deterministic case (a rule-based router) is the boring case; the interesting case is the model-evaluated predicate, where the LLM itself is the sensor reading the system's state and reporting back. The model becomes part of the control loop.

A loop is a continuation predicate plus a body. After every iteration the predicate re-reads the now-mutated state and decides whether to keep going. Each iteration's output is the next iteration's input. The loop terminates because the predicate says "done" or because a deliberate iteration cap fires — a floor against runaway feedback.

This is the cybernetic claim made literal. A monitor (the predicate) compares observed state to a desired condition. A controller (the branch or the iteration) adjusts behaviour. The orchestration is no longer a program that runs once; it's a system that observes itself running and steers itself toward an attractor. The framework is named for this.

### 5. Tools, adapters, and runtime plugins — the rest of the world

Once a closed-loop control system exists, the remaining design space is what the system can actually do at each step.

**Adapters** are how a prompt effect leaves the process. They're substitutable behind one Protocol — a `generate(model, prompt) → result` interface. An orchestration written for Ollama runs on OpenAI or Anthropic with no edits, because the orchestration doesn't know which provider it's talking to. Portability across the LLM market is a property the abstraction gives away for free.

**Tool plugins** generalise the prompt. A computation whose result isn't a token stream but a side effect, an external observation, or a piece of structured data — reading a file, fetching a URL, computing an embedding, querying SQL, sending a Slack message — each fits the same monadic shape, so they participate in the orchestration's control flow indistinguishably from prompts. The framework doesn't care whether a node is "talking to a model" or "running ffmpeg"; both write a value to a deterministic state path and become composable.

**Runtime plugins** are the system's introspection. They observe the closed loop without participating in its control flow — `on_run_start`, `on_effect_complete`, `on_run_success | failure` are pure observers. Persistence, pub/sub, observability — all subscribe to lifecycle events, do something side-effectful, and change nothing about the orchestration's semantics.

Adapters carry the conversation. Tool plugins do work. Runtime plugins record what happened. Layered on the closed-loop core, these turn the framework from a clever YAML language into a production substrate — but the conceptual core is still just five abstractions, composed.

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
cof list                          # bundled orchestrations
cof list --extensions             # compiled-in adapters / tool plugins / runtime plugins
cof info recipes/article_summarizer
cof info recipes/article_summarizer --json
cof eject recipes/comic_strip
cof eject recipes/comic_strip --out my-comic.yml

# Validate an orchestration
cof check recipes/hello.yml
cof check recipes/hello.yml --json
cof check recipes/hello.yml --skip-preflight  # structure only; skip env-readiness

# Skip the dependency preflight at run time too (advanced)
cof run recipes/comic_strip --skip-preflight

# Generate an orchestration from natural language
cof gen blog_pipeline "Build a pipeline that drafts, critiques, and revises a blog post"
cof gen summarizer "Summarize a PDF" --format toon

# System diagnostics and setup
cof setup              # Interactive backend detection + config wizard
cof setup --json       # Non-interactive detection output
cof doctor             # Per-extension preflight + config check; non-zero exit when anything fails
cof doctor --generate  # Also test live model connectivity

# Project setup
cof init

# Version
cof version
```

### Auto-Pipe Detection

When stdout is not a TTY (e.g., piped to `jq`), `cof run` automatically switches to `--json` mode with quiet output. `--tail` overrides this when you want just the raw value.

## Run from a Claude conversation

Circuitry ships an MCP server (`circuitry-mcp`) so you can drive an orchestration from a Claude Code (or Claude Desktop) chat. The host Claude session itself becomes the LLM — every `prompt` effect pauses the run, surfaces the rendered prompt as a tool result, and waits for the assistant's response. Tool effects (ffmpeg, ComfyUI, etc.) still execute server-side; only LLM prompts cross the wire.

```bash
# Verify the server is installed (`pipx install circuitry-cof` provides it).
circuitry-mcp --help

# Same entrypoint via the main CLI:
cof mcp --help
```

Wire it into Claude Code via `.mcp.json`:

```json
{
  "mcpServers": {
    "circuitry": { "command": "circuitry-mcp" }
  }
}
```

Then in chat, the tool loop is: `list_orchestrations()` → `run_orchestration(name, …)` → respond to each entry in `pending_prompts` via `submit_response(run_id, prompt_id, …)` until `status` is `completed`. Parallel `flow: tree` orchestrations and parallel loop iterations work uniformly — `pending_prompts` simply contains N entries instead of one. To exercise an orchestration that pins a non-Claude `model:` (e.g. for testing) without changing the YAML, pass `override_model=True` to `run_orchestration`. See [`.claude/commands/cof.md`](.claude/commands/cof.md) for the `/cof` slash command and full tool-loop reference.

The plain `cof run …` CLI continues to work as-is. The MCP server is a strict addition — no existing behavior changes.

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

### Allowlists

By default every compiled-in adapter / tool plugin / runtime plugin is available. Production deployments can restrict that surface by setting allowlists either via env (`CIRCUITRY_ENABLED_ADAPTERS`, `CIRCUITRY_ENABLED_PLUGINS`, `CIRCUITRY_ENABLED_TOOLS` — comma-separated) or via top-level keys in config.json:

```json
{
  "default_adapter": "ollama",
  "enabled_adapters": ["ollama", "openai"],
  "enabled_tools": ["http", "fs", "json"],
  "enabled_plugins": ["circuitry.runtime_plugins.sqlite"],
  "environment": "prod"
}
```

`null` (or unset) means default-open. `[]` locks the category down. A non-empty list is strict allowlist. Validation rejects orchestrations that reference disallowed extensions before any LLM call. The `environment` field flips defaults that vary between dev/prod/test (e.g. SQL persistence plugins skip storing raw provider responses in prod).

### Preflight

Each extension declares a `check()` that reports its dependencies (env vars, binaries, library imports, host reachability). At run time, validation invokes `check()` on every referenced extension and aborts with an actionable error before the first LLM call:

```
$ cof run recipes/comic_strip
Preflight failed: tool:ffmpeg: not ready — missing ['binary:ffmpeg'].
Re-run with --skip-preflight to bypass.
```

`cof doctor` runs the full per-extension preflight without executing an orchestration.

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
  ref: utilities/critique     # curation-library lookup
  inputs:
    article_text: "{{prime.fetch.value}}"
    max_words: 50
  outputs:
    summary: prime.summarize.value
```

State is fully isolated — the child orchestration runs in its own store. Only mapped inputs are passed in, only mapped outputs are extracted.

Reference fields:
- **`ref:`** — curation-library lookup, slash-delimited (`utilities/critique`, `recipes/article_summarizer`).
- **`path:`** — filesystem path. Absolute, cwd-relative, or parent-orchestration-relative.
- **`inline:`** — Mustache template that renders to orchestration YAML at runtime (for LLM-generated plans).

Plus:
- **`validate:`** — schema validation of inline YAML before execution (default `true`).
- **`on_error:`** — `fail` (default), `skip`, or `continue`.
- Works inside loops, conditionals, and dynamics.
- Cycle detection: validation rejects orchestration graphs that recurse on themselves.

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

When neither `outputs:` nor a child `interface:` declares outputs, the entire child `prime` subtree is exposed at `prime.<use_name>.<child_effect>.value` (full-namespace mode).

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

69 built-in providers, organised by purpose:

| Group | Providers |
| --- | --- |
| Time / utility | `clock`, `math`, `regex`, `json`, `uuid`, `hash`, `base64`, `hex` |
| Filesystem / data | `fs`, `csv`, `tar`, `zip`, `gzip` |
| Internet | `http`, `web_search`, `web_fetch`, `webhook`, `wikipedia`, `rss`, `weather` |
| Browser automation | `playwright`, `screenshot` |
| Communication | `email_smtp`, `slack`, `discord` |
| Productivity SaaS | `github`, `jira`, `linear`, `notion`, `gcalendar`, `gdrive` |
| Storage / cloud | `s3` (tool variant) |
| Audio / image / video | `ffmpeg`, `comfyui`, `imagemagick`, `exiftool`, `ocr`, `yt_dlp` |
| PDF / docs | `pdf_extract`, `pdf_render`, `pandoc`, `mediainfo` |
| Embeddings / RAG | `embed`, `rerank`, `vector_search` |
| Code / dev | `git`, `ripgrep`, `pytest`, `linter`, `gh`, `docker`, `kubectl` |
| Sandboxed exec | `python_eval`, `shell` (allowlist-gated) |
| Text processing | `awk`, `sed`, `diff_patch` |
| Network | `dns`, `whois`, `ping`, `traceroute`, `port_check` |
| System info | `system_info`, `process_list`, `env_vars` |
| Crypto / encoding | `gpg` |
| XML / HTML | `xml`, `html_extract` |
| Archives / containers | `7z` |

Plugins that depend on optional PyPI packages or external binaries lazy-import. `cof doctor` reports each one's readiness; `pip install circuitry-cof[<plugin>]` (e.g. `[playwright]`, `[github]`, `[embed]`) pulls the deps the plugin needs.

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

Circuitry ships with ready-to-run orchestrations under `src/circuitry/curation/`, organised by purpose. Browse with `cof list`, inspect with `cof info`, and eject with `cof eject`.

```bash
cof list                                   # everything
cof list --category recipes                # filter to a category
cof info learn/hello                       # details + source preview
cof eject recipes/article_summarizer       # copy to local for editing
```

Categories (slash-delimited; the prefix is the directory):

- **`learn/`** — single-primitive demonstrations, one concept per file (good first read).
- **`utilities/`** — composable, single-output orchestrations meant to be called from other orchestrations via `use:` (e.g. `utilities/critique`, `utilities/summarize`).
- **`patterns/`** — multi-primitive composition templates (kitchen-sink, critique → refine, parallel → judge, classify → route).
- **`recipes/`** — full real-world workflows (`recipes/article_summarizer`, `recipes/comic_strip`).
- **`agents/`** — orchestrations that build or improve other orchestrations (`agents/improver`, `agents/meta_orchestrator`).

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
    Compiler ──> Definition Objects ──> Allowlist + Preflight Gates
       |
    Runtime ──> Effect Execution ──> State Writes ──> on_effect_complete hooks
       |              |
    Store        Adapter Layer ──> Model Provider
  (feedback)        Tool Plugins ──> external systems
                    Runtime Plugins ──> persistence / observability
```

- **Compiler** — parses YAML into typed definition objects, validates against a Draft-07 JSON Schema.
- **Allowlist + Preflight** — gates referenced extensions before any LLM call: allowlists enforce the per-environment surface; preflight invokes each extension's `check()` to verify deps / binaries / env / endpoints.
- **Runtime** — executes definitions, manages state feedback between effects, fires lifecycle hooks (`on_run_start` / `on_effect_complete` / `on_run_success|failure`).
- **Store** — hierarchical state with deterministic path resolution.
- **Adapters** — 24 in-tree, behind one `Adapter` Protocol. Major SaaS LLMs, self-hosted servers (vllm, llama.cpp, LM Studio), aggregator routes (openrouter, litellm), and the MCP `host_claude` adapter that lets a host Claude session drive an orchestration.
- **Tool plugins** — 69 in-tree, behind one `ToolPlugin` Protocol. Stdlib utilities, subprocess wrappers, SDK-driven integrations, and sandboxed `python_eval` / `shell`.
- **Runtime plugins** — 30 in-tree, behind one `RuntimePlugin` Protocol. SQL persistence (B-prime schema across 7 dialects), document / KV / object stores, append-log, pub/sub, observability (OpenTelemetry, Sentry, Datadog, Honeycomb, Prometheus, Loki, CloudWatch).

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
