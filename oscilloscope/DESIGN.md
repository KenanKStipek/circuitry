# oscilloscope — design

Moved verbatim from issue #418's "Full design" section (the owner's decisions
and the design that followed them), so it lives with the code it describes.

---

# oscilloscope (`osi`): design

`osi` launches an orchestration on either engine (`cof` or electricity) and shows it running. It reads two things: the plan, which is the compiled document, and the run's live output. From these it keeps a status for every effect and writes a readable log. The first front end is a TUI. A GUI can come later and reuse the same core.

```
CYBERDINER_TOKEN=... osi do-thing.yml config.json
```

**Decided by the owner:**

- osi lives in its own top-level folder, `oscilloscope/`, in the Circuitry repository. It is its own Cargo workspace, uses electricity's crates by path, and has its own CI workflow and release jobs.
- Configs are JSON only and are passed to the engine unchanged. The CLI is `osi <orchestration> [config.json]`, and the environment is inherited.

**Sources.** Every statement in §1 was measured unless it is marked *(from code)*. The measurements used `cof` 0.1.0 with the `scripted` adapter: no network and no credentials, a temporary `HOME`, and a 60 s limit per run. Each probe used 1 to 2 s `sleep` steps, and the `--live-state` file was sampled every 0.1 s. Thirteen runs were made, covering:

- chain and tree dynamics (nested);
- named and unnamed `if`, taken and untaken;
- `each` and `while` loops, chain and tree, named and unnamed, with `collect`;
- shell and json tools, text and json prompts, `yield`, and `use` (`path` with full namespace, `path` with declared outputs, and `inline`);
- prompt retries, tool retries, `expect`, `on_error: continue` and `skip`, dynamic and root `finally`;
- a tree branch that fails the run under `stop_on_error`;
- one SIGINT, two SIGINTs, and one SIGTERM.

The p1–p5 runs were repeated with a small probe runtime plugin. It logged every `on_effect_start` and `on_effect_complete` call, with its time and thread. The reflector was not probed.

---

## 1. The live-state file over a run's lifetime (measured)

### 1.1 When the file is written

| Moment | What is in the file |
|---|---|
| First write, 0.4–0.9 s after launch (Python start-up) | `input`, `runtime` (`last_run` with `completed_at: null` and no `totals`; `effective_settings`; `plugins`), `_run_id`, `_timestamp`. There is no `prime` key yet. |
| Later writes | At least 0.5 s apart. *(as measured against `cof` 0.1.0, before issue #634 fixed this.)* Each write followed an effect completing or a `Store.set` call; an effect starting now wakes the mirror too, so a running node is captured reliably, not only by chance (§1.4). Writes are coalesced: p2 (12.6 s) produced 20 distinct snapshots, and p4 (6.4 s) produced 8. |
| Final write | Equal to `--out`. `runtime.last_run.completed_at` and `runtime.last_run.totals` are now set, and `prime.value` is `true` (ok) or `false`. |
| Second SIGINT during cleanup | **No final write and no `--out`.** The file stays at its last mid-run snapshot, still with `completed_at: null`. |

A write serialises the shared state dict at write time, not at the moment of the event that triggered it *(from code: `cli/live_state.py`)*. As a result, a write can include nodes created after its trigger. A running node is now captured reliably rather than only by chance (§1.4).

### 1.2 Fields per effect type

Every node has the shape `{value, meta}`. Every `meta` has `created_at`, `completed_at` (ISO-8601 UTC, `null` while running) and `error` (`null` or a string).

| Effect | `value` | Other `meta` fields seen |
|---|---|---|
| root `prime` / `dynamic` | `null` → `true`; `false` when a failure was swallowed (`on_error: continue`) or failed the run | `adapter` (`"_noop"` when no prompt needs one), `model`, `tokens_sent`/`tokens_received` (always `null`), `flow` (`chain`/`tree`), `dry_run`, `labels`. On failure: `error: "<child path>: <message>"`, e.g. `"guarded.g_fail: /bin/ls failed (exit 1): ls: ..."`. |
| `tool` (shell) | stdout, e.g. `"x\n"` | `provider`, `stdout`, `stderr`, `exit_code`, `waiting_for`, `params_rendered`, `binary`, `raw`. **When the command fails, `exit_code`, `stdout` and `stderr` are `null`.** The exit code appears only inside `error` (`"/bin/ls failed (exit 1): <stderr>"`). |
| `tool` (json) | the parsed value | the same keys; `exit_code: null`, `raw: {mode}` |
| `tool` + failed `expect` | `null` | `error: "expect failed: value == 'yes'"`, `expect: {mode, expr, result: false}`. `stdout` and `exit_code: 0` are kept. |
| `tool` (agent), running | `null` | `provider: "agent"`, and (issue #637, from issue #636's engine side) **`live`**: `v`, `engine`, `model`, `session_id`, `transcript` (set from the first moment), `attempt`, `phase` (`"turn"`/`"repair"`), `started_at`/`updated_at`, `turns`, `tool_calls`, `tools: {name: {calls, ms}}`, `current_tool` (`{name, started_at}` or `null`), `tokens_sent`/`tokens_received`/`cost_usd` (`null` where the CLI doesn't report one live -- Claude Code's `tokens_received`/`cost_usd`), `tokens_cached_in`/`tokens_cache_write`, `context_tokens`/`context_window`, `turn_tokens` (so far), `files_changed`/`lines_added`/`lines_removed`, `earlier_attempts` (from the second attempt on). Published at most once a second. Removed once the attempt ends, except when the run itself was interrupted (see §1.4). |
| `tool` (agent), finished | the reply text, or `result_file`'s parsed JSON | `provider: "agent"`, `raw: {engine, session_id, turns, tool_calls, tokens: {sent, received}, cost, repair_turn, transcript, context_tokens, context_window, tool_counts, turn_tokens, files_changed, lines_added, lines_removed, ...}` (docs/plugins/agent.md's full table) -- **no top-level `model`**, unlike `live`. The ordinary `tokens_sent`/`tokens_received`/`cost_usd` mirror (a `prompt` effect's own fields) carries the same totals as `raw.tokens`/`raw.cost`, so every other screen already reads a finished agent step the same way it reads any other tool. `live` is gone. |
| `prompt` | the text, or the parsed JSON | `adapter`, `model`, `model_reason`, `prompt_type`, **`prompt_sent` (the full rendered prompt)**, `tokens_sent`/`tokens_received`, `tokens_*_total`, `fallback_attempts[{adapter, model, status, error, tokens_*}]`, `fallback_recovered`, `waiting_for`, `dry_run`. Also `retries_used` when it is above 0, and `finish_reason` when the provider reports one. |
| `yield` | the rendered text | `dry_run` |
| named `if` | `{result, branch, effects: [{index, type: "ToolDefinition", name}]}`. An untaken branch with nothing in it gives `effects: []`. | `mode`, `threshold`, `labels`, `condition_result`, `branch`. Already set while the branch is still running. |
| named loop | `{iterations, termination: {reason}, effects_by_iteration: [...]}`, `null` until the loop ends | `mode` (`each`, or `cel` for a CEL `while`), `each_in_path`, `each_as`, `min_iterations`, `max_iterations`, `labels`, **`progress: {done, total, elapsed_s, eta_s}`** (updated after each pass), `completed_passes`. Children: `iter_N`, `collected: {value}`, and `last: {"$ref": "iter_N"}`. |
| `use` | `null` (full namespace) or `{<declared outputs>}` | `orchestration`, `inline`, `resolved_path`, `validation_errors`, `child_errors`, `inputs`, `orchestration_sha256` |

These values were measured but were not what a reader would expect:

- **Retry visibility differs between tools and prompts.** *(as measured against `cof` 0.1.0, before issue #421 fixed this upstream — see Q9. Neither `cof` nor electricity's own tool retry loop still behaves this way.)*
  - A retried tool resets `created_at` to the start of its *last* attempt, and on failure it carries **no `retries_used`**.
  - A retried prompt keeps the start of its *first* attempt. `retries_used: 2` appears only on success, and `fallback_attempts` lists only the final attempt.
- **`on_error: skip` and `on_error: continue` give identical nodes** (`value: null` plus `error`). The plan is the only way to tell them apart.
- **Run totals.** `runtime.last_run.totals = {wall_time_s, effects_run, tokens_sent, tokens_received, cost_usd}` appears only in the final write. `effects_run` counts every completed node, the root and the containers included. An interrupted effect is not counted.

### 1.3 How state paths map to the document

| Construct | State path (measured) |
|---|---|
| root | `prime` (a node with its own `meta`) |
| `finally` (root or dynamic) | A sibling under the same container (`prime.root_cleanup`, `prime.guarded.g_cleanup`). There is no separate namespace. |
| nested dynamics | `prime.fan.inner.step1` |
| named `if` | `prime.gate_taken.chosen`. `then` and `else` reuse the same child names. |
| unnamed `if` | no node; the branch writes into the parent (`prime.flat_branch`) |
| named loop pass | `prime.each_chain.iter_0.nap` (zero-indexed), plus `last` (a `$ref`) and `collected` |
| unnamed loop | no node and no pass index. Every pass overwrites `prime.u_nap`. In tree flow, three concurrent passes write the same path. |
| `use` (path or inline, full namespace) | `prime.first.c_nap` (the child's root `prime` is stripped) |
| `use` with declared outputs | children are **dropped** from the final state; only `value: {ans: ...}` remains |
| `if` / `while` decision with `mode: model` | no node of its own; the scripted reply path is the container's own path *(from spec)* |

### 1.4 What does not appear

| Situation | Visible in live state? |
|---|---|
| A running leaf in **chain** flow | *(as measured against `cof` 0.1.0, before issue #634 fixed this.)* Was only by accident: its node (`completed_at: null`) appeared only if a write happened to land while it ran. Seen in p2 (`prime.gate_taken.chosen` during its 1 s sleep). Not seen in p4: no write occurred during `flaky`'s 2 s retry window. Every effect start now wakes the mirror (`Store.fire_effect_start`), at every level, so this is reliable now, within the mirror's usual coalescing interval, the same as the tree/`use` rows above. |
| A running branch or pass in **tree** flow (dynamic or `each`) | *(as measured against `cof` 0.1.0, before issue #634 fixed this.)* Branches ran in isolated stores that republished only once a branch finished, so a leaf appeared only when it completed, and a nested container inside a branch (`prime.fan.inner`) appeared only when the whole container completed; the loop node's `progress.done` was the only live sign. `cof` now republishes a branch's own snapshot at every effect start inside it (and on every write, as any leaf already did), so a running leaf appears there the same way it does at the top level, within the mirror's usual coalescing interval -- the same outcome electricity's own, unchanged mechanism already gave it (electricity-DESIGN.md Q11). |
| A running child of a `use` | *(as measured against `cof` 0.1.0, before issue #634 fixed this.)* The `use` node showed `completed_at: null`, but its children appeared only afterwards, all at once. Both engines now show a running child's own effect the same way, whether the `use` has declared `outputs:` or not -- a declared-outputs child's effects still disappear from the *finished* state once its mapped value lands, exactly as before. |
| A queued branch (under `max_concurrency`) | No. Nothing tells a queued branch from a running one. |
| A retry in progress | Tool: yes, from state alone — `completed_at` going back to `null` (running again) with the same `created_at` as the attempt that just failed (§2.1 rule 6, §2.4). A leaf that *completes* again with the same `created_at` as an earlier failure, with no running sighting in between, gets the same `↻` retroactively alongside its own end line. Prompt: no. |
| A prompt in flight | *(as measured against `cof` 0.1.0, before issue #634 fixed this.)* Was only visible as a chain-flow node; a tree-flow or `use`-child prompt in flight is visible the same way now. `completed_at: null`, and `prompt_sent` is already set. Model, adapter and the rendered prompt are visible. Elapsed time is not. |
| A branch cancelled by `stop_on_error`, or an effect after a failure | No node at all. Hooks do not fire for it either. |
| An effect interrupted by SIGINT/SIGTERM | **It stays `completed_at: null`, `error: null` in the final state.** Its enclosing loop also stays `completed_at: null`. `prime.meta.error` is `"Interrupted (Ctrl-C/SIGINT)"` or `"Interrupted (SIGTERM)"`, and `finally` effects run and appear. |
| A running agent session (issue #637) | Yes, from `meta.live` alone: its tool-call counts, token usage and context size are published again on every stream event the CLI produces, at most once a second, with no `--events` needed -- not a periodic heartbeat of its own, so `updated_at` can still go stale during any quiet stretch longer than 5 seconds (a long tool call) even while the session is genuinely still running. osi treats `meta.live` as *running* only while `updated_at` is within 5 seconds of now; a run interrupted mid-session leaves `meta.live` in place (its one documented exception, above) with `updated_at` no longer moving, so it reads as stopped within a few seconds, never as still running -- its last-known figures stay on screen, under a `stopped` marker, in both the now pane and Flow's selected pane. Tool-call detail (a glyph strip, the clock-timed "doing" list, the last test command's own outcome) comes from the transcript file `meta.live.transcript` names, tailed directly -- only for the one step on screen, only its last 64 KiB, at the usual 100 ms poll; a missing file, a cut-off last line, or a file replaced mid-run all fall back to the summary alone (or, with no transcript tailed at all yet, to `meta.live`'s own `tools`/`current_tool` names and counts). |

**The runtime hooks see more than the live state does** (probe plugin):

- `on_effect_start` fires on the worker thread at the real start time. That includes each tree branch's leaves, in dispatch order: with `max_concurrency: 2`, `iter_2` started only after `iter_0` ended. It also fires for every `use` child.
- It fires for the root, named containers and leaves. It does not fire for unnamed `if` or loop containers.
- On a tool's start, `meta` is still `{}`. On a prompt's start, `meta` already holds `prompt_sent` and `model`.
- When an unnamed loop starts its second pass, the start event carries the *previous* pass's node.
- Start and complete for one effect always arrive on the same thread.
- The hooks fire once per effect, not once per retry.

### 1.5 Snapshot excerpts (synthetic; `runtime` and some meta keys omitted)

**A: chain, t ≈ 1.0 s (p2).** A running leaf caught in-flight, and an `if` decision recorded before its branch ends:

```json
"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
  "items": {"value": ["x", "y", "z"], "meta": {"created_at": "19:55:43.892181Z", "completed_at": "19:55:43.892286Z", "provider": "json", "exit_code": null}},
  "gate_taken": {"value": null,
    "meta": {"completed_at": null, "mode": "cel", "condition_result": true, "branch": "then"},
    "chosen": {"value": null, "meta": {"created_at": "19:55:43.892753Z", "completed_at": null,
      "provider": "shell", "exit_code": null, "params_rendered": {"command": "sleep", "args": ["1"]}}}}}
```

**B: tree `each` loop, `max_concurrency: 2`, t ≈ 7.5 s (p2).** Passes 0 and 1 have finished. Pass 2 is running and is not in the file at all:

```json
"each_tree": {"value": null,
  "meta": {"completed_at": null, "mode": "each", "each_in_path": "prime.items.value",
           "progress": {"done": 2, "total": 3, "elapsed_s": 1.028, "eta_s": 0.514}},
  "iter_0": {"t_nap": {"value": "", "meta": {"completed_at": "19:55:50.249076Z", "exit_code": 0}},
             "t_echo": {"value": "x\n", "meta": {"completed_at": "19:55:50.261287Z", "exit_code": 0}}},
  "iter_1": {"t_nap": {"value": "", "meta": {"exit_code": 0}}, "t_echo": {"value": "y\n", "meta": {"exit_code": 0}}}}
```

**C: final state after one SIGINT at 2.6 s (p6, exit 130).** The interrupted pass stays "running" for ever:

```json
"prime": {"value": false, "meta": {"completed_at": "19:56:14.136202Z", "error": "Interrupted (Ctrl-C/SIGINT)"},
  "first": {"value": "", "meta": {"completed_at": "19:56:12.334678Z", "exit_code": 0}},
  "slow": {"value": null, "meta": {"completed_at": null, "progress": {"done": 0, "total": 5, "eta_s": null}},
    "iter_0": {"tick": {"value": null, "meta": {"created_at": "19:56:12.335526Z", "completed_at": null, "error": null}}}},
  "cleanup": {"value": "", "meta": {"created_at": "19:56:13.120701Z", "completed_at": "19:56:14.135968Z", "exit_code": 0}}}
```

---

## 2. Inference rules

Two inputs feed these rules: the plan (§5), and a sequence of observations. An observation is a snapshot, or an event once events exist (§3). **Event time comes from `meta.created_at` and `meta.completed_at`, never from when osi noticed a change.** Log lines are sorted by those times. A short effect that starts and ends between two snapshots still gets its line, in the right place.

### 2.1 Status of a plan node at concrete path `p`

**With events (exact):**

| Condition | Status |
|---|---|
| a `start` is open | **running** |
| `end` with `ok` | **done** |
| `end` with an error and `on_error: fail` in the plan | **failed** |
| `end` with an error and `on_error: skip` or `continue` | **failed (handled)** |
| no `start`, and the run or the parent is still active | **pending** |
| no `start`, and the parent ended | **skipped** (§2.2 gives the reason) |
| a `start` is still open when `run_end` arrives with an interruption | **cancelled** |
| no `run_end` and the process exited | open starts become **aborted** |

**From state alone (best effort):**

1. Node absent:
   - If an ancestor is complete, the node is **skipped**.
   - If the parent is a running **chain**, and every earlier plan sibling is complete, the node is **likely running**. Its node may simply not have been written yet.
   - Otherwise the node is **pending**.
2. Node present with `completed_at: null`: **running**.
   - In the final snapshot, or once the process has exited: **cancelled** if `prime.meta.error` starts with `Interrupted`, otherwise **aborted**.
3. Node present with `completed_at` set: **done** if `error` is `null`. Otherwise **failed**, or **failed (handled)** when `on_error` is `skip` or `continue` (from the plan).
4. A `tree` container that is running, with unfinished children (a dynamic branch or an `each` pass): those children are **running or queued** and cannot be told apart. The count is bounded: at most `max_concurrency` minus the branches already running.
   - A tree `each` without `max_concurrency` defaults to `min(32, cpu + 4)` workers. osi cannot know `cpu`, so it must say "≤ N".
   - The completed count comes from `meta.progress.done`.
   - **With events:** the container's own `dispatch` event gives an exact bound instead of this estimate — `branches` is the true total (so progress reads "`done` of `branches`" instead of the plan's own, possibly data-dependent, count), and `concurrency`, when the stream has it, is the exact running ceiling in place of the `max_concurrency`/`cpu`-guess above.
5. A running `use`: its plan children (from compiling a `path` child) follow rule 1. An `inline` child has no static plan, so its children are discovered from the observations.
6. A retry: a running tool node whose `created_at` moved forward is **retrying (attempt ≥ 2)** — kept for whichever engine/version still moves `created_at` between attempts, though neither cof nor electricity do since issue #421. The current rule: a tool node that was non-running with an error and is running again with the *same* `created_at`, or that completes again with the same `created_at` as an earlier failure, is **retrying** too (§2.4's table). A prompt retry cannot be detected from state either way.
7. Run status (one function, shared by the TUI header, the plain `--log` summary and `osi watch`):
   - `runtime.last_run.completed_at` set means the run **ended**: **ok** if `prime.meta.error` is `null`, **cancelled** if it starts with `"Interrupted"` (a confirmed `q`, a Ctrl-C cancel, or a forwarded SIGINT/SIGTERM/SIGHUP always reach this branch, §4.1), otherwise **failed**.
   - The process exited with no ended state: **failed** only when the exit code is exactly 1 *and* there is a pre-execution failure reason (`cof`'s own stdout JSON, or a `run_end` event's error) -- the shape of a document invalid enough that nothing ever ran (§4.1). Otherwise **aborted** (a second signal, SIGKILL, a crash: no final write). `osi watch`, which owns no process of its own, reads `--events`' own `run_end.ok == false` in place of the exit code.

### 2.2 Why a node was skipped

| Reason | How to tell |
|---|---|
| untaken branch | from the named `if`'s `meta.branch`. An unnamed `if` with both branches naming the same effect cannot be told apart: show the merged node. |
| not reached | an earlier chain sibling failed, and the parent has an `error` |
| cancelled sibling | the parent is a tree with `stop_on_error`, and its `error` names another child |
| disabled | `enabled: false` in the plan (a profile) |
| zero passes | loop `value.iterations == 0` |

### 2.3 Paths that cannot be resolved

- **An unnamed loop's body** writes one path for every pass. Use the order of `created_at` values to number passes in chain flow. In tree flow, concurrent passes overwrite each other, so only events (an `id` per instance, §3) separate them.
- **`last: {"$ref": "iter_N"}`** is an alias: ignore it and use `iter_N`.

### 2.4 Log lines from snapshot diffs (per path, between two consecutive observations)

| Change | Line |
|---|---|
| new node with `completed_at: null` | `▶ <path>  <summary>`. The summary is the tool and its rendered command or args, or the prompt's adapter and model with the first 60 characters of `prompt_sent`. |
| `completed_at` became set | `✓ <path>  <duration>  <result>` or `✗ <path>  <error, first line, 120 chars>  (on_error: continue)` |
| new node that is already complete | its `▶` line (backdated to `created_at`) and its `✓`/`✗` line |
| named `if` gets `meta.branch` | `◆ <path> → then` |
| `meta.progress.done` changed | `⟳ <loop> pass d/total  ETA eta_s` |
| a running node's `created_at` moved | `↻ <path> retry` |
| a non-running, failed node restarts running with the same `created_at` | `↻ <path> retry` |
| a complete node completes again with the same `created_at` | `↻ <path> retry` for the earlier failure, then its own `✓`/`✗` |
| a complete unnamed-loop node's `created_at` moved | `▶`/`✓` lines for the new pass, numbered `#k` |
| `last_run.completed_at` set | `■ run ok / failed: <prime.meta.error>  <totals>` |

A `report` effect (#605) fires none of the above — it has its own log
line, from `--events`' own `report` line (§3), not from a snapshot diff:
`▸ <message>`, or `▸ warning: <message>` for `level: warning`, with a
compact one-line JSON preview of `data` after it (`(data truncated)`
when the engine itself replaced an oversized one) — truncated to fit
the TUI's own log pane width, or to 200 characters in `--log`. The run's
own `run_start` gets a line of its own too, `■ run started  <run_id>`.

Values are never logged in full: one line, with a character cap. The details pane shows the full value on request.

---

## 3. Engine addition: an `--events <file>` JSONL stream (recommended)

**The problem.** Live state cannot answer "what is running now" precisely:

- tree branches and `use` children were invisible while they ran *(as measured against `cof` 0.1.0, before issue #634 fixed this; `cof` now republishes a branch's own snapshot at every effect start inside it, the outcome electricity's own mechanism already gave it)*;
- a leaf is visible only within the mirror's own coalescing interval, never the instant it starts or ends;
- writes are coalesced to every 0.5 s or more;
- each write serialises the whole state.

The runtime already has exactly the right hooks (`on_effect_start`/`on_effect_complete`, §1.4).

**Other options rejected:**

| Option | Why not |
|---|---|
| The `jsonl-file` runtime plugin | It needs a `plugins` entry in config, and osi passes configs unchanged. It has no start event. It reads `runtime_plugins.jsonl-file`, not `runtime.runtime_plugins` *(from code)*, and appends across runs by default. |
| A start marker written into the live state | *(before issue #634.)* Tree branches lived in isolated stores until they merged, so they would still have been invisible. Adding one also changes the state shape, which both engines must reproduce byte for byte. |
| Flushing the mirror on start | Costs a full serialisation per start, for precision `--events` already gives for free. |

**Format.** UTF-8 JSONL, one object per line, every line complete:

```json
{"v":1,"seq":0,"ts":"2026-10-08T19:56:22.433Z","ev":"run_start","run_id":"…","orchestration":"do-thing.yml","engine":"cof 0.2.0","pid":4242}
{"v":1,"seq":7,"ts":"…","ev":"dispatch","path":"prime.each_tree","branches":3,"concurrency":2}
{"v":1,"seq":8,"ts":"…","ev":"start","id":8,"path":"prime.each_tree.iter_0.t_nap"}
{"v":1,"seq":12,"ts":"…","ev":"end","id":8,"path":"prime.each_tree.iter_0.t_nap","ok":true,"ms":1008}
{"v":1,"seq":20,"ts":"…","ev":"end","id":15,"path":"prime.always_fails","ok":false,"ms":5,"error":"/bin/ls failed (exit 1): ls: …"}
{"v":1,"seq":99,"ts":"…","ev":"run_end","ok":false,"error":"Interrupted (Ctrl-C/SIGINT)","signal":"SIGINT"}
```

| Field | Meaning |
|---|---|
| `v` | Format version, `1`. |
| `seq` | Strictly increasing in file order. |
| `ts` | Wall-clock UTC time, in milliseconds. |
| `id` | Unique per effect *instance* (one per pass or branch), so `start` and `end` pair up even when several instances share an unnamed path. |
| `path` | The absolute path, as in state and in scripted-replies keys. |
| `dispatch` | Sent once by a tree loop or tree dynamic before its branches start. This is the existing `concurrent_dispatch` callback. `branches` is the true branch count (an `each` loop's item total, or a `dynamic`'s effect count); `concurrency`, when present, is the ceiling — at most this many run at once. `concurrency` is optional: a stream from a `cof` built before it was added has `branches` alone, and osi still works, just with a less precise bound (DESIGN.md §2.1 rule 4). |
| `error` | Present only when `ok` is false. Its first 500 characters, already redacted. |
| `report` | A `report` effect (#605): `path`/`level`/`message`, then `data` or `data_truncated` — see electricity's own `runtime-semantics.md` §8.7 for the exact shape. Never paired with a `start`/`end` (no `id`, never counted toward `effects_run`); `path` is the report's own state path when it is named, else its enclosing container's. osi's own handling of this line is #610 (§2.4's log line, §6.3's header and details pane). |

What the format leaves out, and why:

- **No values and no prompt text.** osi reads those from the live state.
- **No effect kind.** osi takes it from the plan, or from the shape of `meta` for generated children.

**Rules:**

- **Ordering.**
  - A container's `start` comes before any child's `start`.
  - Every child's `end` comes before the container's `end` (measured: true for the hooks today).
  - Tree branches may interleave.
  - `run_start` is the first line. When `run_end` is written, it is the last line, written after the final live-state write. So when osi sees `run_end`, the final snapshot is already on disk.
- **Flushing.** The file is opened and truncated before the first effect. Each event is written with **one `write()` of one whole line, under one lock, and flushed at once**. There is no buffering across events, so a reader tailing the file never sees a torn line, except at EOF while a write is in progress. Readers keep any trailing partial line until its `\n` arrives.
- **Failures.** A failure to write is logged once and then ignored. It never fails the run, the same as the live mirror.
- **Abort.** No `run_end` is written after `os._exit` (second signal). osi treats EOF with no `run_end` plus a dead process as aborted.

**What `cof` must change:**

- **New `src/circuitry/cli/events.py`.**
  - `EventLog(path)` with `run_start(run_id, …)`, `on_start(path, node)`, `on_complete(path, node)`, `on_dispatch(path, n)`, `run_end(ok, error, signal)` and `close()`.
  - Instance IDs: each `on_start` pushes a new `id` onto a per-thread stack keyed by `path`, and `on_complete` pops it. Start and complete for one effect are always on the same thread (measured), so this pairing is safe.
  - `ok` and `error` come from `node["meta"]["error"]`.
- **`src/circuitry/cli/runtime_shim.py`.**
  - Add `RunRequest.events_path: Path | None`.
  - In `run()`, build the `EventLog` next to `LiveStateMirror`.
  - Append `on_start` to `start_observers` and `on_complete` to `effect_observers`.
  - Compose `on_dispatch` with `req.concurrent_dispatch_observer` into `Store(concurrent_dispatch=…)`.
  - Emit `run_end` in the `finally:` after `live_mirror.close(state)`, taking the signal from `get_token().signum`.
- **`src/circuitry/cli/app.py`.** Add `--events PATH` to `run` and `run-library`, and pass it into `RunRequest`.
- **Optional:** expose `events_path=` on `circuitry.api.run_orchestration`.
- **Tests:**
  - the ordering, ID-pairing and no-torn-line rules, under a tree loop with `max_concurrency`;
  - a SIGINT test (`run_end.signal`);
  - a doc page in `docs/guidebook/12-surfaces.md`;
  - a `changelog.d` fragment.

**What electricity's spec must then require:**

- **CLI.** Add `--events <path>` to the CLI scope (DESIGN.md §1).
- **New spec section.** Add a section to `docs/spec/runtime-semantics.md` with the format and rules above, emitted from the VM's `fire_effect_start`/`fire_effect_complete` (M0-H already plans these).
- **Conformance cases.** Compare the event sequence after normalising `ts`, `ms`, `run_id`, `pid` and `engine`. For a tree, compare the start-before-child and child-ends-before-container partial order, not the exact interleaving.
- **Milestone.** **Land it in M0-H, not M3.** Live state is M3-B, so until M3 electricity's only live channel would be `--events`.
- **Later.** An `effect_retry` event (`{id, attempt, error, backoff_ms}`) would make retries visible. It needs a callback from the prompt and tool retry loops (`core/prompt.py`, `core/tool.py`), so it is left out of v1 (§7, Q4).

---

## 4. Launching each engine

osi makes a run directory and launches the engine with these files in it:

- `state.live.json`
- `state.json`
- `events.jsonl`
- `stdout.txt` and `stderr.txt`

By default the directory is a new temporary one, printed when the run ends; `--out-dir <dir>` chooses it instead. These are the same file names a `cof run --live-state <dir>/state.live.json --out <dir>/state.json` user already has, so `osi watch <dir>` (§7) can attach to such a run.

### 4.1 `cof`

```
cof run <orchestration> [--config <config.json>] --quiet \
    --live-state <dir>/state.live.json --out <dir>/state.json --events <dir>/events.jsonl \
    [-e key=value]...
```

- **Config.** With no config, `--config` is left off, and `cof` resolves its usual layers: the global config, a *trusted* project config in the working directory, and env vars. `--config` replaces the global and project layers and is always trusted.
- **Inputs.** `-e` values are JSON-sniffed by `cof`, so osi passes them through untouched. osi accepts `-e`/`--state` after the positionals; this does not change the owner's CLI form.
- **stdin is `/dev/null`.** Nothing interactive can then hang:
  - capability consent prompts only when stdin *and* stdout are TTYs and neither `--quiet` nor `--json` is set; otherwise it refuses with a message naming `cof trust <doc>` *(from code: `cli/app.py` `_capability_prompt`)*;
  - an untrusted project config is never a prompt, only a one-line stderr warning.
- **stdout is a pipe** (measured with `--quiet --out`):
  - success: stdout is empty;
  - failure, validation errors included: it holds one JSON object, `{"ok": false, "error": "…", "warnings": [], "state_out": "…"}`. osi parses it for the final error line.
- **stderr is a pipe.** It carries `WARNING: …` log lines and `Warning: …` notices. Each line becomes an `engine` line in the log pane.
- **Process group.** osi spawns the engine with `CommandExt::process_group(0)`, so that terminal signals reach osi only and osi alone decides what to forward.
- **Side effect.** `cof run` updates the user's `--last` stash. This is accepted, and documented.

**Signals.** In raw mode, Ctrl-C is a key, so osi forwards signals itself (measured, sent to the engine's process group):

| Action | Engine result |
|---|---|
| first Ctrl-C → SIGINT | exit **130**. The interrupted effect is left open, `finally` runs, and the final snapshot plus `--out` are written (`error: "Interrupted (Ctrl-C/SIGINT)"`). |
| second Ctrl-C (0.24 s later) → SIGINT | exit **130** at once: no `finally`, no `--out`, no final snapshot, and no stdout. |
| SIGTERM to osi → SIGTERM | exit **143**, same as the first SIGINT, with `"Interrupted (SIGTERM)"`. |
| SIGHUP to osi (terminal closed) | forward SIGHUP: exit 129, handled like SIGTERM *(from code: `cli/interrupts.py`)*. |

osi behaviour:

- **First Ctrl-C:** send SIGINT and show "cancelling, finally running…".
- **Second Ctrl-C:** send SIGINT again.
- **Still running 10 s after the second:** send SIGKILL to the group.
- **`q` while a run is going:** ask "cancel run?" first.
- **Exit code:** osi exits with the engine's code.

### 4.2 electricity

**M0-H's run wiring (issue #441) landed electricity's own side of
this; issue #431's lane E2 wires osi to it.** electricity is still a
preview: `electricity/README.md`'s preview section is the single
source of truth for what it runs, and it refuses everything else up
front, before any state is written, with a message naming the preview
marker (`is a preview and can't run this orchestration`,
`electricity_bytecode::PREVIEW_MARKER`). osi shows that refusal the
same way it shows an invalid document: a pre-execution failure,
`■ run failed: ...`, exit 1 -- though only the refusal writes nothing
at all under the run directory; an invalid document still gets its own
`--out` (electricity's `fail!` macro saves state before returning the
error).

- `electricity <config.json> <orchestration.yml> -e k=v... --out
  <dir>/state.json [--events <dir>/events.jsonl] [--live-state
  <dir>/state.live.json]` -- the same argv shape `ElectricityEngine::
  command` builds (`oscilloscope-core/src/engine.rs`), in that order.
- `ElectricityEngine::detect` probes `electricity --help` for both
  `--events` and `--live-state`, the same way `CofEngine::detect`
  probes `cof run --help` for `--events` alone (`cof`'s own
  `--live-state` predates capability detection, so `CofEngine`'s own
  caps hardcode it `true`). An older electricity built before M0-H has
  neither flag, and osi falls back to the no-events path with the same
  notice `cof` gets ("this electricity has no --events: running from
  state only"), worded for whichever engine is actually missing it.
- Exit codes are exactly `cof`'s own: 0, 1, 2, 130 (SIGINT), 143
  (SIGTERM), 129 (SIGHUP). A config error has nothing on stdout and
  `Error: <text>` on stderr; an invalid document or a refusal prints
  `cof`'s own `{"ok":false, "error":...,"warnings":[...],
  "state_out":...}` shape on stdout instead, which osi's existing
  `read_stdout_json_error`/`failure_reason_fallback`/`run_status`
  (DESIGN.md §2.1 rule 7) already read engine-agnostically -- nothing
  engine-specific was needed there. (A missing orchestration file is
  the one exception, on both engines: a plain `Error: Orchestration
  not found: ...` on stdout, not the JSON shape -- `read_stdout_json_
  error` doesn't parse that line either, so the summary falls back to
  "aborted (no final state)" there, same as `cof`; this predates this
  PR.)
- `--events`/`--live-state` are electricity's own, in `cof`'s exact
  format (events format v1, a final live-state write equal to
  `--out`), so osi's plan join, status inference and `--log` output
  work unchanged.

**Argument order.** The documented form is `osi <orchestration> [config]`; osi maps it to `electricity <config> <orchestration>`. Neither file can be recognised by its extension (an orchestration may be `.json`). osi therefore keeps the order strict. It gives a hint only when the first argument has no `effects` key and the second has one.

**Missing config.** With no config, osi tells the user that electricity needs one. It does not invent a `{}` config, because that would silently drop the user's global settings, which `cof` would have applied.

**End-to-end tests** (`oscilloscope/crates/oscilloscope/tests/e2e_electricity.rs`, mirroring `e2e_cof.rs`) run behind `OSI_E2E_ELECTRICITY=1`, against an `electricity` built with `--features test-tools` (its `sleep`/`fail` providers keep the signal tests deterministic without a real child process) on `PATH`. `oscilloscope.yml`'s own `e2e-electricity` job builds that binary and runs them on Linux and macOS.

---

## 5. The plan side: what osi needs from `Program`

osi compiles the document with `electricity_compiler::check_for_run(path, &CheckOptions{skip_preflight: true, trust_document: true})`, linked by path, so the same plan serves both engines. From the IR it needs:

| Need | IR source today |
|---|---|
| tree, document order | `Program.root: Op`; `Region::Block{ops}`, `Parallel{branches}`, `If{then_, else_}`, `Loop{body}`, `TryFinally{body, finally}` |
| path | `Op.path: EffectPath` (`Name` segments; `Pass(LoopId)` placeholder under a named loop) |
| name, or transparent | `Op.name: Option<String>` (`None` means an unnamed `if`/loop) |
| type | `NodeKind::Leaf(Prompt/Tool/Use/Yield/Reflector)` / `Control(Region…)` |
| `on_error`, `enabled` | `Op.on_error`, `Op.enabled` |
| flow, concurrency | `Region::Parallel{max_concurrency, stop_on_error}`, `Region::Loop{flow, max_concurrency, max_iterations, min_iterations, collect}` |
| loop spec | `LoopSpec::Each{in_path, as_name, truncate}` / `While(Condition)` |
| details-pane summary | `ToolOp{provider, params}`, `PromptOp{model, provider, prompt_type, retries}`, `UseOp{source, outputs}` |

**Joining plan paths with state and event paths.**

- Turn each plan path into a pattern. A `Pass(L)` segment matches `iter_<n>`, and `n` becomes the pass index for that loop.
- Transparent ops add no segment, so their children match in the parent's scope. One concrete path can then match several plan ops: `then`/`else` reusing a name, or an unnamed `if`. osi keeps them as one merged row, with a list of candidate plan ops.
- `finally` ops share the container's scope.
- **`use` with `path`:** osi compiles the child file (with a cycle guard) and grafts its root's children under the `use` node, stripping the child's `prime`.
- **`use` with `inline`, and reflector `generated.iter_N`:** these have no static plan. Their rows are created as paths are first seen. The type is guessed from the shape of `meta`, the same rule the reference's SQL plugins use -- except for `meta.adapter`: the root and a `dynamic` always write the scope's effective `adapter`/`model` into their own node too, and a model-mode `if`/loop writes them alongside its own `condition_result`/`branch` or `progress_total` as the record of a real model call (a `use` writes neither). So a node's own `progress_total`/`each_in_path` (`Loop`), `branch`/`condition_result` (`If`), or `flow`/`mode` (a bare container) all win over `meta.adapter`, whether or not the node has children of its own in the flattened state -- only once none of those match does `meta.adapter` mean `Prompt` (issue #616). `meta.provider` (`Tool`) and `meta.orchestration`/`resolved_path` (`Use`, a leaf that still counts even though it has children of its own) are unaffected -- neither engine writes those to a container.

**Asks for the M0-G lanes:**

1. **A path display helper.** `impl Display for EffectPath`, rendering a pass as `iter_*`, plus a `matches(&str) -> Option<Vec<(LoopId, u32)>>`, so osi does not reimplement the join.
2. **Source positions.** An optional `span: Option<(line, col)>` on `Op`, so the details pane can show the YAML location. This is nice to have.
3. **A visitor.** `Program::walk()` or a visitor that yields `(depth, &Op)`.
4. **Stability.** The IR is documented as having "no stability promise". Since osi links by path in the same repository, a breaking IR change must update osi in the same PR, and the shared CI enforces that.
5. **A fallback.** osi must still work when compilation fails or is unsupported: no plan, and a tree built from observations only. This matters until M0-G lands.

---

## 6. Architecture

### 6.1 The `oscilloscope/` workspace (owner decision)

```
oscilloscope/
  Cargo.toml                      # workspace; path deps on ../electricity/crates/electricity-compiler, -bytecode
  crates/
    oscilloscope-core/            # no terminal code
      plan.rs      # PlanTree from Program (+ use path children), path patterns
      observe.rs   # live-state poller (mtime/size/inode, reread on rename), events tailer (partial-line safe)
      model.rs     # RunModel: rows, status (§2), loop progress, totals, `$ref` handling
      diff.rs      # snapshot diff -> Vec<LogLine>, events -> Vec<LogLine>, time-sorted by meta timestamps
      engine.rs    # trait Engine { fn command(&RunSpec) -> Command; fn caps() }: CofEngine, ElectricityEngine
      supervise.rs # spawn (process_group 0, stdin null, pipes), signal forwarding, exit status
      screens.rs   # Screen, header_line (the one source for the TUI header and --glance), the overview's model
    oscilloscope/                 # binary `osi`: ratatui + crossterm TUI and --log plain mode
  tests/fixtures/                 # recorded runs (scrubbed): plan, snapshots, events, stdout/stderr, exit code
```

`oscilloscope-core` has no terminal code, so a GUI (`oscilloscope-gui`) can reuse it later. The workspace version follows the repository version, like electricity.

Polling, every 100 ms, is preferred over `notify`. The live file is replaced by rename on every write, and polling is simpler to make correct across platforms.

### 6.2 Plain-text mode

`osi --log` is chosen automatically when stdout is not a TTY or `CI` is set. It prints the §2.4 lines, each prefixed with its own `HH:MM:SS` clock time (issue #613; local to the binary's own zone, clamped so the displayed time never runs backwards -- a line with no timestamp of its own, like a forwarded-signal notice, uses the time it's generated), plus the engine's stderr lines (prefixed `engine:`), and ends with a summary line and the engine's exit code. Durations and token counts throughout use `oscilloscope_core::format`'s one formatter each (DESIGN.md §6.3). Example (from p4, replayed at 19:56:22 local time):

```
19:56:22 ▶ flaky                 prompt scripted/scripted-model "flaky"
19:56:24 ✓ flaky                 2.0s  retries 2  ↑3 t ↓1 t
19:56:24 ▶ always_fails          shell ls /nonexistent-osi-probe
19:56:24 ✗ always_fails          /bin/ls failed (exit 1): ls: … (on_error: continue)
19:56:25 ✗ guarded.g_fail        /bin/ls failed (exit 1) …
19:56:26 ✓ guarded.g_cleanup     0.7s  (finally)
19:56:27 ■ run ok  5.7s · 12 effects · ↑3 t ↓1 t
```

### 6.3 TUI (shipped, issue #434; the state pane, treemap and token bar,
issue #527; the golden-section layout, the state tree, update
highlights, the scope icon and colour themes, issue #535)

`ratatui` (`=0.30.0`, default features off, `crossterm` + `underline-color`
only) and `crossterm` `0.29` (the version ratatui's own crossterm backend
uses), both building on the workspace MSRV (1.86). `--log`, or any non-TTY
stdout (a pipe, a file, `CI` set), keeps §6.2's plain stream unchanged; a
TTY gets this instead, for both `osi <doc>` and `osi watch`.

The render model is `oscilloscope-core`'s own (`render.rs`):
a `Header`, the plan tree flattened to depth-first `Row`s, the state pane's
own `StateRow`s, the `TokenBar`'s totals and in-flight line, and a selected
row's `Details` — no terminal code, so a GUI front end could lay the same
`RenderState` out its own way. Only `oscilloscope`'s `tui.rs` imports
`ratatui` widgets. The Cost screen's own by-store treemap (`treemap.rs`,
§6.3b below) is deliberately not part of `RenderState` at all: it is
built (and its squarified layout, `treemap::layout`, pure integer
geometry, computed in `oscilloscope-core` and only ever drawn, never
computed, by `tui.rs`) only while that screen is actually on display,
not on every redraw regardless of which screen is showing.

**Layout** (screen 2, `Flow`, issue #635 — §6.3a below covers the
screens this language switches between and the one-line header every
one of them shares). Below the header line: the flow pane on the left
(today's orchestration pane, read as control flow — each `if`'s own
decision, loop passes, quick steps, below); the selected pane over the
story pane on the right. Every split is the golden section, 62/38
(`oscilloscope_core::layout::golden_section`/`left_column_width`).
Three sizes (`tui.rs`'s own `thresholds::FLOW_THREE_PANE_MIN_WIDTH`/
`_MIN_HEIGHT`, `FLOW_TWO_PANE_MIN_WIDTH`/`_MIN_HEIGHT`):

1. 100×30 and larger: flow pane | (selected pane over story pane), both
   splits 62/38;
2. 80×20 up to that: flow pane | selected pane — the story pane is
   dropped, and `l` shows the log in the selected pane's own place
   instead;
3. below 80×20: one column, the selected pane over the flow pane
   (today's order), with no story pane and the flow pane's own columns
   (start/tokens/cost) dropped to duration alone.

**The flow pane** is `crate::flow::build_flow_rows`'s own output
(`oscilloscope-core/src/flow.rs`), built from the already-folded
orchestration rows (`render::build_folded`) plus `PlanTree`'s own
decision index (below): a named `if`'s real row gains its condition
(the compiled `Condition`'s text) and an untaken-branch summary row
read from `meta.branch`/`meta.answer`; an unnamed `if` writes no node
at all, so its own decision is synthesised and spliced in at its
branches' own position, inferred from whether any row under either
branch has actually started (never `Pending`/`Skipped`) — `PlanTree::
unnamed_if_decisions` records, in document order, every unnamed `if`'s
own host path, condition and both branches' direct paths, covering the
three places today's branch inference (`branch_alternates`) doesn't
reach: a top-level unnamed `if`, one directly under a tree `dynamic`'s
`Parallel` branches, and one inside a named loop's own pass body (its
`iter_*` placeholder resolved against the real pass paths the rows
actually have). Two branches that would read as the same step names
read `◆ if → ?` rather than guessing. A named loop with a known total of
exactly one pass, or an unsettled (`dynamic`) total once the loop
itself has finished with only one pass observed, merges its own
container row with that one pass row into a single `⟲ <name>  pass N
of M` line; more than one pass keeps one row per pass, as the
orchestration pane already did. The pane's own
footer totals every quick step in the run (from `unfolded_rows`, the
same way the overview's own flow card already counts them) and a
health line (failed/retrying/warning counts, read off the already-
folded rows' own status). A named `if`'s own row is no longer
breadcrumb-eligible (`fold::kind_breadcrumb_eligible`), so it never
folds away into a single-child breadcrumb — shared with the Cost
screen's own by-store chain folding (§6.3b), whose `gate › chosen`
tile for a named `if` with one child now splits into two tiles too.

**The selected pane** reads the same `FocusedStep`/`FocusDetail` model
the overview's own now pane already builds (`screens::build_focused_
step`) for an ordinary row, or a `SelectedDecision` (its condition,
mode, branch and, for a model-mode condition, `meta.answer`) for a
decision row. `s` swaps it for today's state tree instead (`App::
flow_selected_shows_state`), so a named report (#610) stays selectable.

**The story pane** is `crate::flow::build_story`'s own merge of the
TUI's own bounded story buffer (`main.rs`'s `story: Vec<diff::LogLine>`,
capped at 2,000 lines, filtered to `LogKind::{RunStart, RunEnd,
Decision, Report, Failure}` — every other kind, such as a plain `Start`/
`End`, is pushed to `--log`/the log pane same as ever but never enters
the story) and every unnamed `if`'s own inferred decision this same
build resolved, sorted by each entry's own parsed instant (never a
plain string compare -- an `--events` line's own millisecond
timestamp and a state-sourced one's microsecond one aren't
lexicographically comparable at all), with a missing timestamp
sorting last. `l` swaps it for the full log instead (`App::flow_
story_shows_log`).

**The header line (issue #626).** One line, shared by every screen,
replacing the three-row header above: the scope icon's own trace (one
animated braille row, about 8 cells, at the start of the line, coloured
by run state as the old 3-row bezel icon was — a static `◉` with
`--no-animation`), the document, `♥` while the engine process is alive
(the one osi started, or, for `osi watch`, the pid `--events`' own
`run_start` carried), run state, elapsed time, the current clock time
(the same `now` every other time in the frame uses, §6.3's own "one
clock"), and the ETA. Issue #649: once there are 3 or more ok runs of
the document's own content hash in its run history, the ETA is the
whole-run figure history gives (`ETA ~`); below that threshold it
falls back to the innermost running loop's own ETA instead (labelled
`loop ETA ~`, since plans have data-dependent loops and that figure is
never a whole-run one). Once the run is past its own usual (p90) end,
the header reads `ETA ?` rather than a time that keeps sliding.
`oscilloscope_core::history::compute_eta` works this out;
`oscilloscope_core::render::Header`'s `history_eta_ts`/`history_eta_
past_usual` fields carry the result into `header_line` -- set by
`oscilloscope`'s own history wiring (`history_io.rs`) directly on the
`Header` `render::build`/`build_folded` already returned, since
neither of those, nor `oscilloscope-core` generally, reads history
files itself. `oscilloscope_core::screens::
header_line` builds this from a `Header` and a `now`, with no terminal
code — the one source for both the TUI header and `--glance`'s own
line, so the two can never disagree about what it says. The run ID
moves to the root row's own details (and to `--log`'s `■ run started`
line, §2.4); it never appears in the header line at all. The effects
count and tokens that used to live here moved to the overview's own
flow and cost cards, and to the Flow screen's own flow pane title
(effects); tokens moved to the Cost screen (§6.3b) in issue #632,
screen 2's own token bar having moved there with them — below.

**Reports (#605/#610).** Once a `report` effect's own event (§3) has
arrived, the header shows the latest one's message instead, after the
clock time and before the screen tabs — `▸ <message>`, or `▸ warning:
<message>` for `level: warning`, styled in the theme's own warning
colour — for the rest of the run. **Truncation order** (issue #626),
when the line is short of room: the tabs shrink to their own numbers
first (`[0 overview]  2 flow` → `[0]  2`); then the report truncates,
down to a bare `▸` once fewer than about 8 columns are left for it at
all; then the document (keeping its own tail); then the ETA is dropped
entirely. Status, elapsed and the clock never truncate. A new report
counts as its own header change for update-highlighting (`header:
report`, never folded into `header:status`) even when it repeats the
previous one's own level and message word for word -- each sighting
carries its own `ts`, so a repeated "chain pass" on every loop pass
still flashes every time, not just the first; elapsed time still never
highlights.

Issue #613's "one clock": elapsed is measured from the run's own start
-- the first `--events` `run_start`'s own `ts`, else the earliest
`created_at` anywhere in state -- to `now`, the same `now` every row's
own elapsed and the ETA use this frame, never from when `osi` itself
happened to start (so `osi watch` attaching to a run already well
under way shows its real elapsed time, not a few seconds). Once the
run has ended, elapsed stops at its own end (`run_end`'s `ts`, else the
final state's own `prime.meta.completed_at`). The header additionally
shows the run's own start as a clock time (`since 11:04:58`) while it
is going, and once it has ended, its start and end together
(`11:04:58–11:31:10`); the ETA is shown as a clock time too (`ETA
~11:42`), not a remaining duration. Every duration anywhere in `osi`
(rows, the state pane, details, the header, the log's `end` lines, the
ETA) goes through `oscilloscope_core::format::format_duration`'s one
table (`0.4s` / `42.0s` under a minute, `6m12s` under an hour, `2h05m`
under a day, `3d04h` beyond that, rounding carrying into the next unit
at every boundary); every token count goes through `format_tokens`'s
one table (`369 t` below 1,000, one decimal below 100 of a unit
`18.0 kt`, no decimal at 100 or more `142 kt`, rounding carrying into
the next unit the same way). A running row's own elapsed (the
orchestration pane and the state pane both) updates every rebuild
without that, alone, ever counting as a highlighted change
(`changes::diff` never compares it). Every clock time is formatted
through an explicitly passed `oscilloscope_core::format::ClockZone`
(a `FixedOffset` in tests, the binary's own real local zone,
resolved per timestamp so DST is right) -- never the machine's zone
read directly, so golden snapshots are identical regardless of `TZ`.

"Planned"
multiplies a loop body's own leaf by that named loop's `meta.progress.
total` once it's known (so a four-pass loop's one-leaf body counts as
4, not 1), and "done" counts every finished pass separately; while any
crossed loop's own total isn't known yet, "planned" is a lower bound,
shown with a trailing `+` (`3/3+`).

A `use` leaf's own children, not the `use` node itself, are the real
planned/done units (counting both, for
a `use: path:` child whose children are separately grafted into the
plan, or neither, for a `use: inline:`/`ref:` child, which has no
static plan at all, let "done" outrun "planned" once a `use` child's
effects actually landed in state). A `path:` child's children are
statically known (grafted, §5), so "planned" is exact for it from the
start; an `inline:`/`ref:` child's are only known once discovered in
state, so "planned"/"done" count whatever of its children have
actually appeared so far, falling back to the `use` node itself (as
before this fix) only while nothing under it has ever been discovered
at all — a declared-outputs child's own children are dropped from the
final state even once it succeeds (§1.2), so without that fallback such
a `use` would count toward neither side, ever. "Done" never exceeds
"planned".

Both counts count the document root and every *named* `if`/loop/
`dynamic`/`finally`-wrapped scope too, not only leaves — a transparent
(unnamed) `if`/loop is the one exception, since it never fires its own
start/end hooks and so never writes a node of its own (§1.3/§1.4)
either. This matches `runtime.last_run.totals.effects_run`, the
engine's own count: "every completed node, the root and the containers
included" (§1.2). Under no plan at all (`osi watch`, §7, or an
`inline:`/`ref:` `use` child's own un-plotted descendants), a node
`from_meta_shape` can't name from its own `meta` keys but that still
has real children in state is counted as a container too — only a
container ever has them — so a bare `dynamic` with no distinguishing
shape of its own still counts, and a finished run read with no plan
matches the same run read with one.

Once a run has ended with a real `--out` write of its own — `ok`,
`failed`, or a first-signal `cancelled` (DESIGN.md §2.1 rule 7's
"ended" outcomes; never a second-signal `aborted`, nor a pre-execution
validation failure, neither of which ever writes one) — "planned" and
"done" both switch to reading `runtime.last_run.totals.effects_run`
directly (issue #620), the same total `osi` already shows as a token
(§6.3). That total is recorded live as each effect completes, not by
walking the final state, so it is exact for every shape, including
two a state walk alone cannot see: a declared-outputs `use` child,
whose own effects never land in the final state tree at all even
though they ran, and an *unnamed* loop with more than one pass, whose
every pass overwrites the same state path (§1.3) so only the last one
survives for a walk to find. Counting from state (the walk described
above) is only the fallback for an ended run whose final write has no
`effects_run` at all. `osi watch` (no plan at all, §7) gets the same
guarantee: the override needs nothing from the plan, only the run's
own ended status and its `--out` state.

**The orchestration pane** (formerly "plan"; `draw_tree` in `tui.rs`).
Titled ` orchestration <name> `, `<name>` the top-level document's file
name without its extension (orchestration documents have no `name:` of
their own), with ` (following)` appended while follow is on. Each row
shows:
  - a status glyph: `·` pending, `◐` running (also shown for a "likely
    running" guess), `◌` running/queued, `✓`, `✗`, `!` failed-handled,
    `↷` skipped, `⊘` cancelled, `?` aborted (no glyph of its own in the
    original table; `?` is the one status left with no better fit);
  - the duration, `n/total` for a loop's own progress, and the provider
    or model in a dim colour -- the model only for a row that actually
    runs a prompt (`Prompt`/`Reflector`); a container never shows one,
    even carrying `meta.model` of its own (issue #616).

A named loop's own pass container (`prime.fan.iter_N`) never has a
`{value, meta}` node of its own (§1.3) — only its own steps do — so its
row's status is derived from its leaf descendants, at any depth, rather
than defaulting to pending regardless of how they actually finished
(reproduced with a multi-step tree-loop
pass): any descendant still running makes the pass row running; else
any unhandled failure makes it failed; else, once every descendant has
reached a terminal status, it reads done; otherwise it stays pending.
The same rule applies to a chain-flow loop's own pass container, which
has the identical no-node-of-its-own shape.

A `report` effect (#605) never becomes a row here, named or not
(#610): it fires no `start`/`end` and so is never a plan row to
begin with, but a *named* one still writes a `{value, meta}` node
(§1.2) that `RunModel::observe` would otherwise show as a finished
row -- `PlanTree::is_report_path` (resolved the same way `match_path`
resolves a loop's own `iter_N`, so a report inside a named loop's pass
or a grafted `use: path:` child is covered too, and `false` again when
some other op shares that exact path, DESIGN.md §5's own `then`/`else`
name reuse, since that op's row must still show) keeps it out. Where
the plan has no real entry at that path at all (`osi watch`, a `use:
inline:`/`ref:` child), `RunModel::is_report_event_path` (from the
report's own `--events` line, and only once the node's own value also
has the report shape) does the same job instead -- never when the plan
already has something to say about the path, so a named `if`/
`dynamic`/`use` whose first thing is an unnamed report keeps its own
row even though that report's own event also names its path. Either
way, a loop pass that holds a named report alongside one real step
still folds exactly as if the report weren't there at all.

Follow is **on when the TUI starts**: the selection tracks the running
effect (`App::follow_target`, unchanged, now routed through
`App::select_tree_path` so it also resets the details pane's own
scroll), so the running effect's details are open with no key press.
When nothing is running, the selection stays put. Any manual move
(`↑↓`/`j`/`k`, `PgUp`/`PgDn`, `Home`/`End`, `g`/`G`) turns follow off; `f`
toggles it, as before. The pane scrolls: the selected row is always kept
in view (`tui.rs`'s own `clamp_scroll`, moving the top of the viewport
the minimum amount needed rather than re-centring), and once any row is
hidden above or below, the border shows a `12/48`-style position.
Collapse/expand (`←`/`→`), `e` and `/` all act on the rows the pane
already shows, same as before; the scroll position follows whatever
those leave visible.

**The details pane** moves above the orchestration pane (issue #535).
For any node that is a prompt effect (`meta.prompt_chars` recorded --
every `runtime.state.prompt_sent` mode keeps it, #587), two blocks come
right after the `path`/`type` lines, before the rest of the meta list:
`sent  ↑ N tokens` with a snippet of the sent text, and `received
 ↓ N tokens` with a snippet of the reply — `meta.answer` when present
(the raw reply for `boolean`/`number` prompts), else the node's own
value. Both token counts are `model::node_tokens` (the same `_total`-
first rule the header/token bar/state pane/the Cost screen's treemap
and table all use); a still-
running prompt's `sent` shows the token bar's own in-flight estimate
(`≈ceil(chars/4)` over `meta.prompt_chars`) and `received` reads
"waiting for the reply", since neither engine reports output tokens
before a prompt finishes. A prompt that failed before any reply shows
its error as always, and no `received` block. Snippets collapse
whitespace runs (including newlines) to one space and cut at 200
characters with `…`. `tokens_sent`/`tokens_received` drop out of the
plain meta list for these nodes (the block headers already carry
them), and the old `prompt_sent (v for full):`/`value (v for full):`
blocks don't appear either — `v` still opens the full prompt or value.
Under `runtime.state.prompt_sent: full` or `head:<N>` the snippet is
the recorded text (a `head:<N>` one as is, marker included); under
`sha256`, where the text itself was never kept, it is a placeholder,
`not recorded (N characters, sha256 <first 12 hex>)`, built from
`meta.prompt_chars` and `meta.prompt_sent_sha256`
(`model::prompt_sent_or_placeholder`) — `v` shows the same placeholder
rather than falling through to the value. Every other node's details
are unchanged.

Measured on both engines with the scripted adapter: a model-mode `if`/
`while` condition's own container node gets `meta.answer`, `adapter`,
`model` and `tokens_sent`/`tokens_received` on both `cof` (`core/
conditional.py`'s `_evaluate_model`) and electricity (`electricity-vm`'s
`exec::condition_model`) — but *neither* engine ever writes `meta.
prompt_sent` for one, so such a node never gets the two blocks above at
all, only today's plain meta list. The issue's own premise (that these
blocks also cover model-mode conditions) does not hold on either
engine as measured; extending it would need upstream work in both
engines, out of scope for `oscilloscope/`-only changes.

A named report's own node (#605/#610) gets its own details shape
instead of the generic meta list: `type report`, its `level`, the
latest `message` and the clock time it last arrived, and `data` as
pretty JSON, scrollable the same way a long `prompt_sent`/`value` is.
`message`/`level`/`data` come from the latest `--events` `report` line
for that exact path when one has arrived, falling back to the state
node's own copy otherwise; `data_truncated` (the engine's own 16 KiB
cap on the *line*, not the state write) shows `(data truncated)` only
when that fallback has nothing either -- a named report's own state
node always keeps its full value, so a line truncated for size still
shows it there. An *unnamed* report writes no node at all, so its own
`data` is visible only in its log line's preview (§2.4), never here.
The details pane follows whichever selection *last actually changed*
(`App::details_source`) -- the state pane's own
(`App::state_selected_path`) or the orchestration pane's (`App::
selected_path`) -- not whichever pane merely has focus right now: `tab`
passes through `Details` with neither of the other two focused, so
that alone can never answer it, and a selection made in the state pane
must still be what `Details` shows (and lets the user scroll) once
they tab there, since a named report has no row in the orchestration
pane at all to select instead. The two selections stay independent of
each other exactly as before. Only a selection the user moves switches
the details pane's source: follow, a row folding away mid-run and the
first selection of a run move the orchestration pane's selection
without taking the details pane away from a state-pane selection, and
`v` and the details pane's own highlights always act on the node the
details pane is showing.

The details pane scrolls when it has focus (`↑↓`, `PgUp`/`PgDn`,
`Home`/`End`, `g`/`G`, via `ratatui::widgets::Paragraph::scroll`), and
returns to the top the instant either selection actually changes
(`App::select_tree_path`, `App::select_state_path`), whichever one
was moved.

**Focus** (`tab`) cycles orchestration → details → state →
orchestration, each highlighted with the theme's own focused-border
colour.

**The state pane** (`render::build_state_rows`, `tui.rs`'s `draw_state`)
is a tree, not a flat list (issue #535). Every `{value, meta}` node
under `prime` is a row, nested under its parent and indented by depth;
a container with no node of its own (a loop's own `iter_N`) is a row
too, so the tree has no gaps — `render::build_state_tree` groups every
path `flatten_state` returns by splitting it on `.`, synthesising a
bare container wherever an intermediate segment has no entry of its
own. A row is labelled with its own key only (never the full path).
Shows no value or error text at all — the details pane and `v` are
where those are read — only: the status glyph; the model for a row
whose own kind actually ran a prompt (`Prompt`/`Reflector`, the same
gate the orchestration pane uses) or else the provider for a tool, the
duration once finished, `retries N` when `retries_used > 0`, and
`n/total` for a loop, all in dim text (`None` for a bare container,
which has nothing of its own to summarize, *and* for a container that
does have a node of its own — the root and a `dynamic` always write
the scope's effective `adapter`/`model` into their own node too, and a
model-mode `if`/loop writes them alongside its own condition/progress
keys, so a row's *kind*, not merely whether it has children, decides
whether its model shows at all, issue #616); and `↑sent ↓received` on
a second line, only when at least one is nonzero — a leaf's own
tokens, or (for a container, bare or not) the sum over its whole
subtree (`model::subtree_tokens`).

A *named* report's own node (#605/#610) is a row here like any
other leaf's, unlike the orchestration pane's own exclusion above --
selecting it is how its `level`/message/`data` reach the details pane
at all. An unnamed report writes no node, so it has no row here
either.

Order is latest-or-current first, at every level, ties broken by key
(`render::state_subtree_recency`): a row's recency is the latest update
time anywhere in its own subtree (a node's own update time is its
`completed_at`, or `created_at` while it has none, parsed as a real
instant via `diff::parse_instant`, never compared as a raw string), and
a running row anywhere in the subtree makes the whole row count as
current, sorting above its finished siblings regardless of timestamps.
Status for a row with its own node reads `statuses` (from events/plan
inference) when it has an entry for that exact path, else the node's
own running/ok/failed shape; a bare container with neither falls back
to the same rule a tree-loop pass's own container row already uses
(any running descendant makes it running, else any unhandled failure
makes it failed, else done).

Collapse/expand (`←`/`→`) works here too, when the state pane has
focus, on its own independent collapsed set (`App::state_collapsed`,
`keys::visible_state_rows` — the same hidden-subtree rule `visible_
rows` already applies to the orchestration pane's own `App::
collapsed`). Everything starts expanded. The pane scrolls when it has
focus, with its own selection (`App::state_selected_path`) kept fully
independent of the orchestration pane's own — selecting a row here
never moves the orchestration selection, and vice versa.

**The log pane** moves here from the bottom, otherwise unchanged — the
same lines `--log` prints (§2.4), plus the engine's own stderr, as a
scrolling tail (no scrollback in this milestone: always the most recent
lines that fit).

**The token bar's own data** (`render::TokenBar`/`build_token_bar`,
`render::token_bar_in_flight_line`) — issue #632 moved the dedicated
bordered strip this used to draw across screen 2's own full bottom
width to the overview's own cost card (§6.3a) instead, as plain text;
screen 2 itself has no token bar of its own any more. The data and its
own rules are unchanged:
  - **Totals:** `in`/`out` with exact counts (thousands separators) —
    always equal to the header's own `↑`/`↓`, since both read the token
    rule below.
  - **What's in flight:** how many prompts are running, and an
    estimated input for them, `≈ceil(chars / 4)` over `meta.
    prompt_chars`. No output estimate: neither engine reports a
    prompt's tokens before it finishes.

    The *count* comes from `render::Row`, not from the live state
    alone (issue #527's own PR): a tree-loop pass's prompt, and (on
    `cof`) a `use` child's, are confirmed `Running` by `--events`
    while a `--live-state` reader can still be a step behind -- on
    electricity, a tree-loop pass's prompt (issue #465) and a `use`
    child's (issue #448 item U6) both now reach `--live-state` while
    they still run, the same overlay either way, but only once the
    mirror's own next coalesced write actually catches up (DESIGN.md
    §1.4) — a row with no state node of its own yet still counts when
    its own `kind` is known to be [`Prompt`], which a tree-loop pass's
    is (its plan entry is matched through the loop's own `iter_*`
    placeholder)
    and a `use: path:` child's is too (its plan is grafted under the
    `use` node, §5) — but a `use: inline:`/`ref:` child's is not: it
    has no static plan at all, and `--events` carries no effect kind
    (§3), so such a row cannot be told apart from a running *tool* by
    path and events alone. A row that *does* have a state node applies
    `model::is_running_prompt`'s full criteria instead: `created_at`
    and no `completed_at`, `meta.adapter`/`meta.model` set and `meta.
    prompt_chars` recorded, and `meta.waiting_for` empty — a prompt
    merely queued behind a concurrency slot isn't processing yet (an
    event-only row can never be merely queued in the first place:
    `on_effect_start` fires only once a pass or branch actually
    begins). These fields all come from `runtime.state.prompt_sent`'s
    own setting (#577), not `meta.prompt_sent` itself: `prompt_chars`
    and the adapter/model pair are written together in every mode
    (`full`, `head:<N>`, `sha256`), so a prompt running under `sha256`
    -- where `meta.prompt_sent` stays `null` -- is counted exactly
    like one running under `full` (#587). A model-mode `if`/loop
    condition sets `adapter`/`model` too but never `prompt_chars`, so
    it is never mistaken for a running prompt here.

    The *estimate* sums each running prompt's own `ceil(chars / 4)`
    from `meta.prompt_chars` when the run recorded it (every mode), or
    else the length of whatever `meta.prompt_sent` text is actually
    present (older state with no `prompt_chars` at all) -- for one
    confirmed running by events alone, with no state node to read at
    all, neither is available, so the total is shown as a lower bound
    (`≈52+`) rather than silently reading too low. When *no* running
    prompt's `prompt_chars`/`prompt_sent` is known at all, the estimate
    isn't merely low, it's entirely unknown — `≈0+` would read as a
    broken number (zero tokens, yet something's running?), and `now in
    ?` reads as if an estimate were coming once it wasn't going to;
    issue #631's own fix says only what is actually known in that
    case (`3 prompts in flight`), with no `now in` prefix at all,
    rather than either broken form.

    Issue #613's "agents in flight": a running *tool* row whose
    `provider` is `agent` (cof's own rule at `core/compiler.py`,
    trimmed and case-insensitive) is counted alongside running
    prompts (`2 prompts · 1 agent in flight`), with no token estimate
    of its own — live agent token usage (#612) is a later lane. Only
    `cof` can ever produce a running agent: electricity compiles an
    `agent` effect's prompt file for parity with `cof check`, but
    refuses to run the step itself -- `agent` is a known cof plugin
    name (`electricity-bytecode`'s own `PLUGIN_NAMES`), and
    `electricity-tools`' registry has no builder for it, so its
    refusal walker refuses it with the preview marker, the same way
    it refuses any other orchestration outside electricity's own
    preview scope. With neither a prompt nor an agent running, the
    line says so ("nothing in flight").

    This count also reads a running leaf's own `provider` from the
    compiled plan, not state alone: a tree-loop pass's tool child,
    like its prompt counterpart above, never gets a state node of its
    own while `cof` runs it, so an agent there would otherwise never
    be counted.

**The token rule** (`model::node_tokens`): one node's own
`tokens_sent_total`/`tokens_received_total` when present, falling back to
`tokens_sent`/`tokens_received` otherwise. Confirmed by running both
engines with the scripted adapter: `cof`'s `cli/runtime_shim.py`
(`_run_totals`/`_RunTotals.record`) and electricity's
`electricity/crates/electricity/src/lib.rs` (`RunTotals::record`) both
build `runtime.last_run.totals` from exactly this pair, reading `_total`
first and falling back to the plain field. The first retried-prompt
scenario tried (a scripted adapter error, then a success) turned out not
to tell the two rules apart at all: the scripted adapter's own failed
attempt carries no tokens of its own, so the winning attempt's plain
`tokens_sent`/`tokens_received` equalled the `_total` pair exactly
either way. A `prompt_type: json` prompt whose first reply is syntactically
invalid JSON does: decoding it fails (retryable, since a decode failure
is treated the same as a classified-retryable adapter error —
`core/prompt.py`'s own `_generate_with_fallbacks`), but that attempt
still spent real tokens, so they still accumulate into `_total`
regardless of whether the attempt won. Run against both engines (a first
reply of `5`/`7` tokens, a second, valid one of `11`/`22`), `cof` and
electricity wrote the byte-identical `tokens_sent_total: 16`/
`tokens_received_total: 29` and the identical `runtime.last_run.totals`
from it — the plain pair (`11`/`22`, the winning attempt alone) does
*not* equal those totals, so this scenario is what actually tells the
two rules apart. The header, the token bar, the state pane and the
Cost screen's own treemap and table all read tokens this same way
(`model::node_tokens`/`subtree_tokens`), so a retried prompt can never
make one surface
disagree with another; `model::tests::subtree_tokens_matches_run_totals_
for_a_retried_prompt_on_cof`/`_on_electricity` pin it against that same
recorded shape from each engine.

**Folding** (issue #613, `oscilloscope-core/src/fold.rs`) applies to
the orchestration pane's own rows only, after they're built
(`render::build_folded`, used by the TUI; every other caller of
`render::build`, including `--log`/`osi watch`'s own plain stream and
every existing test, sees the full, unfolded tree): on by default,
`z` turns it off.

- **Breadcrumbs.** A container with exactly one child folds with it
  into one row, `outer › inner`, keeping the innermost row's own path
  (so selection, details and follow all still target it) and status.
  A still-`Pending` container or child (including an event-only row,
  discovered from `--events` alone, whose status or duration isn't
  known yet) stays unfolded until it is. A *running* row still folds
  into its own breadcrumb, which is exactly the row follow mode
  selects for it.
- **Quick steps.** Two or more consecutive, finished, sub-second tool
  steps fold into one `N quick steps` row, keyed by the first step's
  own path. A step that is running, failed, queued or still pending
  is never folded into one of these. `⏎` on that row expands the
  group in place (adds its key to a per-session set folding skips);
  `←` removes it again.
- **Pass labels.** A loop pass is labelled `pass N of M` in the
  orchestration pane (1-based; `pass N` alone while the loop's own
  `progress.total` isn't known yet) -- the state pane keeps the real
  `iter_N` key, and the details pane's own title shows both
  (`pass 3 of 8 (iter_2)`).

**Keys:**

| Key | Action |
|---|---|
| `↑↓`/`jk` | move (the focused pane's own selection, or the details pane's scroll) |
| `PgUp`/`PgDn` | move a page (the focused pane's own last-rendered interior height) |
| `Home`/`End`, `g`/`G` | jump to the first / last row |
| `←→` | collapse / expand (orchestration pane) |
| `⏎` | expand a quick-steps row (issue #613) in place; `←` folds it again |
| `z` | fold on/off (issue #613: breadcrumbs and quick-steps summaries); on by default |
| `f` | follow the running row (on by default) |
| `e` | errors only (keeps a failed leaf's own ancestors so there's still a tree to show it under) |
| `/` | filter (substring, on path or label; same ancestor-keeping rule) |
| `tab` | orchestration → details → state |
| `v` | full value, in an overlay; `v` again cycles between `prompt_sent` and the value when the row has both (opening on `prompt_sent` first when there is one); any other key closes it |
| `c` | cancel, with a confirm — the same as a *confirmed* `c`/`q` always has (`do_run`'s own signal-forwarding/kill-escalation path, not a separate one) |
| `q` | quit; asks first if a run is going (confirming sends the same cancelling signal as `c`, and leaves the instant the engine exits — see "the finished screen" below); with nothing running, quits at once, no confirm; in `osi watch`, always just detaches, with no confirm — watch owns no engine to cancel |
| Ctrl-C | checked before any dialog, and never merely dismisses one: forwards SIGINT at once with no confirm while something's running (a second Ctrl-C starts the same 10s kill deadline a real repeated SIGINT would), quits at once with nothing running, and in `osi watch` always just detaches |
| `?` | help |

**The finished screen.** The engine exiting (or, for `osi watch`, the
watched run ending) does not, on its own, end this loop: the header, every
row's final status, and the details pane (`v` included) stay live and
navigable on whatever the run ended with — ok, failed, cancelled or
aborted — until the user explicitly leaves. The instant this loop first
observes the ending (the engine's exit, or `osi watch`'s own events-based
or dead-pid stop condition), it drains once more before ever rebuilding
this screen: events tailed until a pass turns up nothing new, then the
same authoritative final read `finish_run`/`finish_watch` themselves take
— a fast-exiting engine's last events and final write (§1.1: writes land,
then it exits, with nothing left to ever prompt another poll afterwards)
are never missed, and this screen can't show a real `ok`/`failed` ending
as `aborted` just because nothing is still running to eventually correct
it. Only three things make this loop leave the instant the engine has
exited, rather than staying on that final state: a confirmed `q`, a
Ctrl-C cancel, or any external INT/TERM/HUP osi itself received (a SIGHUP
in particular never waits for
a key — the terminal is gone by definition). Short of one of those three,
a run that simply finishes on its own stays up for `q`/Ctrl-C to leave.
`osi watch`'s own `q` while the watched run is *still going* prints a
distinct "■ detached (the run is still going)" ending and exits 0, rather
than the generic "no final state" abort every other stop condition
without one falls into; `q` once the run has already ended gets the
normal ok/failed/aborted summary instead. `finish_run`/`finish_watch`
still print the usual plain summary once the terminal session is torn
down, exactly as they already do for `--log`.

**Terminal safety.** A `TerminalGuard` enters raw mode and the alternate
screen once (undoing raw mode again if the alternate screen fails to
open); `Drop` restores both on every normal return from the TUI loop
(osi's own error, the engine exiting, a confirmed quit), and a panic hook
installed alongside it restores the terminal before the default panic
handler's own message would otherwise print into it. A forwarded
SIGINT/SIGTERM/SIGHUP is forwarded to the engine as the *same* signal
(never turned into a SIGINT regardless of which arrived), with the same
escalation `do_run`'s plain loop applies, and never leaves the TUI loop
early on its own — the loop keeps rendering until the engine actually
exits, so `Drop` still runs on the normal path out. If the TUI itself
can't start (an exotic CI pty, a stdout swapped out from under osi), both
`osi <doc>` and `osi watch` fall back to their own plain supervise/watch
loop rather than a bare, signal-blind wait. `osi` itself being
`SIGKILL`ed is the one exception (§4.1): nothing can run cleanup code
after that at all. The render state (plan-tree rows, state rows,
header counts) is rebuilt at most once every `REDRAW_INTERVAL` and
only when an observation, a key or a resize actually changed something
— forced at least once a second regardless, so the header's own
elapsed time still visibly ticks while the run is otherwise quiet. The
state pane is built from the one `flatten_state` call `render::build`
already makes for the plan tree, not a second pass of its own; the
Cost screen's own by-store tree (§6.3b) is a further, separate pass,
but only while that screen is actually on display, not on every
rebuild regardless of which screen is showing, the way screen 2's old
treemap used to be. Measured against a synthetic 2,000-node state, one
redraw stays well under `REDRAW_INTERVAL`
(`render::tests::redraw_from_a_2000_node_state_stays_well_under_the_
redraw_interval`). That measurement times `render::build` alone, which
leaves out the ratatui draw itself;
`tui::golden_tests::drawing_a_2000_node_state_stays_well_under_the_
redraw_interval` times one `TestBackend` draw of the same state too,
including building and drawing the Cost screen's own by-store tree and
layout for it.

The actual *draw* is a separate cap from the rebuild above
(`REDRAW_INTERVAL`, 100ms, the same tick the icon's own trace moves by):
issue #535's own redraw rule marks the frame dirty every tick, beyond
whatever an observation/key/resize already marked it, while any update
highlight is still fading (`highlight::Tracker::has_active`) or the run
is live with animation on (so the icon's trace keeps moving) — with
neither, this adds nothing, so an idle, ended screen with `--no-
animation` costs no extra redraws at all. `REDRAW_INTERVAL` still caps
how often the actual draw happens either way, and ratatui's own diff
against the previous frame means a tick with nothing visually different
(most of an icon-animation-only tick) writes nothing to the terminal at
all.

**Update highlights** (issue #535). A soft background tint on whatever
just changed, fading to nothing over about 1.2 seconds, eased out. What
changed between two redraws is computed front-end-independently, in
`oscilloscope-core::changes::diff`, from the previous `RenderState` and
the new one, keyed by stable strings (`row:<path>`, `state:<path>`,
`log:<index>`, `tokenbar`, `header:status`/`:effects`/`:tokens`) — a
row's status or loop progress changing, a state row that's new or
whose status/dim meta/tokens changed, a new log line, the token bar,
and the run's own status/effects/tokens. `header:effects`/`:tokens`
keep their names from when the header line itself showed those two —
issue #626 moved them into the overview's own cards and screen 2's
orchestration pane title, but the underlying change they mark is the
same either way; issue #632 gave `header:effects` its first real
consumer, the overview's own flow card, and `header:tokens`/`tokenbar`
their first ones since the token bar itself left screen 2 (also #632):
the overview's own cost card tints its ↑/↓ line with the former and its
in-flight line with the latter. The Cost screen's own by-store
treemap (§6.3b) is built only while that screen is on display, so it
is not part of `RenderState` at all and has no update-highlight key of
its own here. The first observation (`old:
None`) never reports anything, so `osi watch` attaching to an already-
finished run opens on a calm screen; elapsed time, selection, scrolling,
the filter, collapse sets and a resize are not part of `RenderState` at
all, so none of them can ever appear as a change. Every redraw rebuild
feeds its own diff into a `highlight::Tracker` (`main.rs`'s `mark_
highlights`), which only ever remembers the most recent trigger time per
key — a retrigger restarts that key's own fade rather than stacking a
second one. `Tracker::style_for` turns a fade's own linear progress,
eased out, into a `Style` for the active theme and colour mode
(`highlight.rs`, in the `oscilloscope` crate, never `oscilloscope-core`
— the fade's own timing and styling are a front-end concern): a named
theme fades its own highlight tint to its own background in RGB (or the
nearest xterm-256 index on a 256-colour terminal); `terminal` has no
fixed background to fade toward, so it keeps a stepped ramp instead (6
truecolour steps, 3 indexed steps, or bold for about the first half-
second on 16 colours/`NO_COLOR`), always ending in no override at all.
The ramp's own tint is a low-chroma slate blue, `rgb(92, 108, 122)`
(HSV saturation ~0.25), chosen to read as subtle rather than an alert
on a dark terminal while staying legible under dark text on a light
one; each step desaturates it toward a neutral mid grey at roughly
the same lightness, `rgb(107, 107, 107)`, never toward black — mixing
toward black was tried first and either clashed with light default
text (near full strength, on a dark terminal) or buried dark default
text (near the ramp's dark end, on a light one).
`--no-animation` (both `osi <doc>` and `osi watch`) skips marking
entirely, so `Tracker::has_active` never reports anything after it.

**The scope icon's own trace** (`oscilloscope_core::icon` for the
trace's own dot math, issue #535; folded into the one-line header,
issue #626). The header line's own first cells (`icon::
header_trace_text`, `icon::TRACE_CELLS` — about 8 of them, one row of
braille glyphs built from a 16×4 dot matrix, `icon::trace_dots`), no
bezel or knob strip any more: a single device sharing the header's own
row, not a box beside it. While the run is live, a sine-like wave
scrolls one dot column per frame, a frame being the same
`REDRAW_INTERVAL` tick the redraw rule above forces while live; once
the run has ended, the trace is a single still, flat line — `osi
watch` on an already-ended run shows the flat line from its own first
frame. `--no-animation` skips the braille glyphs entirely and draws a
static `◉` in their place instead (`header_trace_text`'s own
`animate: false` branch), the same single glyph an ended run's still
trace used to collapse to. The trace's own colour follows run state
through the active theme (`icon_trace_ok`/`_failed`/`_cancelled`,
`icon::trace_color`) — there is no separate bezel colour any more,
nothing in the one-line header is ever drawn dim beside it. `trace_
dots` takes its own frame index as a plain `u64`, never `Instant::
now()`, so a test pins an exact frame.

**Colour themes** (`oscilloscope`'s own `theme.rs`, issue #535). One
`Theme` of named colour roles (background, text, border/focused border,
dim text, a colour per status kind, error, the token bar's in/out
colours, the treemap's on-accent text, the highlight tint, and the
icon's bezel/trace colours) is the only place a `ratatui::style::Color`
literal lives in the `oscilloscope` crate; every draw function reads its
colours from a `&Theme` (`tui::DrawCtx`), never a literal of its own.
`terminal`, the default, reuses the terminal's own ANSI colours and
paints no background — choosing no theme looks identical to before this
issue. The other ten (`dracula`, `nord`, `solarized-dark`/`-light`,
`gruvbox-dark`/`-light`, `catppuccin-mocha`/`-latte`, `tokyo-night`,
`one-dark`) are fixed RGB palettes from each project's own published
colour specification, paint the whole screen's own background, and have
a handful of roles adjusted per theme wherever the published value
didn't clear a 4.5:1 (3:1 for dim text) WCAG contrast ratio against that
background — documented, per role, in `theme.rs` itself.

`--theme <name>` (both `osi <doc>` and `osi watch`) chooses a theme;
with neither flag, `OSI_THEME` applies; with neither, `terminal`. An
unknown name from either is a usage error listing the valid ones. `t`
cycles live through the eleven names in listed order, wrapping, without
rebuilding the render state; the header shows the new name for about 2
seconds, and the log pane gets a `theme: <name>` line. `ColorMode`
(`theme::ColorMode::from_env`) is detected once at startup from
`NO_COLOR`/`COLORTERM`/`TERM`, in that priority order, and shared by
every theme choice for the run: truecolour keeps a named theme's RGB as
is; 256 colours maps every role to the nearest xterm-256 index
(`theme::nearest_xterm256`, the standard 6×6×6 cube plus the 24-step
greyscale ramp, picking whichever is closer); 16 colours falls back to
`terminal` with one log line saying so; `NO_COLOR` always wins, over any
theme, resetting every role to the terminal's own default — structure
still reads from glyphs and modifiers, never colour alone.

### 6.3a Screens, the overview and `--glance` (issue #626)

`oscilloscope_core::screens::Screen` names the four screens osi
switches between with `0`-`3`: `Overview` (`0`, the default for both
`osi <doc>` and `osi watch`), `Time` (`1`), `Flow` (`2`, §6.3's own
layout above, issue #635) and `Cost` (`3`, §6.3b
below). `Time` has no screen built yet — `Screen::is_available` says
so, `Screen::from_key` refuses `1` on that basis alone, and it gets no
tab in the header line. Selection, follow, folding, filters and
collapsed rows all live on `keys::App` outside the `screen` field, so
switching never touches any of them.

**The overview's model** (`oscilloscope_core::screens::Overview`,
`build_overview`) is built from exactly the same inputs `render::build`
already has — the live state, `--events` (through `RunModel`), the
compiled plan and `runtime.last_run.totals` — no engine change needed.
A caller builds it only while the overview is actually on display
(`main.rs`'s own `build_overview_if_active`), right after the existing
rebuild: one rebuild of a 2,000-node state stays well under the redraw
interval, and nothing here is built at all for a screen not shown.

- **Phase strip:** the document's own top-level rows (depth 1 in the
  already-folded `rows` `render::build_folded` produced, so a single-
  child wrapper chain is already one breadcrumb row by the time this
  sees it), each with its own status, start time (`created_at`) and
  duration (ticking while it runs, same as an orchestration-pane row).
  How many fit a given width, and the `○ N more` for the rest, is
  `tui.rs`'s own `draw_phase_strip`, not this model — core stays
  width-agnostic.
- **Now pane:** the step follow selects — the row matching the shared
  `selected_path` if it's running, else the deepest running row (the
  same tie-break `keys::App::follow_target` uses; the document root
  itself is excluded from both this pick and the "+N more running"
  count, since it's running for the whole run and would otherwise
  inflate that count by one permanently). A prompt's own focus detail
  carries its `sent` token count (actual or the in-flight estimate)
  and a snippet; a tool's its rendered command; an agent's (a tool
  whose own `meta.provider` is `"agent"`) its `engine`/`model` when the
  node happens to carry them, plus a fixed note
  (`AGENT_LIVE_DATA_NOTE`) that live session data isn't available yet
  — #612's live-agent-data lane is a later issue. A step known only
  from `--events` (a tree-loop pass's prompt, confirmed running
  before it ever gets a state node of its own, §1.4) has no
  `created_at`/`meta.model`/`meta.adapter` to read `since`/the
  model-or-adapter line from yet — issue #631's own fix:
  `RunModel::event_started_at` gives `since` from the step's own
  `start` event, and `PlanTree::prompt_model_or_provider` gives the
  model or adapter from the compiled plan's own prompt op, the same
  two sources the token bar's own running-prompt count and "agents in
  flight" already trust for exactly this case. With nothing
  running, the last finished row (by `completed_at`); once the run has
  ended (`header.status != Running`), the outcome, start/end and
  totals, plus (issue #637) the run's own stopped agent if it has
  one -- a `meta.live` node left behind by an interruption, with no
  `completed_at` of its own, since an ended run otherwise has no
  step left to focus on at all. It is named, with its last-known
  live block shown the same way a running or last-finished step's
  own already is, never as running.
- **Time card:** the run's own start/end, elapsed, ETA, a bar of the
  phase strip's own durations (drawn as blocks scaled to the longest
  one, `tui.rs`'s own `draw_time_card`) and its slowest *finished*
  entry.
- **Flow card:** `header.effects_done`/`effects_planned` (moved here
  from the header line) and a failure count; the latest two decisions
  (a named `if`'s own `meta.branch`, keyed by its own path's last
  segment, sorted by `completed_at`/`created_at` descending) padded
  with the latest report when fewer than two decisions exist; every
  named loop row's own progress; the first `Pending` row in document
  order; and the count/total duration of every `fold::is_quick_step`
  row in the *unfolded* tree (not however this frame's own folding
  happened to group them).
- **Cost card:** tokens (moved here from the header line too); cost,
  shown only once it's actually positive
  (`screens::effective_cost_usd`, shared with the now pane's own
  ended summary) — `runtime.last_run.totals.cost_usd` once cof has
  written it, summed across every effect's own `meta.cost_usd`
  before that (so it shows the moment any effect reports one, not
  only once the run has ended); a sparkline
  (`screens::build_tokens_over_time`) bucketing each effect's own
  sent-plus-received tokens by its own `completed_at` into twelve
  equal spans across the run so far; the highest-token leaf
  (`cost::top_store`, the same leaf-scan rule §6.3b's own
  largest-stores table ranks first, so the two can't disagree about
  which store is the run's biggest); once the run has ended, the
  run's own stopped agent's last-known tokens (issue #637), as
  `stopped <label>` with its sent/received figures -- `runtime.
  last_run.totals` (the figures above) never includes a session that
  never completed, so its last report lives only in its own
  `meta.live`; and `render::token_bar_in_
  flight_line`'s own text (shared with the Cost screen's own data,
  so the two can't disagree about what's running either).

Issue #649: the time card's own `eta_ts` is the same whole-run ETA
history gives the header, in place of the header's own fallback to
the innermost loop's ETA once there's enough history -- `eta_from_
history` carries the same three-way choice (history / past usual /
loop) the header line makes, so the two never disagree. Once it's
from history, the card also shows `eta_remaining_s` (`(12m)`),
`vs_usual_s` and `usual_total_s` (`vs usual +3m (p50 45m)`, the gap
between this run's own elapsed time at its anchor step and the usual
elapsed time to reach the same point) and the p10/p90 range as clock
times (`usual 11:58–12:09`); below the run-count threshold it shows
`history_count` instead (`history: 1 of 3 runs`). The now pane's own
p50/p90 bar (a running step's elapsed busy time against
`Header::history_step_usual`, pooled across every hash the same way
per-step figures always are) is `FocusedStep::usual_busy_p50_s`/
`usual_busy_p90_s`, filled in by `build_now_pane`'s own `NowFocus::
Running` branch only -- a ghost-bar history of passes, which needs
the bounded interval log below, is the Time screen's own addition
(issue #650). Below
[`tui::thresholds::TWO_COLUMN_MIN_WIDTH`]/`_MIN_HEIGHT` (80×20, the
same line screen 2's own two-column layout collapses at) the overview
restacks instead of just shrinking: the phase strip drops to one
line (status and label only, no start time or duration), the now
pane stacks the step over the log lines instead of splitting them
side by side, and the three cards become one plain summary line
(elapsed, effects, failures, tokens and cost) in place of their own
three bordered boxes. Below 100×30 with the three cards still shown
separately, each card keeps its own order and loses lines from the
bottom as height runs out, same as any other bordered pane here; a
value too long for its own card's width ends in `…` rather than
being clipped mid-word (`tui.rs`'s own `truncate_lines`).

**`--glance`.** `osi <doc> --glance` and `osi watch <dir> --glance`
show only the header line (`current: None` in `header_line`, so no
tabs at all), rewritten in place about once a second
(`GLANCE_INTERVAL`) over `\r` plus a clear-to-end-of-line, with the
final line kept on screen (its own trailing newline) once the run
ends. Signals and exit codes work exactly as `--log`'s own
`supervise_plain`/`finish_run` do — `run_glance`/`watch_glance` mirror
their forwarding and kill-deadline logic, just never printing a diff
line. `--log` and `--glance` can't be combined (a usage error, exit
2). `osi watch <dir> --glance --once` skips the loop entirely: one
read of whatever's on disk right now, one line, exit 0 — for a tmux
status line that runs its own command on a timer instead of leaving
one process attached; `--once` needs `--glance` (a usage error
otherwise). When stdout isn't a terminal, only the final line is ever
written, never the intermediate rewrites. `osi watch` names the
document from the watched run's own `runtime.last_run.
orchestration_path` once a live-state write has it (`model::
orchestration_path`), not from the directory or file it was pointed
at — `watch_document_name` recomputes this every rebuild, since the
first write to come may predate the document name landing at all.

### 6.3b The Cost screen (issue #632)

Screen 3, `Cost`: where a run's tokens (and money, where reported)
went, by state store and over time. Replaces the old `tokens by key`
treemap and token bar screen 2 used to draw (§6.3 above) with a
treemap, a stacked "tokens over time" chart and a "largest stores"
table, built only while this screen is actually on display
(`main.rs`'s own `build_cost_screen_if_active`, the same performance
rule the overview's own model follows).

**The by-store treemap** (`oscilloscope-core/src/treemap.rs`'s
`Measure`/`build_tree_for_measure`, drawn by `tui.rs`'s
`draw_cost_treemap`). A proportional-area chart, one tile per state
store (a path with a real `{value, meta}` node that is either a leaf,
or a `while: mode: model` loop container, which writes its own
`tokens_sent_total`/`tokens_received_total` (and, under `Measure::
Cost`, its own `cost_usd`) onto its own meta every pass, alongside its
real children -- the one case where a container's own tokens/cost are
not null, so it gets a leaf tile of its own, labelled `check`, next to
its real children rather than being silently dropped from the
treemap, the table and the overview's own top store alike). Unlike
screen 2's old treemap, there is no single wrapping `prime` root cell:
`build_tree_for_measure`'s own top-level tiles are the root's own
children directly, each already chain-folded. The default measure is
total tokens (sent + received); `i`/`o`/`$` switch it to input tokens,
output tokens or cost, and the same key again returns to total
(`Measure::from_key`). A node's own value for the chosen measure is
`model::node_tokens`'s sent/received sum, one side of that pair alone,
or `meta.cost_usd` scaled into micro-dollars (`treemap::display_cost_
usd` is the inverse, for display) -- the same integer-weight geometry
`squarify` already uses for tokens, so a cost-by-measure layout needs
no code of its own beyond which value feeds it. A container's own
total is the sum over its subtree, including its own `check` leaf
when it has one.

*Chain folding* (`treemap::fold_single_child`): a node with exactly one
child folds with it into one tile, `outer › inner`, applied to each of
a container's own children as they're collected (never to the node
itself, which is what keeps the top-level root from ever folding its
own single child away into itself, with no path-threading needed to
exclude it specially) -- the exact same rule `fold::kind_breadcrumb_
eligible` applies to the orchestration pane's own row-level
breadcrumbs, shared rather than duplicated: both sides' own `RowKind`
(from a path-to-`RowKind` map threaded in alongside the per-path
statuses, since a per-store node carries no `RowKind` of its own) must
be eligible -- not the document root, a loop container or a
quick-steps summary, and not still `Pending` -- *and* both sides'
status must agree, so a loop's own `n/total` progress never
disappears into a breadcrumb the way a true wrapper does.

*A store still running* (`completed_at` unset) that has not reported
anything for the chosen measure yet still gets a cell, with a nominal,
non-zero placeholder weight (`RUNNING_PLACEHOLDER_WEIGHT`) so it isn't
filtered out as a zero-weight node the way a plain tool (never
token-bearing at all, `is_token_bearing_kind`: `meta.adapter`,
`meta.prompt_sent`, or an agent tool's own `provider`) would be. That
bare placeholder weight is usually many orders of magnitude smaller
than its real siblings', though, and squarify would otherwise round it
under 2×1 and fold it straight into the merged `…` bucket -- invisible
at every level the folded weight bubbles up through, container after
container, however many are between it and whichever level it finally
competes against a real sibling at. `treemap::boost_running_
placeholders` runs at every level a container's children are
assembled: any child (leaf or container) still sitting at the bare
placeholder weight is boosted to its siblings' own median total above
it, so it always gets a real, visible share of the row. `tui.rs`'s own
rule is `cell.total == 0`, then the status, never the other way
around: `cell.total` (always the real, unboosted value) greater than
zero always shows the real number, regardless of status -- a
container with at least one real, finished child already shows that
real total, never hidden behind "tokens when done" merely because
something else under it is still running. Only once `total` is
actually zero does `status` decide the placeholder text: `Running`
reads "tokens when done"; `Aborted`/`Cancelled` (the identical data
shape a still-running leaf has, `completed_at` never written either
way, but never reaching "done") reads "no tokens reported" instead;
any other status falls back to the real number anyway (zero, read as
itself).
A store with no value for the chosen measure at all (finished, never
running) is left out, and `build_tree_for_measure`'s own second return
value counts how many -- the treemap's own title shows it (`3 stores
without cost`).

*Squarify's own cell-aspect correction.* A terminal's own character
cells are about twice as tall as they are wide, so a tile that is
actually square *in character cells* reads as visibly tall and
narrow on screen. `layout_into` stretches the y-axis by
`treemap::CELL_ASPECT` (2.0) before calling `squarify`, and divides
every resulting rect's `y`/`h` back down by it afterward -- every
area and position stays exactly correct in real character-cell units
(a uniform stretch of one axis scales every sub-rect's own area by the
same factor, so dividing back out after the fact is exact, not an
approximation), but the row-growth and orientation decisions
`squarify` makes along the way are now biased toward a tile that is
visually square, which is what "the worst aspect ratio" ought to mean
on a terminal.

Sibling tiles are still ordered largest-total-first, ties broken by
key (`treemap::sorted_children`, now `pub` so `oscilloscope`'s own key
handling can pick the same order `↑`/`↓` select tiles in). Merged `…`
buckets for too-small cells, bordered containers at 3×3 and up, and
the leaf/container fill rules are otherwise unchanged from screen 2's
old treemap (§6.3 above described them in full before this section
replaced it).

**Zoom and selection** (`keys::App::cost_zoom`/`cost_selected`,
`cost_level`). `↑`/`↓` move `cost_selected` among the current zoom
level's own tiles, in the same size order `sorted_children` lays them
out in; `⏎` pushes the selected tile's own path onto `cost_zoom`
(only once it actually has children -- a leaf has nothing further to
show), and `esc` pops one level back out. `cost_level` walks
`cost_zoom`'s own stack into the current measure's tree each time,
dropping any trailing entry that no longer resolves (the underlying
state changed since it was pushed) rather than leaving the screen
stuck on a level that no longer exists. The zoomed store's own *name*
at each level (not its full dotted path, which already contains every
shallower level's own as a prefix and would read `prime.fan › prime.
fan.iter_0`) is shown as a breadcrumb in the treemap's own title,
alongside the measure, which stays visible while zoomed rather than
being replaced by the breadcrumb.

**Layout caching** (`tui::CostLayoutCache`, DESIGN.md's own
performance rule: "laid out again only when the tree or the area
changes"). Owned by the main loop alongside `App`, not folded into
it, since it holds ratatui's own `Rect` and the laid-out
`TreemapCell`s -- neither of which `oscilloscope-core` (pure
geometry/model, no ratatui dependency) ever sees. Compares the
current level's own tiles and the pane's own area against what it
last laid out; `squarify` only actually runs again when either
changed.

**Tokens over time** (`oscilloscope-core::cost::tokens_over_time_by_
phase`, drawn by `tui.rs`'s `draw_cost_chart`). A stacked chart, one
band per top-level phase (the document's own top-level steps, already
breadcrumb-folded the same way the overview's phase strip is), on a
clock axis from the run's start to its own end (`header.run_end_ts`
once the run has one, `now` while it's still going -- a finished run's
chart, under `osi watch`, stops growing new buckets as real clock time
keeps advancing underneath it): each effect's own tokens (sent +
received) are counted into one of `cost::TOKENS_OVER_TIME_BUCKETS`
(24) equal time spans by its own `completed_at`, the same rule the
overview's own single-series sparkline (`screens::build_tokens_over_
time`) uses, just bucketed per phase instead of summed across the
whole run. `CostScreen` carries the exact start/end timestamps this
bucketing used (`chart_start_ts`/`chart_end_ts`), so the chart's own
clock-axis labels can never name a different span than the bars
themselves cover. The chart rebins those buckets across exactly as
many columns as the pane's own width has -- splitting one bucket's
value across every column it spans when the pane is wider than the
bucket count, or summing several buckets into one column when it
isn't -- rather than capping the chart's own width at the bucket
count, then fills each column's own rows bottom-up, per phase,
proportional to that phase's own share of the column's total -- the
nearest a character-cell terminal gets to a real stacked-area fill.
Each band also draws in its own fill glyph (`tui::phase_glyph`), not
only its own colour (DESIGN.md's own "never colour alone" rule), and
the bottom two rows (whenever there's room for them) are a clock axis
(start, middle and end, `format::midpoint_clock_time` for the middle)
and a glyph-and-label legend, rather than more bars. No projection to
the run's own ETA yet, though issue #649 now gives one -- a later
issue (#641).

**The largest-stores table** (`oscilloscope-core::cost::
largest_stores`, drawn by `tui.rs`'s `draw_cost_table`). Always ranks
by total tokens, regardless of the treemap's own selected measure --
built from the same by-store tree as the treemap (always at
`Measure::Total`), flattened to its own leaves (every leaf there is a
real, individual store, including a `while: mode: model` loop's own
`check` leaf alongside its passes -- unzoomed, that row's own label is
the loop's own breadcrumb plus `check` (`fan › check`); zoomed into the
loop itself, where there is no ancestor prefix left to strip, it reads
bare, `check`, [`cost::zoomed_row_label`]). Each row shows the store's
own breadcrumb label (the same chain-folded one the treemap's own tile
for it would show), relative to the run's own root unzoomed or to the
current zoom root once zoomed (re-derived from the row's own real path
rather than stripping a prefix back out of the root-relative label),
shortened from the left with a leading `…` when it doesn't fit the
pane's own width (`cost::shorten_breadcrumb_from_left` -- the leaf
segment, never the leading ones, is what actually has to stay
readable), tokens in/out, cost when reported, and its own share of the
table's total; a row with nothing reported yet reads "tokens when
done" (running) or "no tokens reported" (aborted/cancelled without
finishing) instead of `↑0 t ↓0 t 0%`, the identical rule the treemap's
own tiles use. `tui::draw_cost_table` filters those rows to whichever
level `App::cost_level` says is currently zoomed into (the same
self-healing walk the treemap itself uses) and reverses every row
whose own path equals `App::cost_selected` *or* descends from it (a
selected container has no row of its own unless it is also a `while:
mode: model` loop, but every row under it still needs to show the
selection) -- below the treemap's own minimum size, where the table is
the only thing on screen, this is what keeps `⏎` and `↑`/`↓` having a
*visible* effect, not only a change to hidden state. The footer always
stays visible, right after the rows -- with more rows than fit, rows
are what gets cut, ending in a `+N more` line, never the footer itself
scrolling off the bottom. Unaffected by zoom (it is about the whole
run, not a zoomed slice of it), it gives the run's own totals
(`runtime.last_run.totals`, falling back to summing every node's own
tokens directly while the run hasn't ended yet, the same fallback
`render::build_header` already uses), on its own footer line; the
coverage note, when there is one, gets a further line of its own
(`StoresFooter::cost_label_for_width`, shrunk to the pane's own width
by dropping the leading `$cost` figure before the `N of M` fragment
itself, the one thing a partial label exists to show, is ever lost);
and the gap between the run's own totals and the table's own sum,
when there is one -- a declared-outputs `use` child's own effects
count toward the run's totals as they complete but never land in the
final state tree at all (DESIGN.md §1.2), so the table can only ever
fall short of the run's own totals there, never exceed them.
The overview's own cost card shows the identical top store
(`cost::top_store`, read straight from `flat` without building the
by-store tree at all, ranked by the exact same rule the table's own
sort uses -- largest total first, ties broken by the lowest path --
so the two can never disagree even when a tie is involved, and
excluding the run's own root the same way `build_tree_for_measure`
excludes it from the Cost screen's own top-level tiles, since the root
carries its own tokens exactly like a `while: mode: model` loop's
container can and must never surface as "the largest store" in its own
run) as its own "largest store", so the two screens' rankings can
never disagree.

**Cost.** `meta.cost_usd` is shown wherever an effect reports it, on
any leaf, not only a prompt's -- a row with no `cost_usd` simply has
no `$` figure of its own. The table's footer sums it the same way the
overview's own cost card does (`run_cost_usd`, falling back to
summing every effect's own `meta.cost_usd` before the run has ended);
when some *finished* stores never reported one at all, the figure is
marked with its own coverage (`StoresFooter::cost_label`: `$4.20 (from
3 of 9 prompts)`, the issue's own wording), rather than reading as the
whole run's cost when it is only part of it -- a store still running
is never counted in that coverage figure either way, since it hasn't
failed to report a cost, it simply hasn't finished yet.

**Small terminals** (`tui::draw_cost_screen`, thresholds in `tui.rs`):

1. 100×30 and up (`thresholds::COST_CHART_MIN_WIDTH`/`_MIN_HEIGHT`):
   the treemap (62% of the height, `layout::golden_major_section`)
   above the chart and the table, side by side at 62/38;
2. 80×20 to just under that (`thresholds::COST_TREEMAP_MIN_WIDTH`/
   `_MIN_HEIGHT`, the same floor screen 2's own two-column layout
   collapses at): the treemap above the table alone, no chart;
3. below that: the table alone, where `⏎` still zooms it.

   Both thresholds are whole-*terminal* sizes, inclusive, checked
   against `frame.area()` itself (`draw_cost_screen`'s own
   `frame_area` parameter), never against `body` (the terminal minus
   the header's own single row) -- the same rule screen 2's own
   thresholds already follow.

### 6.3c Run history (issue #649)

`oscilloscope-core::history` is the one place that knows the record
format, the statistics and the ETA -- no file IO policy difference
from `observe.rs`, which already does its own; `oscilloscope`'s own
`history_io.rs` (the binary crate) is the only thing that ever reads
`OSI_HISTORY`/`OSI_HISTORY_DIR`/`$XDG_STATE_HOME`/`$HOME`, resolves
which document a run or watch is keyed by, and decides *when* to call
into it, the same split `config_discovery`/`main.rs`'s own config
resolution already makes.

**The record** (`history::HistoryRecord`): `v`, `run_id`, the
document's own absolute path (resolved by `history_io.rs`, never read
back out of `runtime.last_run.orchestration_path` as given -- neither
engine guarantees that one is absolute), `document_hash`, `engine`
(from `run_start`'s own `engine` field when `--events` carried one,
else `null`), `started_at`/`ended_at`, `outcome`, `wall_s`,
`effects_run`, tokens, cost, and `steps`: a map from each plan-known
path's own display key (loop passes written `iter_*`, a path the plan
doesn't know by name folded into one `(other)` entry) to its own
`start_s`/`end_s`/`busy_s`/`passes`/tokens/cost/`ok`. Only
`runtime.last_run`/`.totals` and each node's own
`created_at`/`completed_at`/token/cost fields are ever read -- no
value, prompt, param, command, tool target, error text, report, or
transcript content reaches a record (a test built from a state whose
every one of those fields holds a marker string asserts the marker
never appears in the serialized line).

**Where `busy_s`/`passes` come from** (`RunModel::step_intervals`, a
bounded log, issue #649's own "keyed by event id"): an id-paired
`start`/`end` event pair, when `--events` carried one for a path,
else the state-only "just finished" transition `observe` already
detects for a retried/looped path (`last_created_at` moving) --
never both, so the two sources can't double-count the same pass.
Grouping every interval by display key and merging overlapping
`[start_s, end_s]` ranges before summing is what makes a tree loop's
concurrent passes count their overlap once, for free, whether the
loop is named (each pass its own concrete path) or not (one path,
several passes in sequence). Capped at 50,000 entries; past that,
further passes only bump a per-path count with no timing -- a bound
the Time screen (issue #650) is the first thing expected to approach,
not this one.

**The file format:** one JSONL file per document, in
`$XDG_STATE_HOME/circuitry/osi/history/<sha256 of the absolute
path>.jsonl` (`~/.local/state/...` by default, macOS included --
one documented place on every platform). New directories `0700`, new
files `0600`, kept `0600` through compaction (a temp file in the same
directory, then a rename); an existing, already group-/world-
accessible directory is left alone, with one warning on stderr, same
rule as `--out-dir`'s own `create_run_dir`. Appends and compaction
take a blocking advisory `flock` on `<file>.lock`; readers never lock,
and tolerate a corrupt line, an unknown `v`, a torn last line, and a
line over 64 KiB, each skipped without losing the lines around it. A
repeated `run_id` (an `osi watch` filling in an already-recorded run)
counts once, the first copy read winning. Limits: 500 steps and 64
KiB per line, 50 runs per document (compacted to the newest 50 once a
file reaches 100 lines), 500 documents and 64 MiB in total (oldest-
written evicted first).

**Statistics and the ETA** (`history::compute_stats`/`compute_eta`):
nearest-rank percentiles over the newest 20 ok runs -- p50 needs 3,
p10/p90 need 5. Whole-run figures (`DocumentStats::whole_run`) use
only runs sharing the *current* `document_hash`; per-step figures
shown generically for a step (`DocumentStats::per_step`) pool every ok
run of the document's own path, any hash, so an edit that changes the
hash still has per-step history to show (just no whole-run one, until
3 runs of the new version exist). The ETA's own "what usually follows
this step" is a *different* map, `DocumentStats::anchor_remaining_
after_p50_s`, restricted to the same same-hash pool `whole_run` itself
comes from -- pooling every hash here, the way `per_step` does for
display, would let an edit that adds work after an anchor step make
the ETA too early. The anchor is the most recently finished step with
an entry in that map (`EtaInput::finished_steps`, built by
`history::eta_input_from_state` from whichever state the caller
already has) -- before any has finished, the run's own start plus the
whole run's p50; a currently running step is a floor under that (its
own usual time left, plus the p50 of what usually follows it), never
earlier than now; past the usual (p90) end, `None`, read by the header
as `ETA ?`. `EtaOutput::vs_usual_s` is the gap, at the anchor, between
this run's own elapsed time and the usual elapsed time to reach the
same point -- `None` before any anchor exists.

**When it's read and written:** `history_io::HistoryHandle` resolves
the document lazily for `osi watch` (the first live-state write that
names one, absolute `orchestration_path` else `--plan` else history
off for that watch, one stderr line) and eagerly for `osi <doc>` (the
CLI argument itself, canonicalized); reads a document's own file at
most once and recomputes statistics only when `document_hash`
changes. `apply_history_eta` is the one place `Header::history_eta_ts`/
`history_eta_past_usual` (and the time card's own extra figures) are
ever set to anything but their own "no history" defaults -- directly
on the `Header` `render::build`/`build_folded` already returned,
right after every real call to either, in `run_tui`/`run_tui_watch`/
`write_glance_line`. Once the run has truly ended (`model::run_ended`),
the ETA's own elapsed-time anchor freezes at `runtime.last_run.
completed_at` rather than the live clock -- a run held open in the
TUI after it finishes would otherwise watch its own ETA keep sliding
forward, eventually past its own usual end.

`HistoryHandle::write` is the one write path, called from all four
endings (`finish_run`/`finish_watch`, and both `--glance` loops, which
don't go through either finish function): a no-op unless `state`
satisfies `model::run_ended` (so an abort with no final write, and
`osi watch` detaching from a run still going, write nothing), and
`build_record` itself returns `None` for a dry run, a validate-only
run, or one with no `document_hash`. `append_record` itself skips the
write, under the same lock the append takes, when the record's own
`run_id` is already in the file -- not just folded away by
`read_records`' own dedupe, which would otherwise let a repeatedly
re-watched run directory grow the file forever. `osi watch --glance
--once` never calls `write` at all -- it only reads. A notice `history_
io::HistoryHandle::ensure_document` has (no absolute path and no
`--plan`) is queued, not printed straight away: it can run on every
redraw inside the TUI's own raw-mode, alternate-screen loop, where a
bare `eprintln!` would land as a stray line over the drawn frame;
`flush_history_notices` prints anything queued once that loop (or
`--glance`'s own rewritten-in-place line) is done.

### 6.3d The Time screen (issue #650)

Screen `1`: a clock-axis timeline, one row per folded step (the
same rows Flow and the overview already build), ghost bars from run
history, three strips (events/min, decisions, in-flight), a zoom
pane for the selected step, and a "vs usual" pane against history.
`oscilloscope-core::time` holds every pure computation (the axis's
own span, a bar's own cells and partial-block end, ghost bars, loop
collapse/expand, the strips, the zoom pane's own agent bands and
footer, and the vs-usual marks); `screens::build_time_screen` composes
them into one `TimeScreen`, built only while the screen is actually
on display, the same rule `build_cost_screen`/`build_flow_screen`
already follow. `oscilloscope`'s `tui.rs` is the only thing that
turns any of it into actual columns and styling.

**The clock axis:** start to the later of now and the ETA's p90 end,
or the plain ETA itself below the p10/p90 run threshold where there
is no p90 yet (plus a margin proportional to the elapsed span --
about 10% of it, floored at 2s, so a run only seconds old doesn't
have its own bars squeezed into a narrow strip on the left) while
the run goes, or start to end once it has. `format::clock_tick_seconds` picks the
round tick, by spacing alone, from one candidate list -- 1, 2, 5, 10,
15 or 30 seconds; 1, 2, 5, 10, 15 or 30 minutes; 1, 2, 3, 4, 6, 12 or
24 hours (only a divisor of a day -- a 5h, 7h or 9h tick would land
on a different local clock time every day, and drift further across
a daylight-saving change); then a whole number of days -- taking the
smallest that keeps labels about 10 columns apart over the axis's own
span and the pane's own width, with no preference for a round minute
over a tighter sub-minute tick that also satisfies the spacing. Ticks
fall on round clock times in the zone osi displays
(`time::axis_tick_columns`), each one recomputed from its own local
midnight rather than stepped from the last by a fixed duration
(`time::next_round_local_time`), so an hour tick lands on a local
whole hour even in a zone offset by a half hour from UTC, or across a
daylight-saving change -- never merely at the axis's own start, which
is whatever odd second the run happened to begin at. A sub-minute
tick is labelled `HH:MM:SS` rather than `HH:MM`, so a short run's axis
still has labels; once the axis's own start and end fall on different
local calendar dates, the tick at each local midnight is labelled
`MM-DD` instead of the ambiguous `00:00`. The now line (`┃`)
is drawn through every row and strip while the run is going, never
once it has ended -- the strips share the timeline's own gutter
exactly (one leading column for the timeline box's own left border,
since the strips have no border of their own), so the now column and
every tick line up between the bars and the strips in every frame.

**A row's own bar:** `time::bar_cells` places `[start_s, end_s]`
(run-relative seconds, the same offset `history::StepRecord::start_s`
uses) onto the axis's own columns -- full cells, then one partial-
block glyph (`▏`…`█`, `time::partial_block`) for the column the bar's
own end falls inside, in the row's own status colour. A running
row's own end is already `now` (a row's `duration_s` already counts
up to now while it runs, the same rule every other screen's own
duration column follows), so the only extra thing a running row
needs is a `▶` head in place of its own partial-block end -- one
column to the left of the now line rather than under it, so the two
stay visible side by side instead of the line simply overwriting the
head.

**Ghost bars** (`░`, `time::step_ghost_bar`): a pending step's own
p50 `start_s`/`end_s` from history (`history::StepStats::start_p50_s`/
`end_p50_s`, pooled across every ok run of its own path the same
"earlier versions" way `busy_p50_s` already is), shifted by
`time::vs_usual_shift_s` -- how far ahead or behind the run is at its
own ETA anchor. That shift is gated on `Header::history_same_hash_
count` reaching the same 3-run threshold the whole-run ETA itself
needs, not on `Header::history_vs_usual_s` directly: that field stays
`None` until some step has actually finished (`history::compute_eta`'s
own anchor) even with plenty of same-hash runs already in hand, so
gating on it directly hid every ghost bar until the first step
finished -- exactly when they matter most. Before any step finishes,
the shift is simply `0.0` (the anchor is the run's own start). There
are still no ghost bars below 3 ok runs of the same document version,
or without the step's own p50 figures. The last row, `planned`, is a
ghost from now to the ETA (`time::planned_ghost_bar`).

**Loop collapse and expand:** a loop row is drawn collapsed by
default -- its own bar already spans first-pass-start to last-pass-
end for a named loop (the loop container's own `created_at`/
`completed_at` already cover that). An unnamed loop's own body never
gets a `RowKind::Loop` row at all (§1.3: it writes no path segment of
its own, so it takes its enclosing scope's own kind, a leaf's); its
own passes, which therefore all share that one leaf's path, come from
`RunModel::step_intervals` by event id order (`#1`, `#2`, …) instead
of from `iter_N` children -- without `--events` they can't be told
apart, so the row stays one and `⏎` is a no-op (`TimeRow::
loop_expandable` is `false`). `PlanTree::is_unnamed_loop_body_path`
records an unnamed loop's own body paths wherever its `Loop` op is
reached while compiling the plan -- a chain sibling, a tree
`dynamic`'s own `Parallel` branch, or nested inside a named loop's
own pass (where the lookup normalises the pass's concrete `iter_N`
back to the `iter_*` placeholder the paths are recorded under) --
not only the one context a transparent op is ordinarily reached
through. With `--events` and more than one logged interval at the
leaf's own path, its collapsed row reads the whole loop's own span
too: its first pass's own start (the one thing state alone can never
answer, since each later pass overwrites the same path's node)
combined with state's own last-pass end (already extended to now
while it's still running, same as any other row's own `duration_s`,
never an event-sourced end of its own, which a step right after it
could start running before by a little clock skew between the two
logs), and a `⟲ N/M` progress count of every logged (finished) pass
-- while the loop is still running, `M` reads `?` instead, since the
total is never known until the last pass finishes, and the pass
currently in progress is never counted among the `N` done; once the
loop itself has ended, `M` is the real total. `⏎` toggles `App::
time_expanded_loops` (by the row's own path); when a loop's path is
in that set, `time::expand_loop` replaces the base tree's own
(breadcrumbed) rows for that loop's passes with the Time screen's
own: for a named loop, one row per `iter_N` index found under its own
path, each pass's own span aggregated from every flat node in its own
subtree (a chain each loop's own `iter_N` carries no `{value, meta}`
of its own in either engine's real state -- only its children do; a
tree loop's own pass container happens to have one, but aggregating
either way gives the same answer when it's the only node in its own
subtree) -- two tree passes running at once simply overlap, shown as
such, never merged away; for an unnamed loop, one row per logged
interval in event id order, plus one more, still running, for each
pass currently open (`RunModel::open_starts_at`) if the loop itself
hasn't finished yet. Past `RunModel::step_intervals`'s own 50,000-
interval cap, an unnamed loop's own `⟲` count and its expansion both
stop growing -- the same bound the interval log itself already keeps
(§6.3c). `time::loop_is_expandable`'s own unnamed branch requires the
leaf's own path itself (not merely its parent's) to be one `PlanTree::
is_unnamed_loop_body_path` says an unnamed loop's own body directly
contributes -- a sibling at the same parent path that merely happens
to sit next to the loop must never expand, or read its own progress
count, either. Drawing always hides every row under any loop's own
path from the base tree, collapsed or expanded, since the Time
screen's own pass rows (or lack of them) are what stands in for them
either way. Follow selecting a leaf whose own row is hidden this way
(a named loop's pass, collapsed or expanded) highlights and scrolls
to the nearest ancestor loop row that is actually drawn instead of
leaving the selection unmatched.

**The strips** carry raw points, not pre-bucketed heights --
`screens::TimeStrips` holds `events_points`/`decisions`/`in_flight_
intervals` as-is, and `tui.rs` buckets them (`time::bucket_weighted`/
`time::in_flight_heights`) at whatever width and `+`/`-`/`=` visible
window the bars themselves are drawn at right now, the same
`TimeLayout` both the timeline and the strips share each frame.
Bucketing once at a separate, fixed resolution instead couldn't keep
up with the bars' own zoom, and would let an in-flight strip run past
the now line. The now line
is overlaid on every strip the same way it is on every bar row.
- **events/min:** one point per logged start and end event, plus a
  currently running leaf's own start (`RunModel::step_intervals` has
  no entry for one until it finishes), plus -- past the interval
  log's own 50,000-entry cap -- the per-minute counts it keeps
  instead (`RunModel::overflowed_minute_counts`), folded in at each
  minute's own midpoint. Without `--events` at all
  (`RunModel::has_events`), the strip reads `(state)` and the
  same `created_at`/`completed_at` transitions `observe` already
  tracks stand in for events. While exactly one leaf is running and
  it's an agent (a `RowKind::Tool` with provider `agent`,
  `screens::is_agent_row` -- never only a `RowKind::Prompt`, which is
  an ordinary prompt and not necessarily an agent at all) and the
  strip's own rate over the last 60s has actually dropped
  (`time::quiet_agent`), not counting that leaf's own start event, it
  ends in `quiet = agent`.
- **decisions:** a `◆` at each named `if`'s own first `Decision`
  story line (`diff::LogKind::Decision`), one per path -- an unnamed
  `if`'s own inferred decision isn't marked, for now.
- **in flight:** a sweep over every *leaf* row's own `[start_s,
  end_s]` that itself has no child rows (`time::in_flight_heights`;
  neither a container nor a `use` step with running children of its
  own counts alongside the one leaf actually running inside it) --
  an unnamed loop's own body leaf contributes one interval per logged
  (finished) pass at its own path, plus one more for each pass
  currently open, rather than a single interval merging the whole
  loop into one, which otherwise undercounted a `flow: tree` unnamed
  loop (named and unnamed loops run their own passes the same way in
  either flow, §5) at a max of 1 regardless of how many passes were
  genuinely running at once -- ending in the
  run's own peak concurrency and, when any running node currently
  carries `meta.waiting_for` (`time::queued_now`), how many are
  queued behind a slot right now. The height in each column is the
  *most* intervals that actually overlap at a single instant inside
  it, not how many merely touch it -- a sweep over every interval's
  own start and end, ends processed before starts at an equal time,
  so a purely sequential run reads 1 throughout rather than 2 at
  every step boundary -- rounded to the millisecond first, since one
  interval's own end can come from state (microsecond precision)
  while the next's own start comes from `--events` (millisecond
  precision, §3's format table): without that, ordinary clock skew
  between the two logs can round-trip as a few microseconds of
  apparent overlap between two steps that never actually ran at the
  same time at all. Both the suffix figures and the start/duration
  columns drop below 90 columns, the same width the timeline's own
  columns do.

**The zoom pane:** the selected step's own `FocusedStep` (reused
from `screens::build_focused_step`, the same one Flow's own selected
pane already builds) decides what it shows, together with
`screens::TimeZoomExtra` (`build_time_screen`'s own extra data for
this one pane): a container's direct children, or a prompt/tool's
own p50/p90 marks -- both built only for the selected row, since
neither has anywhere else to be read from.

- The pane's own border title names the selected step (`zoom ·
  fan › pass 1 of 3 › leaf`), cut from the *left* (with a leading
  `…`) when it overflows, since the step's own name at the end is
  the identifying part -- the one thing the timeline itself can't
  show once a loop collapses its own passes out of it. A step under
  a loop gets that loop's own label prepended, whether the loop is
  collapsed or expanded.
- Every kind's own pane leads with one axis line -- start and end
  (or `now` while still running) as clock times, plus the duration,
  e.g. `03:23:52 → 03:23:57 · 5.0s`. The label column to the left of
  every bar or band is sized to the longest label the pane will
  actually draw (a container's children, or the agent's own band
  names), up to a third of the pane's own width, rather than a fixed
  width that left a loop's own passes all reading a truncated `✓
  pass…`.
- **A container** (`TimeZoomExtra::Container`): its direct children
  (one level of depth down, never recursed into) as rows with bars
  on the container's own axis -- its own start to its own end (or
  now). Past the pane's own height, the rest fold into one
  `+N more` line (`zoom_container_budget`) rather than overflowing.
- **A prompt or tool** (`TimeZoomExtra::StepBar`): one bar across its
  own start-to-end (or now) window, widened past that when needed so
  its own p50/p90 (`Header::history_step_usual`) both land on it as
  marks, with the actual figures underneath; `waiting for the
  reply` for a still-running prompt, `running` for a still-running
  tool.
- **An agent** (`FocusDetail::Agent`): the think band's own end, the
  footer's running-tool time, and this pane's own axis end all apply
  only while the step is actually `Running` *and* `meta.live` is
  present -- an interrupted run leaves `meta.live` behind with no
  `completed_at` of its own (issue #637), so gating on `meta.live`
  alone would keep all three growing by however long ago `now` is
  from the interruption; the axis end instead falls back to the
  row's own (frozen) span. `time::agent_zoom_bands` turns
  its own transcript's tool calls into bands by kind
  (`transcript::ToolKind`, the same classification `tool_glyph`
  already gives the overview's own activity strip -- moved into
  `oscilloscope-core` for exactly this, so the two screens never
  disagree), with a `think` band covering every gap between them
  (and from the session's own start to the first call, and from the
  last call to now while it's still running). Each kind draws as its
  own row, one band kind per line, since an agent can call the same
  kind of tool more than once in a session. When the transcript's own
  tailed window doesn't reach back to the session's own start
  (`transcript::TranscriptView::truncated`, from `observe::
  transcript_tail_truncated`), the earliest band's own row leads with
  a dim `…` instead of reading as a long, genuine gap. With no
  transcript at all, the pane falls back to a plain `running: <tool>`
  line from `meta.live.current_tool`, or the brief-window note
  (`screens::AGENT_LIVE_DATA_NOTE`) with neither. The footer's own
  turns/tool-calls/busiest-tool figures (`time::zoom_agent_footer`)
  read `meta.live`'s own `tools.<name>.ms` while running, or `meta.
  raw`'s `tool_counts` once finished, plus the call currently in
  progress's own elapsed time (`time::running_tool_elapsed_ms`, from
  the transcript's own unmatched `tool_start` or else `current_tool`
  and its own `started_at`) folded into that tool's own total --
  `tools.<name>.ms` alone only ever counts calls that have already
  finished, and so alone would read `bash 0.0s` for as long as the
  first call of a run is still going.
  Transcript tailing now also covers screen `1`, and a finished
  agent's own `meta.raw.transcript` (not just a running one's `meta.
  live.transcript`), since this is the first screen that ever shows a
  finished agent's own bands.

**"vs usual"** (`time::vs_usual_mark`): lists only the timeline's own
rows, in the timeline's order -- a collapsed loop is one row, with
the loop's own figures (history keeps one, summed across a chain's
passes or overlap-merged across a tree's); a loop's own pass rows
never appear here at all, collapsed or expanded, since history keeps
no figure for any one pass on its own. A step with 5 or more runs of
history is marked `▲`/`▼` outside its own p10/p90, else `≈`; with 3 or
4, more than 20% from p50 either way -- a still-*running* step never
reads `▼` purely for not having gotten as far as p10/20%-under yet,
only `≈` until it either finishes or actually passes the upper bound.
A difference under a second is always `≈`, on top of either rule --
without it, a step taking a few milliseconds reads `▲`/`▼` at random
from one run to the next, both the 20% and p10/p90 rules having
nothing but rounding noise to go on at that scale.
Below 3 ok runs of the current document hash, with no pooled step
history from earlier versions either, the pane reads `history: N of 3
runs of this version` instead of any rows (`time::vs_usual_too_few_
runs`). With some pooled history to fall back on -- including right
after an edit, where the current hash's own count is `0` -- the title
instead reads `vs usual · earlier versions`
(`time::vs_usual_is_earlier_versions`) and the footer carries no
finish time; with history off entirely, neither applies and the
title carries no suffix at all. The footer's own `from N runs` reads
the current hash's own count normally, but under `earlier versions`
that count is, by definition, always below the threshold -- it reads
the largest pooled count among the rows actually shown instead, so
figures genuinely pooled from several earlier runs never read `from
0 runs`.

**Zoom** (`App::time_zoom`, `+`/`-`/`=`): a fraction of the whole
axis, narrowed or widened by `tui.rs`'s own `time_visible_window`
around now (while following) or around the selected row's own bar
midpoint (otherwise) -- `None` (the default, and `=`'s own target)
means the whole run, with no panning in this screen at all.

**Small terminals:** 100×30 and up, the timeline with its strips
(62% of the height, `layout::golden_major_section`) above the zoom
pane and the vs usual pane side by side (62/38,
`layout::left_column_width`). 80×20 up to 100×30, the timeline and
strips alone take the *whole* body height (neither the zoom nor the
vs usual pane draws there at all, so reserving a golden-ratio minor
section underneath them would only leave it blank), with the
start/duration columns additionally dropped below 90 columns. Below
80×20, a plain list (status, step, start, duration), indented by
depth the same way the overview's and Flow's own small-terminal
lists are, no axis at all.

### 6.4 Tests

- **Golden tests, with `insta`.** Each fixture is a recorded run: plan, snapshot sequence with times, events, stdout/stderr and exit code. The fixtures are the §1 probes, with paths scrubbed. Two outputs are checked against goldens:
  - the status table after each observation;
  - the `--log` output.

  Cases cover every row of §2, including events-only, state-only and abort (no final snapshot).
- **TUI render tests (shipped, issue #434; the new layout and panes, issue #527; the Flow screen, issue #635).** `ratatui::backend::TestBackend` buffers (`oscilloscope/src/tui.rs`'s own `golden_tests` module, `insta`), one per state the acceptance list named: pending plan, running chain, a running tree loop with `max_concurrency`, a loop with two of its passes already done, queued-under-concurrency, failed, failed and handled, cancelled, aborted (no final state) and `osi watch` — at 100×24 (the two-pane layout: flow pane | selected pane, no story pane), since that is what most of these actually want to show. `pending_plan` and `aborted_no_final_state` stay at 70×20 deliberately, as this suite's own one-column-fallback coverage. `fallback_at_80x24_drops_the_state_pane` and `two_column_layout_at_100x30_shows_the_state_pane` cover the Flow screen's own three-size boundary: 80×24 is still two-pane (the story pane stays dropped), and 100×30 is the first size with all three panes, flow pane | (selected pane over story pane). `full_layout_with_state_pane_following`, at 120×40, is the most comfortable size, with every pane populated. `follow_picks_the_running_row_not_a_hard_coded_one` builds its own `App` and drives the selection through `App::follow_target`, the same way `main.rs`'s own redraw loop does, rather than hard-coding `selected_path` to a row that happens to be running — every other golden here does the latter, which (correctly) also turns follow off, since setting `selected_path` directly bypasses the key handling a real manual selection goes through. `a_long_plan_scrolls_to_keep_a_followed_effect_below_the_fold_visible` is a synthetic 48-leaf chain with the 45th leaf running and followed, at 70×20: the first leaf scrolls out of view, the followed one stays visible, and the border shows a `45/49`-style position. `folding_breadcrumbs_and_quick_steps` additionally covers a breadcrumb fold whose statuses agree staying folded (and one whose statuses differ staying apart), and three quick steps folding into one row with the right total. `flow_matrix_running`/`finished_at_120x40`/`100x30`/`80x24`/`70x20` cover the acceptance list's own matrix: a named decision taken with its untaken branch's dim summary, an unnamed decision inferred, a single-pass loop merged into one row, a quick-steps row, and a non-empty story, running and finished, at every required size.
- **Key handling (shipped, issue #434; the new navigation, issue #527).** Unit tests in `oscilloscope/src/keys.rs`: navigation, `PgUp`/`PgDn`/`Home`/`End`/`g`/`G` (against a ten-row synthetic plan, with an explicit `tree_viewport` standing in for `tui.rs`'s own last-rendered height), collapse/expand (with a trailing `.` so a same-prefix sibling is never hidden too), follow (on by default, and turned off by every manual move), errors-only, filter, `v`'s own prompt-first-then-cycle behaviour, both confirm dialogs (`c`, and `q` while a run is going), Ctrl-C's own action (checked before, and never merely dismissing, a dialog), the three-way `tab` cycle (including skipping straight past the state pane when it isn't drawn at all, `state_viewport == 0`), the state pane's own selection staying independent of the orchestration pane's, and the details pane's own scroll resetting whenever the orchestration selection changes — pure, with no real terminal. `tui.rs`'s own `scroll_tests` module unit-tests `clamp_scroll`/`scroll_indicator` and the state pane's line-aware `clamp_state_scroll` directly, beyond what the rendered goldens already exercise indirectly.
- **The render model (`oscilloscope-core/src/render.rs`).** Unit tests for the loop-pass multiplier/lower-bound on "effects planned", and the stderr tail's own reading order; `oscilloscope-core/src/model.rs` and `plan.rs` cover an unnamed `if`/`else`'s own two branches (neither pollutes the other's earlier-siblings, and an untaken branch resolves once the taken one is observed, rather than blocking every chain sibling after it forever). The state pane's own order, abbreviation (including a multi-line error showing only its first line), container token sums over a loop, a tree dynamic and a `use` child, are covered on synthetic snapshots; the token rule is pinned against each engine's own real recorded state for a retried `prompt_type: json` prompt whose first reply fails to decode (`model::tests::subtree_tokens_matches_run_totals_for_a_retried_prompt_on_cof`/`_on_electricity`); the token bar's own running-prompt filter (`waiting_for` excludes a queued prompt) and estimate are unit-tested; and the 2,000-node redraw measurement above lives here too. A multi-step tree- and chain-flow loop pass's own status, derived from its leaf descendants rather than defaulting to pending, is covered for the running/failed/done cases, alongside a regression test that a single-step pass still derives the same way (`a_single_step_pass_still_derives_done_from_its_one_leaf`). The token bar's own count is tested against a row confirmed running by events alone, with no state node at all (a tree-loop pass's prompt; a lower bound, since there is no `prompt_sent` to estimate from) and against one that cannot be classified as a prompt at all from path and events alone (a `use: inline:` child's, pinning that known gap rather than silently assuming it). "Effects planned"/"done" are tested against a real compiled `use: inline:` and `use: path:` document each, at every discovery stage (nothing yet, one child, every child, and a declared-outputs style `use` whose children never appear even once it finishes), always with done never exceeding planned.
- **The treemap (`oscilloscope-core/src/treemap.rs`).** Unit tests show that cell areas are proportional to totals within rounding, children stay inside their own parent's rect, a zero-weight key gets no cell (including a zero-total tree, which is empty, not an empty root cell), many tiny children merge into one `…` cell sized by their combined total (repeating the merge pass until nothing new needs folding in, for a survivor the second pass itself leaves too small), a container lays its own children out inside its own border, the same input always produces the same rectangles regardless of the order the caller happens to list it in (an input and a shuffled copy of it both tested against the same expected layout), and two equal-weight tiles in a rect wider than it is tall split by stacking rather than by columns once the terminal's own ~2:1 cell aspect is accounted for; `tile_aspect_ratios_stay_within_bound_across_distributions_and_pane_sizes` checks that bound itself holds across several weight distributions (equal, one dominant, a long tail) and several pane sizes at once, rather than one example at a time. Issue #632's own Cost screen additions: `build_tree_for_measure` reads `i`/`o`/`$`/total separately from the same node (`measure_switching_reads_in_out_and_cost_separately`), the same key toggling back to total (`measure_from_key_toggles_back_to_total_on_the_same_key`), a single-child chain folding into one breadcrumb tile when both ends are breadcrumb-eligible and their status agrees, staying unfolded when either isn't (including a loop container, which must survive zoomed-out even with a single pass, `chain_folding_follows_the_breadcrumb_eligibility_rule_not_status_agreement_alone`), a running store getting a placeholder cell instead of vanishing (and a running plain tool never getting one at all), a finished store with no value under the chosen measure being excluded and counted, a `while: mode: model` loop's own tokens getting a `check` leaf alongside its passes rather than being dropped, and a running leaf next to real-sized siblings still getting a drawable (non-merged) cell once boosted. `oscilloscope-core/src/cost.rs` covers the largest-stores table ranking by total tokens with a zero-gap footer, a gap when the run's own totals exceed the table (a declared-outputs `use` child's effects), the partial-cost label marking its own coverage once some but not all *finished* stores reported one (and having none at all once none have, no coverage note once every one has, and a still-running store never counted either way), the overview's own top store matching the table's first row, the tokens-over-time chart's own per-phase bucketing, and the stores' totals matching the run's totals on both engines' own recorded final state for a retried prompt (the same fixture `model::tests::subtree_tokens_matches_run_totals_for_a_retried_prompt_on_cof`/`_on_electricity` use). `oscilloscope-core/tests/cost_fixtures.rs` covers the same acceptance item against every recorded fixture with a `prompt` effect, loaded from its own `snapshots.jsonl` rather than hand-copied JSON, including that the retried-prompt fixtures' own `prime.ask` actually carries `retries_used == 1`, on both engines. `oscilloscope-core/src/format.rs` covers `midpoint_clock_time`'s own halving and its rejection of an unparseable timestamp. `oscilloscope/src/keys.rs` covers the Cost screen's own keys: `i`/`o`/`$` switching the measure and the same key returning to total, `↑`/`↓` selecting tiles in size order, `⏎` zooming into a store with children (and doing nothing on a leaf), `esc` zooming back out (and doing nothing at the top level), a stale zoom path that no longer resolves being dropped rather than sticking, and every Cost key being inert on any other screen. `tui.rs`'s own golden tests cover the Cost screen at 120×40, 100×30, 80×24 and 70×20, both running and finished, and the 2,000-node timing measurement (both building and drawing it, and that redrawing with nothing changed skips `squarify` by way of `CostLayoutCache`) covers it alongside the overview's own; further unit-level `tui.rs` tests cover the help overlay still fitting at 80×24, `header:tokens`/`tokenbar` tinting the cost card's own lines, the table alone (below the treemap's own minimum size) still reflecting a zoom and a selection already in effect, and an aborted leaf's own placeholder weight never reading back as a real token count.
- **Terminal restore (shipped, issue #434).** Unit tests in `oscilloscope/src/terminal.rs` prove `TerminalGuard`'s own entered/restored bookkeeping, and that the installed panic hook itself (not a direct `restore()` call) clears it through a real `catch_unwind`; `oscilloscope/tests/e2e_tui_pty.rs`'s `a_forwarded_sigint_still_restores_the_terminal` proves the real escape sequences on a real pseudo-terminal, after a forwarded SIGINT.
- **End-to-end with `cof`** (`tests/e2e_cof.rs`, plain mode; `tests/e2e_tui_pty.rs`, the TUI on a real pseudo-terminal via `posix_openpt`/`grantpt`/`unlockpt`/`ptsname` — no pty crate dependency), gated on `OSI_E2E_COF=1` (`cof` on `PATH` required once set):
  - the scripted adapter;
  - a temporary `HOME`;
  - credential variables removed (the four named in this repository's CLAUDE.md);
  - a hard timeout per test;
  - one test per probe shape, plus SIGINT once, SIGINT twice and SIGTERM, asserting exit codes and final statuses;
  - the pty tests additionally assert the alternate-screen entry/exit escape sequences, and that nothing osi or cof started survives, with no `osi-*` directory left in the real system temp directory (every pty test uses its own `--out-dir`); since the TUI now stays open on its own final state, every pty test that lets a run finish on its own sends `q` to leave, and three further pty tests assert SIGTERM (143), SIGHUP (129) and a raw Ctrl-C byte, `0x03` (130, since raw mode disables the kernel's own `ISIG`) in the TUI specifically.

  Live tests are never run.
- **End-to-end with electricity** (`tests/e2e_electricity.rs`, issue #431's lane E2): the same shape as `e2e_cof.rs`, against an `electricity` built with `--features test-tools`, behind `OSI_E2E_ELECTRICITY=1`.
- **The golden-section layout (issue #535).** `oscilloscope-core/src/layout.rs` pins `golden_section`/`left_column_width` at widths 80, 100, 120, 160, 200 and 300, plus `LEFT_MIN`. **The scope icon.** `oscilloscope-core/src/icon.rs` pins `trace_dots` at frame 0, that a later frame shifts the trace by one dot column, the flat line once ended (frame-independent), and that `--no-animation`'s own still frame (0) renders; `oscilloscope/src/icon.rs` pins the icon's own 3x17 shape, that it differs frame to frame while running, run-state colour (including aborted mapping to the cancelled trace colour), and `NO_COLOR`. **Update highlights.** `oscilloscope-core/src/changes.rs` covers the first-observation-silent rule, a row/state/log/token-bar/header change each, and elapsed time never counting; `oscilloscope/src/highlight.rs` covers a retrigger restarting rather than stacking, `prune`/`has_active`, and `style_for` at `t=0`/mid-fade/after the end for every colour mode including `NO_COLOR`; `tui.rs`'s own `highlight_tests` module pins a highlighted cell's real buffer background at `t=0` and that nothing overrides it once the fade has ended. **Colour themes.** `oscilloscope/src/theme.rs` covers every named theme defining every role, `terminal` painting no background, the xterm-256 mapping at the cube's corners and a mid-grey, the unknown-name error text, `t`'s own cycle order and wrap, `NO_COLOR`/`--theme`-beats-`OSI_THEME` resolution, and a per-theme WCAG contrast check (text/status/error at least 4.5:1, dim text 3:1, text on the highlight tint at `t=0` at least 4.5:1); `tui.rs`'s own `highlight_tests` module additionally pins a named theme painting an otherwise-untouched buffer cell's background. **The state tree and the details pane's sent/received blocks.** `oscilloscope-core/src/render.rs` covers nesting and key labels, a bare container with no node of its own, latest-or-current-first ordering (a running row before a longer-finished sibling), a bare container's own subtree token sum and empty dim, and that no value or error text appears in any row; and the sent/received blocks for a finished text prompt, a finished JSON prompt, a running prompt (the estimate and the waiting state), a `boolean` prompt's `meta.answer`, a prompt that failed before any reply (no `received` block), and a non-prompt node (no blocks at all).
- **Engine parity** (`oscilloscope-core/tests/engine_parity.rs`): a handful of documents using only `tool`/`json`, `dynamic`, a CEL `if` and `finally:`, recorded against both engines under the same config (`scripts/record_fixtures.py --parity-only`), assert the same `--log` lines apart from each effect's own duration.
- **The Time screen (issue #650).** `oscilloscope-core/src/time.rs` unit-tests every pure piece on its own: tick choice across spans from 30s to 6h, and that ticks stay about 10 columns apart at any width; `bar_cells`'s own partial-block end, an exact cell boundary (no partial glyph), and a running bar clamped to the axis's own end; `now_column` outside the axis; ghost bars shifted by `vs_usual_shift_s` (zero before any step finishes, `None` below the same-hash run threshold or after an edit with nothing pooled to fall back on), and `None` below the run threshold or with no shift to apply; loop expand for a named chain loop (including one whose own `iter_N` carries no `{value, meta}`, only its child does -- the real shape a live PTY run against `cof` caught), a named tree loop's two overlapping passes both keeping their own real span, double-digit pass indices sorting correctly against single-digit ones (`iter_9` next to `iter_10`, the one case a naive string-prefix check alone would get wrong), an unnamed loop's own body leaf (never a `RowKind::Loop` row, against a real compiled plan) expanding its passes by event id, and the same leaf with no `--events` staying one row; the events/min strip with and without events (and its own per-minute overflow tail folded in, and a currently running leaf's own start alongside a finished interval's), `quiet_agent`'s own lone-running-prompt-at-a-low-rate rule, decisions keeping only the first sighting per path, in-flight heights by overlap, and `queued_now`; the zoom pane's agent bands (gaps covered by `think`, a still-running call extended to now, no calls at all) and its footer's busiest-tool percentage; vs-usual marks at the 3- and 5-run thresholds (including that a still-running step never reads faster-than-usual early), `earlier_versions` correctly telling "too few runs at all" apart from "enough pooled history, just not of this hash", and the too-few-runs case. `oscilloscope-core/src/screens.rs` covers `build_time_screen` end to end against a real compiled plan: a running row's own bar and the now line, a loop row's passes spliced in only once `app.time_expanded_loops` names it, and `TimeZoomExtra` for a container (direct children only, not a grandchild) and for a running tool/prompt (`running`/`waiting for the reply`) and a finished one with pooled p50/p90. `oscilloscope-core/tests/time_parity.rs` mirrors `flow_parity.rs`: the same row paths/labels/statuses (never timing) on both engines' recorded fixtures, including a's and b's own quick-steps fold agreeing. `tui.rs` unit-tests `time_bar_string`'s own running head and failed end, `overlay_now_line`'s own head-stays-one-cell-left-of-the-now-line rule, `multi_segment_bar`, and `zoom_container_budget`'s own `+N more` reserve, all directly rather than only through a golden. `tui.rs`'s own golden tests cover the Time screen at 120×40, 100×30, 80×24 and 70×20, running and finished, with and without history, a collapsed and an expanded loop, and a running agent in the zoom pane with a real transcript (bands, footer, and a dim `…` once the tail is marked as not reaching the session's own start) and, separately, with none at all (`running: <tool>`); a 400-pass loop still collapses to one row, the existing 2,000-node timing test also builds and draws the Time screen within the same budget, and a second 2,000-node timing fixture uses real clock times rather than the shared fixture's unparseable ones, so its own timing actually exercises the bar/strip arithmetic rather than measuring blank rows. `oscilloscope/tests/e2e_tui_pty.rs`'s `pressing_1_during_a_live_run_opens_the_time_screen` presses `1` during a live `cof` run, checks a running row's own bar and the now line, navigates to a finished loop row and presses `⏎` for one row per pass, then `+` and `=`, each checked against its own incremental output rather than the whole buffer (which already contained everything either key's own check was looking for). `oscilloscope/tests/e2e_history.rs` gains an electricity-only run history test and one that pools a `cof` run and an `electricity` run of the identical document under one `document_hash`. `oscilloscope-core/src/history.rs` covers `build_record` folding overflowed passes into `(other)` once the interval log's own cap is reached.

---

## 7. Open questions (each with a recommendation)

Each row below is already settled: Q1-Q10 here are the same Q1-Q10 the
owner decided in issue #418's own "Taken here; the owner can override"
table, kept here with the reasoning rather than as still-open
questions. None of them are waiting on a decision.

| # | Question | Recommendation |
|---|---|---|
| Q1 | Add `--events` to `cof run` (§3) instead of using live state alone? | **Yes.** Without it, tree branches, `use` children and most running leaves are invisible (§1.4). |
| Q2 | Pull electricity's `--events` into M0-H? | **Yes.** `fire_effect_*` already exists there, and live state waits until M3-B. |
| Q3 | Default engine? | **`cof`** until electricity passes M1, with `--engine electricity` to opt in. Later, auto-detect. |
| Q4 | Show retries live (`effect_retry` event)? | **Later (v2).** It needs callbacks in the prompt and tool retry loops. v1 shows "retrying" for tools only, from `created_at` moving. |
| Q5 | Attach mode, `osi watch <dir \| live.json> [--plan doc.yml]`, for runs osi did not start? | **Yes in v1.** It costs little, and it covers the existing `--live-state <dir>/state.live.json` habit. |
| Q6 | Where do run files go by default? | **A temp dir**, printed at the end. `--out-dir` keeps it. |
| Q7 | How much prompt and reply text to show? | **None in the log** beyond a 60-character preview. Full text only in the details pane, on request. |
| Q8 | Should `cof` make interrupted effects explicit in state (e.g. `meta.error: "cancelled"`) instead of leaving `completed_at: null`? | **Yes, upstream, both engines.** Today a reader cannot tell "cancelled" from "still running" without the run-level error. |
| Q9 | Make tool retries record `retries_used` on failure, and settle on one `created_at` rule (first attempt or last) for tools and prompts? | **Yes, upstream.** Pick the first attempt for both, so durations include backoff. **Done for `cof` and for electricity's own tool retry loop (issue #421).** |
| Q10 | Windows? | **Not in v1:** process groups and signal forwarding are POSIX. |

The probes, the runner and the raw measurements are not part of this issue. They were run once from a scratch directory and can be recreated from §1's description.

