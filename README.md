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

Circuitry builds up from a single prompt to a full feedback control system in five conceptual steps. Reading them in order is the fastest way to understand why the YAML looks the way it does.

### 1. Prompt

The atomic unit. One model invocation, one typed result, written to state at a deterministic path.

```yaml
- type: prompt
  name: greeting
  template: "Say hello to the world."
```

After this runs, the state contains `prime.greeting.value` — the model's response. That's it: that's the smallest valid orchestration.

### 2. State, and composing prompts

The moment you have two prompts, the second wants to read what the first produced. Circuitry's answer: every effect writes to a path derived from its `name`, and templates interpolate from the global state object using Mustache-style `{{path}}`.

```yaml
effects:
  - type: prompt
    name: draft
    template: "Write a short intro for {{topic}}."

  - type: prompt
    name: critique
    template: "Critique this draft: {{prime.draft.value}}"
```

`{{topic}}` came in as user input. `{{prime.draft.value}}` is the first prompt's output. The state object is the wire between effects. **Deterministic paths are the foundation — everything that comes later relies on the next effect being able to address what the previous effect produced.**

### 3. Dynamics — grouping and ordering

Once you have multiple effects, you need scopes (so names don't collide across reusable building blocks) and a way to declare "do these in sequence" vs "do them in parallel". A `dynamic` is both:

```yaml
- type: dynamic
  name: research
  flow: chain
  effects:
    - type: prompt
      name: gather
      template: "List five sources on {{topic}}."
    - type: prompt
      name: summarize
      template: "Summarize them: {{research.gather.value}}"
```

Effects inside the `research` dynamic write to `prime.research.gather.value`, `prime.research.summarize.value` — their state is namespaced.

The `flow:` parameter chooses topology:

- **`chain`** (also `chain_of_thought` / `cot`) — sequential. Each effect runs after the previous one finishes and sees its state.
- **`tree`** (also `tree_of_thought` / `tot`) — parallel. All effects launch simultaneously against the same input snapshot. Use this when downstream effects don't depend on each other.

If your orchestration only needs sequential prompts that each read the prior, you don't strictly need a dynamic — Circuitry wraps the file's top-level `effects:` in an implicit `prime` dynamic. But for any non-trivial topology, dynamics are how you express it.

### 4. Loops and conditionals — this is the cybernetic part

So far the orchestration is a directed graph: prompts feed each other, dynamics group them, `chain`/`tree` decides whether they run in series or parallel. That's pipeline composition — not yet cybernetic.

Cybernetics enters when **the orchestration observes its own state and changes its control flow based on what it observes**. Two primitives unlock that:

**Conditionals** branch on observed state:

```yaml
- type: if
  name: needs_revision
  if:
    mode: model
    template: "Does this draft need more work? {{prime.draft.value}}"
  then:
    - type: prompt
      name: revise
      template: "Revise: {{prime.draft.value}}"
```

`mode: model` lets the LLM read state and decide which branch runs. `mode: cel` does the same with a deterministic CEL expression for cases where you'd rather not pay an LLM for the routing decision.

**Loops** re-evaluate continuation against the freshly-written state:

```yaml
- type: loop
  name: refine
  while:
    mode: model
    template: "Is this good enough? {{prime.refine.draft.value}}"
  max_iterations: 5
  body:
    - type: prompt
      name: draft
      template: "Improve: {{prime.refine.draft.value}}"
```

Each iteration writes a new `prime.refine.iter_<N>.draft.value`; the `while` condition is evaluated against the latest one. The loop exits when the model says "good enough" — or when `max_iterations` triggers (a deliberate floor against runaway feedback).

Together, conditionals and loops turn the orchestration from a static plan into a closed-loop control system. The output of one effect drives the choice of the next. **That's the cybernetic claim**: a monitor (the conditional / loop predicate) compares system state to a desired condition, and a controller (the branch / iteration) adjusts behaviour. The orchestration steers itself.

### 5. Tools, adapters, and runtime plugins — the rest of the world

Once the control plane is in place, the catalog of *what* you can do at each step opens up:

- **Adapters** — how the LLM call leaves the process. 24 in-tree (anthropic, openai, gemini, groq, ollama, openrouter, mistral, cohere, deepseek, xai, perplexity, vllm, llamacpp, lmstudio, …) plus litellm for the long tail. All sit behind the same `Adapter` Protocol so orchestrations stay portable.
- **Tool plugins** — non-LLM side effects. 69 in-tree, from stdlib utilities (`json`, `regex`, `csv`, `hash`, `uuid`) to subprocess wrappers (`git`, `ripgrep`, `pandoc`, `ffmpeg`, `gh`) to SDK-driven (`slack`, `github`, `playwright`, `embed`, `vector_search`). A `tool` effect calls one with a `params` dict and writes the result to state, exactly like a prompt effect.
- **Runtime plugins** — lifecycle hooks for persistence and observability. 30 in-tree (sqlite, postgres, mongodb, redis, s3, kafka, opentelemetry, sentry, datadog, prometheus, …). They observe `on_run_start` / `on_effect_complete` / `on_run_success|failure` to record runs or emit traces, without changing orchestration semantics.

Plus `use` for composing orchestrations from other orchestrations (with state isolation and typed I/O), and `reflector` for letting the model plan the next set of effects at runtime — both built on the same primitives above.

`cof list --extensions` enumerates everything available in your installation; `cof doctor` reports which deps / binaries / env vars each one expects.

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
