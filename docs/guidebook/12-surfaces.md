# Surfaces — CLI, TUI, SDK, MCP, and observability

Everything the world reaches Circuitry through. One runtime sits under all of them — the CLI, the terminal UI, the Python SDK, the MCP server, and the REST trigger all call the same `run` path, so an orchestration behaves identically whichever door it came in by — and one record comes out of all of them: the state graph, which is the complete decision record of the run.

## The CLI: `cof`

| Command | What it does |
| --- | --- |
| `cof setup` | Interactive first-run: detect local backends, pick a model, write the global config. |
| `cof init` | Write a project config and a `hello.yml` in the current directory. |
| `cof doctor` | Run every extension's preflight `check()` and report; `--generate` also makes a live model call. Non-zero exit when anything fails. |
| `cof run <name-or-path>` | Execute an orchestration. Library entries by slash name (`learn/hello`), files by path. |
| `cof check <path>` | Validate against the schema and the static rules, then preflight the extensions it references. `validate` is the same command. `--skip-preflight` for structure only. |
| `cof score <name-or-path>` | Static per-effect complexity preview; no model calls. |
| `cof list` | Browse the library. `--extensions` lists compiled-in adapters, tool plugins, and runtime plugins; `--models <adapter>` asks an adapter what it can serve. |
| `cof info <name>` | An entry's description, interface, and resolved complexity settings with provenance. |
| `cof eject <name>` | Copy a library entry into the working directory for editing. |
| `cof inspect <path>` | Static metadata: effect counts and types, declared runtime hints. |
| `cof gen <name> "<goal>"` | Generate an orchestration from a sentence, single-shot, via `agents/meta_orchestrator`. |
| `cof wizard --goal "…"` | Build one by conversation — clarifying questions, then a validated draft — via `agents/wizard`. |
| `cof library refresh <source>` | Fetch a remote library source into the local cache. The only library command that touches the network. |
| `cof fetch` / `cof run-library` | Retrieve and run a shared-library asset by id and version. |
| `cof mcp` | Run the MCP server on stdio (also `circuitry-mcp`). |
| `cof tui` | Launch the terminal UI (needs the `tui` extra). |
| `cof version` | |

### `cof run`

```bash
cof run learn/hello -e name=World                      # inline inputs, repeatable
cof run dinner.yml --state inputs.json                  # inputs from a file
cof run dinner.yml -e occasion=anniversary --tail       # print only the final effect's value
cof run dinner.yml --json | jq '.prime.plate.value'     # machine-readable state (automatic when piped)
cof run dinner.yml --out state.json --pretty            # write the finished state
cof run dinner.yml --live-state dinner.live.json        # rewrite the state file after every effect
cof run dinner.yml --dry-run                            # no model calls; rendered prompts and shapes
cof run dinner.yml --verbose                            # per-effect progress, tokens, timing
cof run --last                                          # re-run the previous invocation
cof run dinner.yml --profile fast                       # apply profiles/fast.yml
cof run dinner.yml --profile-from-state runs/fast.json  # reconstruct the profile a past run recorded
cof run dinner.yml --adapter ollama --model llama3.2    # the run default, highest-priority layer
cof run dinner.yml --scoring --routing --explain-routing
cof run dinner.yml --scoring --decompose --decompose-out ./plans
cof run dinner.yml --skip-preflight                     # run even if a check() reported not-ready
```

When stdout is not a terminal, `cof run` switches to `--json` with quiet output on its own, so it composes with `jq` and with scripts without flags. `--tail` overrides that when you want the raw final value. `--quiet` suppresses prose; `--config <path>` (or `CIRCUITRY_CONFIG`) points at a specific config file.

### `cof gen` and `cof wizard`

Two ways to write an orchestration without writing YAML, and they are different artifacts. `cof gen` is single-shot: one goal in, one document out, driven by the `meta_orchestrator` agent with the full authoring ruleset injected. `cof wizard` is a conversation: the `wizard` agent handles one turn at a time — *interpret → ask-or-draft → validate → revise → gate* — and hands back either a question or a validated draft, with `done` true only when it is finished *and* the draft passed validation, so it cannot finish on invalid YAML. `--reply answers.txt` drives it headlessly. [The Wizard](../wizard.md) documents the turn contract.

## The TUI

`cof tui` is the same runtime with a keyboard: nine views, reachable by number, every widget focusable without a mouse.

| Key | View | |
| --- | --- | --- |
| `1` | Library | browse entries, filter, read an interface, launch |
| `2` | Run | the launch form — inputs, profile, adapter and model, the three complexity switches as tri-state dropdowns — then live progress with a complexity column |
| `4` | Runs | the state inspector: a tree of every node in a finished run, values and `meta` side by side |
| `5` / `6` | Doctor / Settings | preflight results; the resolved config and where each value came from |
| `7` | Validate | `cof check` with the warnings channel visible |
| `8` | Chat | the wizard, conversationally |
| `9` | Profiles | edit a named profile: the effect tree with a picker and a toggle per row |

`?` shows the help overlay; `q` goes back, then quits; `Ctrl-C` quits from anywhere. [Terminal UI](../tui.md) is the full tour.

## The SDK

```python
from circuitry import run_orchestration

result = run_orchestration(
    orchestration_path="dinner.yml",
    state={"occasion": "anniversary", "diet": "vegetarian"},
    live_state_path="dinner.live.json",
)
print(result.ok)                          # True / False
print(result.state["prime"]["plate"]["value"])
print(result.state["runtime"]["effective_settings"]["sources"])
```

`run_orchestration` reuses the CLI runtime path deliberately, so behaviour is identical across interfaces. `state` is wrapped under `input` for you. `dry_run=True` and `validate_only=True` do what the flags do; `out_path` writes the state; `raise_on_error=True` (the default) raises `CircuitryExecutionError` — which carries the failed `RunResult` as `.result`, so the partial state is never lost — and `False` returns the result with `ok` false instead.

The `adapter=` parameter is the seam: pass an already-constructed object implementing the `Adapter` protocol (`generate(model=, prompt=, timeout_seconds=)`) and the run uses it instead of the one config resolves, with preflight skipped. It is how a host supplies its own model transport, and how a test scripts one.

The rest of the public surface: `validate_orchestration(orchestration_path=)` returns `{ok, errors, warnings}`; `inspect_orchestration` returns static metadata; `inspect_divergence_paths(state=)` walks a finished state and returns every errored node in path order; `run_shared_orchestration` runs a shared-library asset by id and version; `circuitry.adapters.build_adapter` constructs an adapter from config. [API Reference](../api-reference.md) and [Stability](../stability.md) say what is public and how it is versioned.

Embedded callers also get the two per-effect events without writing a plugin: `RunRequest.effect_start_observer` and `RunRequest.effect_observer` are `(effect_path, effect_node)` callables, composed with any configured plugins.

## The MCP server: a Claude session as the model

`circuitry-mcp` (also `cof mcp`) is an MCP server that lets a Claude Code or Claude Desktop chat *drive* an orchestration — with the host session as the LLM. Each `prompt` effect pauses the run and hands the rendered prompt to the assistant; the assistant's reply becomes the effect's value; tool effects still execute server-side.

```json
{
  "mcpServers": {
    "circuitry": {"command": "circuitry-mcp"}
  }
}
```

The tool loop: `list_orchestrations()` → `run_orchestration(orchestration, initial_state)` returns `{run_id, status, pending_prompts, state}` → for each entry in `pending_prompts`, `submit_response(run_id, prompt_id, response)` → repeat until `status` is `completed`, `failed`, or `cancelled`. `pending_prompts` is always a list: length one for a sequential run, length *N* for a `flow: tree` or a parallel loop — parallel branches simply arrive together, and submission order does not matter. `get_run_state(run_id)` snapshots mid-run; `cancel_run(run_id)` wakes every blocked branch. `validate_orchestration(path)` is there too.

Underneath is the `host_claude` adapter, which is why the plain `cof run` is unchanged and the MCP server is a strict addition. The bundled `/cof` slash command (`.claude/commands/cof.md`) teaches the loop to a Claude Code session; a project `.mcp.json` registers the server.

## REST and scheduling

`circuitry.service` carries a minimal REST trigger — `POST /v1/triggers/run` with a `state` payload, bearer-token gated — and a scheduler, both calling the same runtime. They are building blocks for embedding rather than a deployment; a service that needs them wires them into its own HTTP stack.

## Observability

The run record *is* the observability. Three layers, from cheapest to most durable:

**The state file.** `--out` at the end, `--live-state` as it happens, `--json` on stdout. Every effect's `value`, every `meta` — adapter, model, `model_reason`, `prompt_sent`, tokens, timing, error, fallback chain, complexity score and band, decomposition record — and `runtime.effective_settings.sources` saying where every setting came from. The TUI's Runs view is a browser for exactly this file.

**Lifecycle hooks.** Runtime plugins subscribe to `on_run_start`, `on_run_success`, `on_run_failure`, and the balanced per-effect pair `on_effect_start` / `on_effect_complete`. `on_effect_start` fires immediately *before* dispatch, carrying the node as it stands — the resolved model, the rendered prompt, the score — which is where a routing decision is observed at the moment it is made; `on_effect_complete` fires once `value` is final, including on failure. A `use` child's effects fire inside their parent's pair at namespaced paths. Plugins observe; they never change control flow, and a plugin that raises is recorded under `runtime.plugins.events` and isolated from the run.

**Exporters.** Thirty-odd runtime plugins ship in-tree: OpenTelemetry, Sentry, Datadog, Honeycomb, Prometheus, Loki, CloudWatch, and the persistence backends — [Tools and persistence](13-tools-and-persistence.md) lists them. They are registered in config, not in documents.

## Anti-patterns

**Parsing prose output.** `cof run` prints for humans on a terminal and JSON when piped. Scripts read `--json` or `--tail`; nothing else is stable.

**Reaching into `runtime.*` keys that are not documented as public.** [Stability](../stability.md) names the state paths you can rely on.

**Driving the MCP loop one prompt at a time when `pending_prompts` has three.** They are parallel branches; answer them all, in any order.

## See also

- [Terminal UI](../tui.md) · [The Wizard](../wizard.md) · [API Reference](../api-reference.md) · [Stability & Versioning](../stability.md).
- [`.claude/commands/cof.md`](../../.claude/commands/cof.md) — the MCP tool loop, as a slash command.
