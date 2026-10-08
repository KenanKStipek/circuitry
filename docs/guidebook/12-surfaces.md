# Surfaces — CLI, TUI, SDK, MCP, and observability

Everything the world reaches Circuitry through. One runtime sits under all of them — the CLI, the terminal UI, the Python SDK, the MCP server, and the REST trigger all call the same `run` path, so an orchestration behaves identically whichever door it came in by (except for the host settings in its `runtime:` block: a file you name by path applies them, a document a library, tool or network caller hands in does not — see [Configuration](04-configuration.md#what-a-document-can-set)) — and one record comes out of all of them: the state graph, which is the complete decision record of the run.

## The CLI: `cof`

| Command | What it does |
| --- | --- |
| `cof setup` | Interactive first-run: detect local backends, pick a model, write the global config. |
| `cof init` | Write a project config and a `hello.yml` in the current directory, and trust the config. `--yes` skips every prompt (scripted use). |
| `cof trust [path]` | Show what a project config sets, flag the host-sensitive settings, and — after you confirm (`--yes` to skip) — trust it so runs apply it. `--list` shows every trusted file and whether it still matches. A `.yml`/`.yaml` PATH instead shows the capabilities an orchestration document needs and consents to them (#275). See [Configuration](04-configuration.md#trusting-a-project-config) and [Capability consent](04-configuration.md#capability-consent-for-a-fetched-or-referenced-document). |
| `cof untrust [path]` | Stop applying a project config you trusted. |
| `cof doctor` | Run every extension's preflight `check()` and report, including the project config and its trust state; `--generate` also makes a live model call. Non-zero exit when anything fails. |
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
| `cof fetch` / `cof run-library` | Retrieve and run a shared-library asset by id and version. `cof run-library` (and a `use: ref:` child of any document) asks before a first run whose tool effects shell out, evaluate Python, write/delete a file, or reach the network — `--allow-capabilities` for a scripted/CI run. See [Capability consent](04-configuration.md#capability-consent-for-a-fetched-or-referenced-document). |
| `cof mcp` | Run the MCP server on stdio (also `circuitry-mcp`). |
| `cof tui` | Launch the terminal UI (needs the `tui` extra). |
| `cof version` | Also `cof --version`, at the root command. |

### `cof run`

```bash
cof run learn/hello -e name=World                      # inline inputs, repeatable
cof run triage.yml --state issue.json                   # inputs from a file
cof run triage.yml -e issue="parse_duration fails on 1h30m" --tail   # print only the final effect's value
cof run triage.yml --json | jq '.prime.reply.value'     # machine-readable state (automatic when piped)
cof run triage.yml --out state.json --pretty            # write the finished state
cof run issue_to_pr.yml --live-state agent.live.json    # mirror the state while it runs (≤ every 0.5 s, and at the end)
cof run issue_to_pr.yml --events agent.events.jsonl      # a JSONL stream of effect starts/ends, written as they happen
cof run issue_to_pr.yml --dry-run                       # no model calls; rendered prompts and shapes
cof run issue_to_pr.yml --verbose                       # a start and a result line per effect, pass tags, timing
cof run --last                                          # re-run the previous invocation
cof run issue_to_pr.yml --profile local                 # apply profiles/local.yml
cof run issue_to_pr.yml --profile-from-state runs/local.json  # reconstruct the profile a past run recorded
cof run triage.yml --adapter ollama --model llama3.2    # the run default, highest-priority layer
cof run issue_to_pr.yml --scoring --routing --explain-routing
cof run issue_to_pr.yml --scoring --decompose --decompose-out ./plans
cof run issue_to_pr.yml --skip-preflight                # run even if a check() reported not-ready
```

When stdout is not a terminal, `cof run` switches to `--json` with quiet output on its own, so it composes with `jq` and with scripts without flags. `--tail` overrides that when you want the raw final value. `--quiet` suppresses prose; `--config <path>` (or `CIRCUITRY_CONFIG`) points at a specific config file.

A `--state` path that doesn't exist is an error (`state file not found: ...`), not empty input, and the document's own `interface.inputs` are checked before anything runs: a missing `required: true` input fails the same way it would as a `use` child's input, a declared `type` is checked (and a `-e`/`--state` string coerced to it), and a `default:` fills in an omitted input. [Composition](09-composition.md#use--the-composition-effect) covers the full contract — it applies the same way here as it does to a `use` child.

`--verbose` prints one start line and one result line per effect. A step inside a loop carries its pass as `[N]`, and a child effect of a `use` is labelled with the `use` that called it. The adapter on a prompt's line is the one the call actually went to, with a failed primary shown before the fallback that answered. The same run also warns when a `while` loop stops at `max_iterations` rather than on its condition. Library warnings — a malformed config, a failed template render, a plugin hook failure — print on stderr at `WARNING`, `INFO` under `--verbose` where a command has it; stdout stays clean for `--json`/`--print`.

A tree branch or a `use` child only ever appears in `--live-state` once it finishes — a still-running one has no node there yet. A running chain leaf shows up there only if a coalesced write happens to land while it runs (writes are at least 0.5 s apart), so most of the time it is invisible too. `--events <file>` (also on `cof run-library`) is the complement: a JSONL stream of `run_start`, `dispatch`, `start`, `end` and `run_end` events, one complete line per event, written the instant each effect dispatches and lands rather than on a 0.5 s timer. It is created (or truncated) before the first effect; a failure to write it is logged once and otherwise ignored and never fails the run — unlike `--live-state`, whose path must be writable up front for its first, synchronous snapshot write. `run_end` is always the last line, written only after `--live-state`'s own final write, and carries the interrupting `signal` (`SIGINT`/`SIGTERM`/`SIGHUP`) when the run was cancelled; a second signal ends the process at once with no `run_end` at all. The orchestration reference's "Watching a Run" section has the full format.

### `cof gen` and `cof wizard`

Two ways to write an orchestration without writing YAML, and they are different artifacts. `cof gen` is single-shot: one goal in, one document out, driven by the `meta_orchestrator` agent with the full authoring ruleset injected. Its output is checked exactly as `cof check` would before anything is written — a model response that fails validation prints the errors and exits non-zero, writing nothing; `--out` names where the generated orchestration lands (default `./<name>.<ext>`), and `--live-state` separately mirrors the generation run's own state JSON while it's in progress. `cof wizard` is a conversation: the `wizard` agent handles one turn at a time — *interpret → ask-or-draft → validate → revise → gate* — and hands back either a question or a validated draft, with `done` true only when it is finished *and* the draft passed validation, so it cannot finish on invalid YAML. A turn whose own orchestration run fails (e.g. a small model returning non-JSON) prints a clean `Error:` line and exits 1, not a traceback. `--reply answers.txt` drives it headlessly. [The Wizard](../wizard.md) documents the turn contract.

## The TUI

`cof tui` is the same runtime with a keyboard: eight views, reachable by number (`3`, Inspect, is still a placeholder screen), every widget focusable without a mouse.

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

Library's `enter` ("run this entry") carries whether the entry resolved from a remote (refreshable, e.g. GitHub) source through to Run, which applies the same capability consent gate (#275) `cof run hub/entry` does — and `Ctrl-R`'s replay in the Runs view carries the same answer from the stashed run it repeats, including a `cof run-library` run: its own stash now records that it came from a remote source too, so replaying it (from the Runs view or `cof run --last`) applies the same gate the original run did instead of skipping it (#337). Neither ever prompts: the TUI refuses the same way a script or CI run of `cof run` does, naming `cof trust <document>`, rather than opening a dialog — a deliberate choice to keep one message and one path to approve a document, not a technical limit.

## The SDK

The SDK is how an agent runs inside another program. A webhook handler for new issues, say, triages each one as it arrives:

```python
from circuitry import run_orchestration

def on_issue_opened(issue: dict) -> str:        # your webhook handler calls this
    result = run_orchestration(
        orchestration_path="triage.yml",
        state={"issue": issue["title"] + "\n\n" + issue["body"]},
        live_state_path="triage.live.json",
    )
    print(result.ok)                            # True / False
    print(result.state["runtime"]["effective_settings"]["sources"])
    return result.state["prime"]["reply"]["value"]
```

`run_orchestration` reuses the CLI runtime path deliberately, so behaviour is identical across interfaces. `state` is wrapped under `input` for you. `dry_run=True` does what `--dry-run` does; `validate_only=True` runs the same checks `cof check` runs by default — structural check, compile, preflight — and stops there: `ok` is `False` for any document `cof check` rejects, never `True` before those checks have run. `out_path` writes the state; `raise_on_error=True` (the default) raises `CircuitryExecutionError` — which carries the failed `RunResult` as `.result`, so the partial state is never lost — and `False` returns the result with `ok` false instead.

`run_orchestration` trusts the file you pass the way `cof run ./file.yml` does: its whole `runtime:` block and `plugins:` list apply, with a notice in `result.warnings` when they include host settings. Pass `trust_document=False` for a path you did not choose — a fetched or generated file, or one a caller of your code named — and the document is limited to `runtime.complexity` and `runtime.state`. `run_shared_orchestration` is always limited. `trust_document=True` (the default) is still overridden back to limited when the path itself resolves inside a configured library source's own cache directory — the same fetched content a library-name run already limits, regardless of what the caller believes about a path it did not actually pick itself (#343).

A `use: ref:` child reached from either call — and `run_shared_orchestration`'s own fetched asset — goes through capability consent (#275) regardless of `trust_document`: refused unless its digest was already consented via `cof trust`, the same message `cof run` gives. Both calls take `allow_capabilities=["shell", ...]` to pre-approve that one call — the embedding program's own consent, since it is the host deciding what to run; never persisted, never read from the document or its input. See [Capability consent](04-configuration.md#capability-consent-for-a-fetched-or-referenced-document).

The `adapter=` parameter is the seam: pass an already-constructed object implementing the `Adapter` protocol (`generate(model=, prompt=, timeout_seconds=)`) and the run uses it instead of the one config resolves, with preflight skipped. It is how a host supplies its own model transport, and how a test scripts one.

The rest of the public surface: `validate_orchestration(orchestration_path=)` returns `{ok, errors, warnings}`; `inspect_orchestration` returns static metadata; `inspect_divergence_paths(state=)` walks a finished state and returns every errored node in path order; `run_shared_orchestration` runs a shared-library asset by id and version; `circuitry.adapters.build_adapter` constructs an adapter from config. [API Reference](../api-reference.md) and [Stability](../stability.md) say what is public and how it is versioned.

An embedded caller gets the same two per-effect events a runtime plugin gets (`on_effect_start` / `on_effect_complete`, see below) by writing one: `run_orchestration` composes every configured runtime plugin's hooks on every run, so a plugin is the public way to observe effects from Python, not a `run_orchestration` keyword.

## The MCP server: a Claude session as the model

`circuitry-mcp` (also `cof mcp`) is an MCP server that lets a Claude Code or Claude Desktop chat *drive* an orchestration — with the host session as the LLM. Each `prompt` effect pauses the run and hands the rendered prompt to the assistant; the assistant's reply becomes the effect's value; tool effects still execute server-side.

```json
{
  "mcpServers": {
    "circuitry": {"command": "circuitry-mcp"}
  }
}
```

The tool loop: `list_orchestrations()` → `run_orchestration(orchestration, initial_state)` returns `{run_id, status, pending_prompts, state}` → for each entry in `pending_prompts`, `submit_response(run_id, prompt_id, response)` → repeat until `status` is `completed`, `failed`, or `cancelled`. `pending_prompts` is always a list: length one for a sequential run, length *N* for a `flow: tree` or a parallel loop — parallel branches simply arrive together, and submission order does not matter. Both `run_orchestration`'s initial snapshot and the one `submit_response` returns are settled on a real signal, not a fixed debounce window: a `flow: tree`/parallel-loop dispatch tells the run manager exactly how many branches to expect before any of them start, and a `submit_response` call waits specifically for the prompt it just answered to leave `pending_prompts`, so a slow-to-schedule worker thread can never show up as an empty, partial, or stale snapshot. `get_run_state(run_id)` snapshots mid-run; `cancel_run(run_id)` wakes every blocked branch. `validate_orchestration(path)` is there too, and resolves the same host config `run_orchestration` would use for the same document, so "valid" here reliably predicts "runnable" instead of skipping the allowlist/preflight checks a real run applies. The model picks the path, so an MCP run or validation is limited like a library document: only `runtime.complexity` and `runtime.state` apply, and the response's `warnings` name what was ignored. Every MCP run supplies its own adapter (the host session as the LLM), so preflight skips only the adapter-reachability check for it — tool and library-ref preflight still run.

`run_orchestration`'s `orchestration` argument resolves a library name the same way `cof run` does, including a remote (refreshable, e.g. GitHub) source — and gets the same capability consent gate (#275, #334) as a result: a name that resolves there, or a `use: ref:` child reached from any document, needs its digest already consented via `cof trust <document>` or the run refuses, naming the same command. The same gate applies when the argument is instead the raw, resolved path of that source's own local cache — an MCP caller is not the host, so naming the cached file directly cannot stand in for the host's own by-path trust (#337). The tool has no field a caller can use to grant capabilities itself — MCP never prompts, so there is nothing to grant one with.

Underneath is the `host_claude` adapter, which is why the plain `cof run` is unchanged and the MCP server is a strict addition. The bundled `/cof` slash command (`.claude/commands/cof.md`) teaches the loop to a Claude Code session; a project `.mcp.json` registers the server.

## REST and scheduling

`circuitry.service` carries a minimal REST trigger — `POST /v1/triggers/run` with a `state` payload — and a scheduler, both calling the same runtime. They are building blocks for embedding rather than a deployment; a service that needs them wires them into its own HTTP stack. `RestTriggerService(auth_token=...)` requires a bearer token by default; the embedder must pass `allow_unauthenticated=True` explicitly to run without one, so there is no accidental open trigger from an unset env var. Every request's `orchestration_path` (and `out_path`, if given) is resolved and must stay inside `orchestration_root` (the service's working directory at construction, or an explicit path) — a request naming a path outside it is refused before anything runs. `config=`, left unset, is resolved the same way `cof run` would resolve one for a document under `orchestration_root` — global config, then a trusted project config discovered there, then environment variables — rather than a bare, allowlist-open `CircuitryConfig()`; an embedder that passes an explicit `config` has that win. A REST caller names the document over the network, so it runs limited to `runtime.complexity` and `runtime.state`; a scheduler job's path is the operator's own, so it is trusted like `cof run ./file.yml` unless the job sets `trust_document=False` — or unless that path itself resolves inside a configured library source's own cache directory, which stays limited regardless (#343). `orchestration_path` is always a file under `orchestration_root`, never a library name, so the top-level document is a run by path (#284) regardless — but a `use: ref:` child it reaches still goes through capability consent (#275) independently, and the payload has no field a caller can use to grant capabilities; an unconsented child refuses, naming `cof trust <document>`.

## Observability

The run record *is* the observability. Three layers, from cheapest to most durable:

**The shadow state file.** `--out` at the end, `--live-state` as it happens, `--json` on stdout. Every effect's `value`, every `meta` — adapter, model, `model_reason`, `prompt_sent`, tokens, timing, error, fallback chain, complexity score and band, decomposition record — and `runtime.effective_settings.sources` saying where every setting came from. A loop's `last` is stored once, as a `{"$ref": "iter_<N>"}` to the pass it aliases ([Shadow state](03-state.md)). The TUI's Runs view is a browser for exactly this file, and it links those references back.

**Lifecycle hooks.** Runtime plugins subscribe to `on_run_start`, `on_run_success`, `on_run_failure`, and the balanced per-effect pair `on_effect_start` / `on_effect_complete`. `on_effect_start` fires immediately *before* dispatch, carrying the node as it stands — the resolved model, the rendered prompt, the score — which is where a routing decision is observed at the moment it is made; `on_effect_complete` fires once `value` is final, including on failure. A `use` child's effects fire inside their parent's pair at namespaced paths. Plugins observe; they never change control flow, and a plugin that raises is recorded under `runtime.plugins.events` and isolated from the run. `cof run --events <file>` writes this same pair (plus `run_start`/`dispatch`/`run_end`) to a JSONL file, for a watcher outside the process rather than an in-process plugin.

**Exporters.** Thirty-odd runtime plugins ship in-tree: OpenTelemetry, Sentry, Datadog, Honeycomb, Prometheus, Loki, CloudWatch, and the persistence backends — [Tools and persistence](13-tools-and-persistence.md) lists them. They are registered in config, not in documents.

## Anti-patterns

**Parsing prose output.** `cof run` prints for humans on a terminal and JSON when piped. Scripts read `--json` or `--tail`; nothing else is stable.

**Reaching into `runtime.*` keys that are not documented as public.** [Stability](../stability.md) names the state paths you can rely on.

**Driving the MCP loop one prompt at a time when `pending_prompts` has three.** They are parallel branches; answer them all, in any order.

## See also

- [Terminal UI](../tui.md) · [The Wizard](../wizard.md) · [API Reference](../api-reference.md) · [Stability & Versioning](../stability.md).
- [`.claude/commands/cof.md`](../../.claude/commands/cof.md) — the MCP tool loop, as a slash command.
