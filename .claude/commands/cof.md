---
name: 'cof'
description: 'Drive a circuitry orchestration from this chat via the circuitry-mcp tool loop.'
---

# cof — drive circuitry orchestrations from this chat

This is a tool-loop integration with the local `circuitry-mcp` MCP server.
You drive an orchestration by calling tools; each `prompt` effect pauses
the run, hands you the rendered prompt, and waits for your response. Tool
effects (ffmpeg, ComfyUI, etc.) execute server-side — you only see the
LLM prompts.

## Setup

Add `circuitry-mcp` to your MCP config (`.mcp.json` for Claude Code):

```json
{
  "mcpServers": {
    "circuitry": { "command": "circuitry-mcp" }
  }
}
```

Verify with `circuitry-mcp --help` (or `cof mcp --help`). Both entrypoints
launch the same stdio server.

## The tool loop

1. **Discover**: `list_orchestrations()` → list of `{name, file, description}`.
2. **(Optional) Validate**: `validate_orchestration(path)` → `{ok, errors}`.
3. **Start**: `run_orchestration(orchestration=<name>, initial_state={...})`
   → returns `{run_id, status, pending_prompts, state, error}`.
4. **Respond**: for each entry in `pending_prompts`, generate a response and
   call `submit_response(run_id, prompt_id, response)`. Submit order does
   not matter for parallel branches.
5. **Repeat** until `status` is `completed`, `failed`, or `cancelled`.

`pending_prompts` is always a list — possibly length 1 (sequential), possibly
N (parallel `flow: tree` or parallel loop iterations). After each
`submit_response`, you may see a fresh batch of prompts from the next stage.
An empty `pending_prompts` with `status: running` means the next stage is
mid-execution (e.g. a tool effect); call `get_run_state(run_id)` to inspect,
or wait — circuitry waits for quiescence before returning.

### Parallel example

Tree-flow with three branches: `pending_prompts` arrives with 3 entries.
Generate one response per branch (in parallel, in a single message if you
prefer), then call `submit_response` three times — once per `prompt_id`.
The order of submission is irrelevant; each response unblocks exactly its
assigned branch.

## Inspecting and cancelling

- `get_run_state(run_id)` — full state snapshot at any point. Useful between
  stages or when surfacing intermediate progress to the user.
- `cancel_run(run_id)` — wakes all blocked branches and joins the worker.

## Model pin

By default, `host_claude` only accepts Claude-family models (or unpinned).
If the orchestration pins a non-Claude `model:` (e.g. `gpt-oss:20b`) and you
still want to drive it from this chat, pass `override_model=True` to
`run_orchestration` — Claude runs the prompts, the original pin is recorded
in the trace as `overridden_from`. Useful for exercising an orchestration
end-to-end before deploying it with its real backend.

```text
run_orchestration(orchestration="…", override_model=True, override_to="claude-opus-4-7")
```

## Bash alternative

Without MCP, the CLI works the same way: `cof run <name> -e key=value`.
Use that when running on a schedule, in CI, or against a non-Claude
backend.

## Worked example

```
list_orchestrations()                                      → [{ name: "learn/hello", … }, …]
run_orchestration(orchestration="learn/hello",
                  initial_state={"name": "World"})         → { run_id, status: "paused",
                                                             pending_prompts: [{prompt_id, prompt: "Say hello to World …"}] }
submit_response(run_id, prompt_id, "Hi World — happy to chat.")
                                                            → { status: "completed",
                                                                state: {prime: {greet: {value: "Hi World — happy to chat."}}} }
```
