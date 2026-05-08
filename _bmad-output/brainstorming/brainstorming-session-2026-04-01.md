---
stepsCompleted: [1, 2, 3, 4]
inputDocuments: []
session_topic: 'Circuitry accessibility, distribution, and developer experience'
session_goals: 'Concrete features, distribution strategy, UX patterns for orchestration discovery and use'
selected_approach: 'progressive-flow'
techniques_used: ['What If Scenarios', 'Cross-Pollination', 'Deep Dive Composition']
ideas_generated: 58
session_active: false
workflow_completed: true
---

# Brainstorming Session Results

**Facilitator:** Kenan
**Date:** 2026-04-01

## Session Overview

**Topic:** Making Circuitry maximally accessible to developers and end-users
**Goals:**
1. Concrete features to build (install UX, orchestration sharing, discoverability)
2. Distribution strategy (GitHub-first, no separate registry infrastructure)
3. UX patterns for finding, running, and composing orchestrations

### Session Setup

**Approach:** Progressive Technique Flow (broad exploration -> pattern recognition -> development -> action)
**Key Constraint:** GitHub as sole source of truth, Kenan as sole orchestration author, Python project with dual install paths (curl|sh and pip/pipx)
**Perceptron:** Web UI, templates, and AI-assisted orchestration building handled separately by the Perceptron project

---

## Complete Idea Inventory

### Theme 1: Zero-Friction Installation

| # | Idea | Description |
|---|------|-------------|
| 1 | **One-Line Bootstrap** | `curl -fsSL https://circuitry.dev/install \| sh` installs CLI, sets up config, prints first-run guidance |
| 2 | **npx-Style Ephemeral Run** | `npx circuitry run summarize-article --url ...` -- no install, temporary runtime, runs and cleans up |
| 3 | **Dual Install Paths** | curl|sh for quick use, pip/pipx for permanent. Shell script detects pipx availability and uses it |
| 5 | **Environment Auto-Detection** | Probes for Ollama, OpenAI key, Anthropic key, ComfyUI, GPU. Generates tailored config.json |

### Theme 2: First-Run Experience

| # | Idea | Description |
|---|------|-------------|
| 4 | **First-Run Wizard** | Interactive setup: detect backends, pick interests, configure .env, list available models, run first orchestration |
| 7 | **Capability-Gated Suggestions** | Filter orchestrations by what's possible with current setup. "Locked" indicators with install hints |
| 15 | **`cof doctor` (expanded)** | Checks Ollama, model availability, API keys, ComfyUI, config validity. Fix suggestions for every issue |

### Theme 3: Orchestration Discovery & CLI UX

| # | Idea | Description |
|---|------|-------------|
| 6 | **Bundled Orchestration Library** | Ship every orchestration from `orchestrations/` as built-in. `cof run article-summarizer` just works |
| 9 | **Orchestration Index File** | `orchestrations/index.yml` with name, description, required backends, input params, category tags |
| 12 | **`cof list` with Rich Table** | Terminal table: name, description, required backends with checkmark/X based on config |
| 13 | **`cof info <name>` Detail View** | Description, required inputs, backends, example usage, expected output, "Run it now?" prompt |
| 14 | **Interactive Input Collection** | Missing required inputs prompt interactively instead of erroring |
| 23 | **`cof examples`** | Categorized real-world use cases with exact runnable commands |
| 25 | **Orchestration README in `cof info`** | Each orchestration has a readme field rendered with Rich in the terminal |

### Theme 4: Distribution from GitHub

| # | Idea | Description |
|---|------|-------------|
| 8 | **GitHub-as-Registry** | `orchestrations/` directory IS the registry. `cof list --remote` fetches from GitHub API |
| 10 | **Version-Pinned Orchestrations** | `cof install` records commit SHA in `.circuitry/installed.json`. `cof update` checks for newer versions |
| 11 | **`cof run github:circuitry/...`** | Fetch-and-run from GitHub without installing. Cached locally after first fetch |
| 43 | **`use` Resolution Chain** | Local path > installed > bundled > remote GitHub. Local overrides beat remote |

### Theme 5: Authoring & Customization

| # | Idea | Description |
|---|------|-------------|
| 16 | **`cof eject <name>`** | Copy orchestration to local file for editing. Like create-react-app eject |
| 17 | **Multi-Format Eject** | `cof eject comic-strip --format yaml\|json\|toon\|python` -- meet users in their native format |
| 18 | **Guided Customization on Eject** | CLI asks "What do you want to customize?" and offers key parameters as prompts |
| 24 | **`cof playground`** | Interactive REPL to build orchestrations step by step without YAML knowledge |

### Theme 6: `use` Composition Primitive (BREAKTHROUGH)

| # | Idea | Description |
|---|------|-------------|
| 26 | **`type: use` primitive** | Run any orchestration as a step. Explicit input/output mapping, state isolation |
| 27 | **`interface` declarations** | Orchestrations declare typed inputs/outputs. Validated at compile time |
| 29 | **Inline orchestration** | `orchestration: { inline: "{{...}}" }` -- LLM-generated plans executed via `use` |
| 30 | **State isolation** | Child orchestration runs in isolated namespace. Clean sandbox, no collisions |
| 31 | **Pipeline composition** | Chain orchestrations with zero prompt writing. Pure wiring |
| 33 | **`on_error` on `use`** | Fault-tolerant composition. Failed step doesn't crash whole pipeline |
| 36 | **`use` inside loops** | Batch processing: feed 50 URLs, run full summarizer on each. `flow: tree` for parallel |
| 37 | **Parallel `use` via tree** | Fan-out: same input, multiple orchestrations concurrently |
| 38 | **Conditional `use` routing** | `if` + `use` = different orchestrations based on state |
| 39 | **`max_concurrency` inherited** | Loop's existing throttle applies to `use` in parallel. Rate-limit batch orchestration |
| 40 | **Multi-value output mapping** | `outputs:` dict extracts multiple values from child orchestration |
| 41 | **Implicit wiring by convention** | If output names match input names, auto-wire. Near-zero-config chaining |
| 42 | **`pass_state: true` escape hatch** | Opt-in full state sharing for power users |
| 44 | **Recursive `use`** | Orchestration uses itself with different inputs. Divide-and-conquer patterns |
| 45 | **`use` replaces `meta_orchestrator`** | Meta-orchestration expressible in the language itself |
| 46 | **Error propagation with context** | Full path in errors: "use 'analyze' -> orchestration 'deep-analysis' -> effect 'extract'" |
| 47 | **Dry-run full expansion** | `--dry-run` shows flattened execution plan across all composed orchestrations |
| 48 | **Per-orchestration timeout** | `timeout_ms` on `use` for SLA enforcement at composition level |

### Theme 7: Reflector v2 -- Decompose into Primitives (BREAKTHROUGH)

| # | Idea | Description |
|---|------|-------------|
| 28 | **Reflector as loop + prompt + `use`** | Kill ReflectorRuntime entirely. ~450 lines replaced by ~15 lines of YAML |
| 32 | **Drop hardcoded prime template** | User-authored reflection prompts. Framework provides loop + execution, not prompt engineering |
| 34 | **Structured output replaces YAML parsing** | `prompt_type: json` + schema. Eliminate `_parse_plan_yaml` and all code fence stripping |
| 35 | **Bundled reflector plan schema** | Standard JSON Schema for plans. Models with structured output guarantee valid plans |
| 45 | **`use` replaces `meta_orchestrator`** | Self-hosting: most complex feature expressible in the language itself |

### Theme 8: IDE Tooling (Language Server & Extensions)

| # | Idea | Description |
|---|------|-------------|
| 49 | **Circuitry Language ID** | Register `circuitry` as a YAML language ID in VS Code/JetBrains. File association for `orchestrations/*.yml` |
| 50 | **Keyword Syntax Highlighting** | Highlight orchestration keywords (`effects`, `type`, `prompt`, `dynamic`, `loop`, `if`, `reflector`, `use`, `flow`, `collect`, `each`, `while`, `prompt_type`, `provider`) distinctly from generic YAML |
| 51 | **State Path Autocomplete** | Context-aware completion for `{{prime.<name>.value}}` references. Parses earlier effects in the file to offer valid state paths |
| 52 | **Self-Referencing State Completion** | Inside a dynamic's effects, autocomplete suggests `prime.<dynamic>.<earlier_effect>.value` based on sibling effects defined above the cursor |
| 53 | **Schema-Driven Validation** | Wire `orchestration.schema.json` into the language server for real-time squiggly-line errors as you type |
| 54 | **Effect Type Snippets** | `cof-prompt`, `cof-loop`, `cof-dynamic`, `cof-if`, `cof-use` snippet expansions with placeholder fields |
| 55 | **Hover Docs** | Hover over a keyword like `collect` or `flow: tree` and see inline documentation pulled from the reference doc |
| 56 | **Go-to-Definition for `use`** | Ctrl+click on a `use` orchestration name to jump to that orchestration file |
| 57 | **State Path Diagnostics** | Warn when a `{{prime.X.value}}` reference points to an effect that doesn't exist or isn't upstream |
| 58 | **Model/Provider Autocomplete** | Suggest known model names and provider values from config.json |

### Parked Ideas (Future)

| # | Idea | Notes |
|---|------|-------|
| *Future* | **Orchestration Registry** | `cof search`, `cof install @user/name`, `cof publish`. GitHub-first for now |
| 24 | **`cof playground`** | Interactive REPL. Lower priority than install/run basics |
| *Perceptron* | **Web UI, templates, AI-assisted building** | Separate project handles this |

---

## Prioritization Results

### P1: Install & Run from GitHub (This Week)

**What:** `curl|sh` install + `cof list` + `cof run <bundled-name>` by name resolution

**Action Steps:**
1. Rework `scripts/install.sh` -- detect Python, prefer pipx, create `~/.circuitry/`, pull config.example.json
2. Create `orchestrations/index.yml` -- catalog every bundled orchestration with metadata
3. Wire `cof list` to enumerate bundled orchestrations from index
4. Wire `cof run <name>` to resolve bundled orchestrations by name (local path first, then bundled)
5. Test end-to-end: fresh machine, curl install, `cof list`, `cof run article-summarizer`

### P2: Onboarding Wizard + .env + Model Discovery (Next)

**What:** First-run wizard that detects backends, creates .env, lists models, suggests orchestrations

**Action Steps:**
1. Backend detection module -- ping Ollama `/api/tags`, check OpenAI/Anthropic env vars, ping ComfyUI
2. Model listing -- Ollama models from API, OpenAI/Anthropic model lists when API keys present
3. `.env` creation -- interactive prompts, write to `~/.circuitry/.env` (global) or local `.env` if one exists
4. Capability matching -- cross-reference index.yml required_backends vs detected backends
5. First-run detection -- check `~/.circuitry/config.json` exists. `cof setup` to re-run manually

**Key Decisions Made:**
- `.env` location: `~/.circuitry/.env` for global, local `.env` override if present
- Model listing: include third-party models (OpenAI, Anthropic) when API key is configured
- Wizard re-runnable via `cof setup`

### P3: `use` Composition Primitive (Design First)

**What:** `type: use` effect with interface declarations, state isolation, and orchestration resolution

**Action Steps:**
1. Write tech spec (use bmad quick-spec workflow)
2. Design state isolation model and interface contract
3. Design inline execution mode (for reflector v2)
4. Implement `UseDefinition`, `UseRuntime`, `_compile_use()`
5. Add resolution chain: local > installed > bundled
6. Update schema, docs, tests

**Blocked on:** Tech spec completion. This is the foundation for reflector v2 and pipeline composition.

---

## Breakthrough Insights

### 1. `use` as Universal Composition Primitive

The single most impactful architectural addition. One new effect type that naturally composes with every existing primitive (loop, if, dynamic, tree) and unlocks: pipeline composition, batch processing, parallel fan-out, conditional routing, AND reflector replacement.

### 2. Reflector Decomposition

`loop` + `prompt(json)` + `use(inline)` replaces the entire `ReflectorRuntime`. Removes ~450 lines of fragile code (YAML parsing, prime templates, plan validation, code fence stripping) and replaces them with ~15 lines of YAML using existing primitives. The framework provides the execution mechanism; the user provides the prompt engineering.

### 3. Onboarding as Product

The first-run wizard isn't just setup -- it's the product's first impression. Detecting backends, listing real models, creating .env, showing what's runnable NOW, and launching a first orchestration in under 60 seconds. This is what makes Circuitry feel like a product rather than a framework.

---

## Session Reflections

**What worked well:** Progressive flow from installation UX (concrete, shippable) into composition architecture (deep, structural). The cross-pollination from npm/Docker/Homebrew/GitHub Actions grounded early ideas in proven patterns. The deep dive on `use` produced a coherent architectural vision rather than scattered features.

**Key creative breakthrough:** The realization that `use` + `interface` + structured output could deprecate ReflectorRuntime entirely. This emerged from asking "what if someone wants to chain two orchestrations" and following the thread to its logical conclusion.

**Creative tension that produced results:** The constraint "GitHub only, single author, no registry" forced simpler solutions (index.yml, bundled orchestrations, name resolution) that are actually better than a premature registry would have been.
