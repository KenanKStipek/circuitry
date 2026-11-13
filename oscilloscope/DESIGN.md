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
| `use` | `null` (full namespace) or `{<declared outputs>}` | `orchestration`, `inline`, `resolved_path`, `validation_errors`, `child_errors`, `inputs`, `orchestration_sha256`, `retries_used`, `attempts` (after a retry: one entry per attempt, oldest first) |

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
own `run_start` gets a line of its own too, `■ run started  <name>  <run_id>`.

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
19:56:24 ✓ flaky                 2.0s  retries 2  ↑3 ↓1
19:56:24 ▶ always_fails          shell ls /nonexistent-osi-probe
19:56:24 ✗ always_fails          /bin/ls failed (exit 1): ls: … (on_error: continue)
19:56:25 ✗ guarded.g_fail        /bin/ls failed (exit 1) …
19:56:26 ✓ guarded.g_cleanup     0.7s  (finally)
19:56:27 ■ run ok  5.7s · 12 effects · ↑3 ↓1
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
`ratatui` widgets. The Store screen's own by-store tree (`store.rs`,
§6.3e below) is deliberately not part of `RenderState` at all: it is
built (and its own layout, `store::layout_store`, pure integer
geometry, computed in `oscilloscope-core` and only ever drawn, never
computed, by `tui.rs`) only while that screen is actually on display,
not on every redraw regardless of which screen is showing.

**The tree screen that used to be here (issue #635) is superseded.**
The four paragraphs below describe screen 2 as it shipped under that
issue -- an indented tree read as control flow. Issue #691 (design v1
part 3, §6.3e below) replaces it with a left-to-right rail; nothing
in these four paragraphs, including the module paths they name
(`crate::flow::build_flow_rows`, the per-pane `s`/`l` swaps), ships
any more.

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
container row with that one pass row into a single `<status glyph>
⟲ <name>  pass N of M` line; more than one pass keeps one row per
pass, as the orchestration pane already did -- but the loop
container's own row then reads `<status glyph> ⟲ <name>` too, not
just its plain status glyph (issue #643): one rule for a loop row's
own glyph, merged or not, the same order the Time screen's own
`⟲ N/M` suffix keeps a loop's identity next to its status either way. The pane's own
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
    and no `completed_at`, `meta.adapter`/`meta.model` set and either
    `meta.prompt_chars` recorded or a non-empty `meta.prompt_sent`,
    and `meta.waiting_for` empty — a prompt merely queued behind a
    concurrency slot isn't processing yet (an event-only row can
    never be merely queued in the first place: `on_effect_start`
    fires only once a pass or branch actually begins). `prompt_chars`
    and the adapter/model pair come from `runtime.state.prompt_sent`'s
    own setting (#577), written together in every mode (`full`,
    `head:<N>`, `sha256`), so a prompt running under `sha256` -- where
    `meta.prompt_sent` stays `null` -- is counted exactly like one
    running under `full` (#587). State a cof build before #577 wrote
    never has `prompt_chars` at all; a non-empty `meta.prompt_sent`
    (there under `full`/`head:<N>`, never under `sha256`) still
    recognises those. A model-mode `if`/loop condition sets `adapter`/
    `model` too but never either, so it is never mistaken for a
    running prompt here.

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

**Keys** (issue #678: `←→`, `z` and `v` are gone from the Tape, which
has no pane or fold state of its own for them to act on; `tab` only
cycles its own pane focus there, with no effect on `↑↓`'s own
movement -- the table below still applies to Flow/Time/Cost as
shipped. `t` moved to `T`; the Tape's own keys, including its own
`t`/`p`, are §6.3e's):

| Key | Action |
|---|---|
| `↑↓`/`jk` | move (the focused pane's own selection, or the details pane's scroll) |
| `PgUp`/`PgDn` | move a page (the focused pane's own last-rendered interior height) |
| `Home`/`End`, `g`/`G` | jump to the first / last row |
| `←→` | collapse / expand (orchestration pane; Flow/Time/Cost only) |
| `⏎` | expand a quick-steps row (issue #613) in place; `←` folds it again |
| `z` | fold on/off (issue #613: breadcrumbs and quick-steps summaries); on by default; Flow/Time/Cost only |
| `f` | follow the running row (on by default) |
| `e` | errors only (keeps a failed leaf's own ancestors so there's still a tree to show it under) |
| `/` | filter (substring, on path or label; same ancestor-keeping rule) |
| `tab` | orchestration → details → state |
| `v` | full value, in an overlay; `v` again cycles between `prompt_sent` and the value when the row has both (opening on `prompt_sent` first when there is one); any other key closes it; Flow/Time/Cost only |
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
Store screen's own by-store tree (§6.3e) is a further, separate pass,
built only while the Store itself is on display, never regardless of
which screen is showing, the way screen 2's old treemap used to be.
Measured against a synthetic 2,000-node state, one
redraw stays well under `REDRAW_INTERVAL`
(`render::tests::redraw_from_a_2000_node_state_stays_well_under_the_
redraw_interval`). That measurement times `render::build` alone, which
leaves out the ratatui draw itself and the Store's own further pass;
`store::tests::build_store_over_2000_leaves_stays_well_under_the_
redraw_interval` times building the Store's own by-store tree and
table over the same scale separately.

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
in-flight line with the latter. The Store screen's own by-store tree
(§6.3e) is built only while that screen is on display, so it is not
part of `RenderState` at all and has no update-highlight key of its
own here. The first observation (`old:
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
unknown name from either is a usage error listing the valid ones. `T`
(issue #678 moved it from `t`, which the Tape now uses for its own
token column) cycles live through the eleven names in listed order,
wrapping, without rebuilding the render state; the header band shows
the new name for about 2 seconds, and the log pane gets a
`theme: <name>` line. `ColorMode`
(`theme::ColorMode::from_env`) is detected once at startup from
`NO_COLOR`/`COLORTERM`/`TERM`, in that priority order, and shared by
every theme choice for the run: truecolour keeps a named theme's RGB as
is; 256 colours maps every role to the nearest xterm-256 index
(`theme::nearest_xterm256`, the standard 6×6×6 cube plus the 24-step
greyscale ramp, picking whichever is closer); 16 colours falls back to
`terminal` with one log line saying so; `NO_COLOR` always wins, over any
theme, resetting every role to the terminal's own default — structure
still reads from glyphs and modifiers, never colour alone.

### 6.3a Screens and `--glance` (issue #626; every screen now superseded by §6.3e, issues #678/#688/#691/#695)

`oscilloscope_core::screens::Screen` names the four screens osi
switches between with `0`-`3`: `Tape` (`0`, the default for both
`osi <doc>` and `osi watch` -- §6.3e, replacing the old Overview),
`Signal` (`1`, §6.3e, replacing the `Time` this section once named),
`Flow` (`2`, §6.3e, replacing the §6.3's own tree layout above this
section once described) and `Store` (`3`, §6.3e, replacing `Cost`,
§6.3b below, now superseded). Selection, follow, folding, filters and
collapsed rows all live on `keys::App` outside the `screen` field, so
switching never touches any of them.

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

### 6.3b The Cost screen (issue #632, superseded by §6.3e's Store, issue #695)

Kept for its own history; screen `3` is the Store now, not Cost --
the treemap, the "tokens over time" chart, the measures (`i`/`o`/
`$`) and the zoom this section describes are gone entirely
(`treemap.rs` and `cost.rs` removed, `path_breadcrumb_relative_to`
moved to `format.rs`), not just superseded in place.

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
keeps advancing underneath it): an ordinary effect's own tokens (sent
+ received) are counted into one of `cost::TOKENS_OVER_TIME_BUCKETS`
(24) equal time spans by its own `completed_at`, the same rule the
overview's own single-series sparkline (`screens::build_tokens_over_
time`) uses, just bucketed per phase instead of summed across the
whole run. An agent session (`meta.provider == "agent"`, issue #641)
is different, since one can run for most of a document's whole span:
while osi actually watches it, `RunModel::record_live_token_
observations` (called alongside `observe` on every rebuild) keeps
every distinct `meta.live.tokens_sent`/`tokens_received` reading it
sees, each at the instant osi observed it -- `tokens_over_time_by_
phase` counts each sample's own increase over the one before it into
the bucket for its own observation instant, not the session's own
`completed_at`, and once the session finishes, reconciles the
difference between what was actually observed and its own final,
authoritative total (`model::node_tokens`) into the bucket at its own
`completed_at` -- the chart's own total for the step always ends up
exactly right either way. A session never observed running at all (no
recorded samples: a finished run opened fresh, or a watch that only
started after it ended) instead spreads its own tokens evenly over its
own span, `created_at` to `completed_at`, proportional to how much of
each bucket's own clock-time window that span overlaps. `CostScreen` carries the exact start/end timestamps this
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
and a glyph-and-label legend, rather than more bars. With more
top-level phases than `tui::PHASE_GLYPHS` has distinct glyphs for
(issue #641), `cost::cap_phase_bands` keeps the `PHASE_GLYPHS.len() -
1` phases with the largest total (summed across every bucket), in
their original relative order, and folds every other phase into one
further `other` band (bucket-wise summed), appended last -- a glyph
is never reused for two different phases in the legend. No projection
to the run's own ETA, though issue #649 now gives one.

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
readable; the nearest `iter_N` to the leaf, shown as `pass N`, is kept
too even once the plain segment itself drops away, issue #641 -- two
loop passes whose own breadcrumb agrees on everything below their own
`iter_N` would otherwise shorten to the exact same text), tokens
in/out, cost when reported, and its own share of the table's total
(`format::format_share_pct`: `<1%`, never a plain `0%` indistinguishable
from an actual zero, issue #641); a row with nothing reported yet reads
"tokens when done" (running) or "no tokens reported" (aborted/cancelled
without finishing) instead of `↑0 t ↓0 t 0%`, the identical rule the
treemap's own tiles use. `tui::draw_cost_table` filters those rows to whichever
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

**`config`/`run_dir`** (issue #698's own part-5 groundwork, format
`v` 1, `#[serde(default)]`, so a record written before these existed
still reads): `config` is the config path exactly as given on the
command line, never resolved further and never the config's own
contents; `run_dir` is the run's own, already-absolute run directory,
so "Recent Runs" (a later part of the same design) can still open a
finished run read-only (`osi watch <run_dir>`) long after any
registry entry for it (§6.3e below) is gone. Both are `None` for a
run with neither, or for any record that predates them.

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

### 6.3d The Time screen (issue #650, superseded by #688)

Screen `1` is the Signal now (§6.3e); this section is kept as the
record of what shipped here before #688 replaced it, not of what
`osi` still does. A clock-axis timeline, one row per folded step (the
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
is whatever odd second the run happened to begin at. A candidate
whose own local reading, read back from the zone, disagrees with what
was actually asked for means the offset itself changes somewhere in
between -- `time::offset_transition_between` binary-searches for
exactly where, and the whole computation restarts from there (issue
#654): a "spring forward" gap skips the requested elapsed time
outright (it never existed), while a "fall back" repeat means an
earlier elapsed time, already behind by the stale reading this
started from, is due again under the new offset -- without this, the
whole repeated local hour got no ticks under an interval shorter than
it (an hour-or-longer tick happened to round-trip either way, so this
never showed there). A sub-minute
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
count, either. A `⟲` count itself needs more than one logged interval
or open pass at the path, the same "more than one" rule `loop_is_
expandable` already requires -- a path reached just once by something
else entirely (an `if` whose `else` branch happens to share a path
with the `then` branch's own loop) reads no progress count at all,
not `⟲ 1/1`. A named loop sitting directly inside an unnamed loop's
own body shares that body's path, but it is still a real
`RowKind::Loop` row with real progress of its own -- the outer loop's
pass count never replaces it. Between two chain passes, this path's
own row can read plain `Done` for a moment, right after one pass ends
and before the next one's own `Start` event lands -- whether the
total is still unknown is decided from the loop's own running state
(a running-type status, or an event-sourced open start events already
know about) rather than from the row's own momentary status, so the
total never flashes a false `N/N` in that gap. Drawing always hides every row under any loop's own
path from the base tree, collapsed or expanded, since the Time
screen's own pass rows (or lack of them) are what stands in for them
either way. Follow selecting a leaf whose own row is hidden this way
(a named loop's pass, collapsed or expanded) highlights and scrolls
to the nearest ancestor loop row that is actually drawn instead of
leaving the selection unmatched.

A run that stops without finishing every pass still shows each one
honestly. A pass still open when the process is simply killed outright
(no `run_end` at all) keeps growing towards `now` on every other
screen's own fallback, but the Time screen instead freezes its
duration at the run's own resolved end (`header.run_end_ts`, the same
fallback used elsewhere once a run has stopped) -- both for a named
loop's own still-running pass and an unnamed loop's own last open one.
A pass an interrupted `run_end` cancels mid-flight is cleared from
`RunModel::open_starts_at` the moment the run ends, so an unnamed
loop's own expansion keeps its own start time separately
(`RunModel::cancelled_pass_starts_at`) and still lists it as its own
row, with a cancelled status, rather than letting it silently vanish
from the list.

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
  last call to now while it's still running). An interrupted run's
  own leftover transcript can end in a `tool_start` with no matching
  `tool_end`, and no later `tool_start` of its own to mark it
  `stopped` either -- `agent_running` (not the call's own
  `is_running` alone, which reads the same whether the session is
  genuinely still going or the run simply died mid-call) is what
  decides whether that last band reads running or frozen at the
  pane's own axis end. Each kind draws as its
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

### 6.3e The Tape and the shared language (shipped, issues #678, #688, #691, #695)

`design/tui-design.md` is the intent for osi's screens; this section
describes what has actually shipped of it so far -- parts 1 through
4 of the epic (#677): colour roles, the shared chrome, the history
median-tokens query, the Tape (`0`), replacing the Overview, Signal
(`1`), replacing Time, Flow (`2`), replacing the tree, and Store
(`3`), replacing Cost (§6.3b, now superseded).

**Colour roles.** `theme::Theme` grows the design's token set (its
§6 table) for every shipped theme: `bg_inset`/`bg_surface`,
`fg_muted` (`fg.faint` stays `dim`), `accent_primary`/
`accent_secondary`, `selection_bg`/`_fg`, `status_info`,
`statusbar_bg`/`_fg`, `keyhint_key`/`_desc`. Two changes apply to
every theme: `status_running` now equals `accent_primary` (running
is the primary accent, not yellow), and `status_pending`/
`status_skipped` both equal `dim` (pending, queued and skipped share
`fg.faint`); `status_failed_handled` equals `warning`, and
`status_cancelled`/`status_aborted` equal `error`.
Catppuccin Mocha, the theme the design's frames are drawn in, is
pinned field-for-field to `design/theme-catppuccin-mocha.rs`
(`theme::tests::mocha_matches_the_design_reference`); the other ten
themes take the same roles from their own published palettes by the
same table, with a theme that has no obvious value for a role taking
Mocha's own ratio to a value it does have (`bg.inset` 0.8× `bg.base`,
for one). 256- and 16-colour degradation and `NO_COLOR` cover every
new role the same way they already covered every existing one.

**History's median tokens.** `history::StepStats`/`RunStats` grow
`tokens_p50`, the median of tokens sent plus received -- per step
path (pooled the same "earlier versions" way `busy_p50_s` already
is) and for the whole run, over the same runs and the same 3-run
threshold as every other `_p50` figure here. A loop pass is also
kept by its own literal, un-stripped path (`history::display_key_
literal`), alongside the existing pass-stripped one the Time
screen's ghost bars read: the stripped key's own per-run sample is
now that run's own tokens *divided by that run's own pass count*
before the median is taken across runs, so it reads as one typical
pass's size whether a loop pass's own literal key has three runs to
go on yet or the Tape falls back to the stripped key for it; a
non-loop step's own pass count is always 1, so this changes nothing
for it. A literal key is capped at the first 8 passes a loop records
in any one run -- past that, a pass keeps contributing to the
stripped key's own sum, it just never earns a literal key of its
own -- and an evicted literal key's own tokens are never folded into
`(other)` a second time on top of its already-counted stripped
sibling's; both guard a loop long enough to otherwise grow one
record past its own 64 KiB line cap. Pinned against the design's own
fixture history (`design/fixtures/hist`): the non-loop steps, the
whole run's own total, and each of a tree loop's own three passes
individually, all match the design builders' own summary (`hist/
history.json`) exactly. The whole-run figure reaches the header
through a new `Header::history_usual_total_tokens`, set in
`history_io::apply_history_eta` from `DocumentStats::whole_run`'s
own `tokens_p50` -- the Tape's `+12%` Totals deviation and `usually
1,324 t` Run End both read it from there, not a `_total` step key
that never existed.

**The Tape** (`screens::TapeScreen`, `build_tape`) replaces the
Overview as screen 0. Built from the same inputs `render::build`
already has, same as the Overview's own model was: no engine change.
Its own list (`screens::TapeItem`) is, top to bottom: `RunStarted`;
every row that has either finished or (a decision or loop) been
dispatched, in finish order (a container ties with its own child at
the same instant, broken by the plan's own depth-first order, the
same order `render::build_folded` already returns rows in); the Now
band while the run is live; every leaf actually running right now,
bold; the slots rule for a loop with a busy pass; every row still
ahead, in plan order, with `usually N t` from history when it has
enough; `RunEnd`. A row's own text is read straight off its own
flattened node (`model::NodeMeta`) and the compiled plan, never
through `render::details_for` (which re-flattens the *whole* state on
every call) -- the Tape asks for one row's worth of text for every
row on screen, not only the selected one, so paying `details_for`'s
own cost per row would turn one redraw of a long run back into the
O(n²) walk issue #637's own budget test exists to catch. The token
unit (`screens::tape_unit`) is the smallest of 10/20/50/100/200/500
that keeps the usual largest step's own history inside eight bar
cells by the same rounding the bar itself draws with, `50` without
enough of it -- the design's own fixture history gives the largest
step (`summary`) a usual 402 t, which still rounds to a full 8-cell
bar at 50 t/cell, so every frame's own `▰ = 50 t` title is this
derivation's real result for that history, not a separate fixed
default kept alongside it. The detail pane reuses
`screens::build_focused_step`/`FocusedStep` -- the same model Flow's
and Time's own selected panes already build theirs from -- plus the
selected row's own Usual (history's median and the run count behind
it) and Share (percent of the run's own tokens so far) lines, which
neither of those two needs.

**The list's own window** (issue #710). While the head is Starting or
Normal, the Now area (the Now band through the last running row, with
any preview lines under them, and the slots rule; in Starting, the
Starting rule alone) sits at the list's own vertical centre whenever
it is shorter than the list: `tui::tape_list_window_start` places the
Now area's own middle row on the list's middle row, rounding down on
an uneven split, by starting the window at `now_area_start - (L - h) /
2` (`L` the list's own height, `h` the Now area's own) -- negative, or
past the real content's own end, is exactly the window's own blank
padding above or below, drawn from the frame's own blank-row
convention; a preview line landing at the window's own top with its
own row scrolled off above it draws blank there instead, never
floating on its own. A Now area at least as tall as the list itself
shows from its own first row while the window is centred. With
follow on, the centring is recomputed on every draw; with it off,
`App::tape_list_start` keeps the window where it last was across
draws, moving only as far as the selection needs to stay in view --
this keep-in-view rule applies however tall the Now area is, so a
Now area at least as tall as the list no longer pins the window to
its own first row once follow is off. The one exception is the very
first draw, before `tape_list_start` is ever set, which centres
regardless of `follow`, the same as if it were on.
Ended and Failed keep the list's own original rule: biased toward the
tail, scrolling up only as far as it must to keep the anchor (the
selection, or the last line while following) in view. Six of the nine
`tape--*` frame goldens move their own rows to this rule (`tape--
normal`'s own three sizes, `tape--usual`, `tape--help`, `tape--
starting`, all at 120x30); `tape--ended`/`tape--failed`/`tape--
toosmall` are unaffected.

**A running prompt's own live figures** (issue #675, on top of #674's
`meta.live`). `model::PromptLive::from_value` parses a streamed
prompt's own live record -- told apart from the agent tool's
`AgentLive` by the one key only a prompt's ever carries, `adapter`
(the opposite of `AgentLive::from_value`'s own check, unchanged) --
into its own struct (`v`, `attempt`, `adapter`, `model`, the two
timestamps, `streaming`, `first_token_at`, the sent/received pairs
each exact-or-estimate, `chars_received`, `tokens_per_second`), kept
separately on `NodeMeta` as `prompt_live`. `--events`' own `progress`
line (`observe::Event::Progress`; `id`, `path`, `ms`,
`tokens_received_estimate`, `chars_received`, `tokens_sent`,
`tokens_sent_estimate`, `first_token_ms`) is kept, one per path, on
`RunModel` (`progress_at`), cleared at the path's own next `start`.
`model::prompt_live_figures` reads either source into one shared
shape, `PromptLiveFigures` -- state's own `meta.live` when a node has
one, else `osi watch`'s own events-only fallback, which has no
`streaming`/`attempt` of its own at all (a `progress` line is only
ever emitted for a streaming adapter's own delta) and derives its own
rate from `ms`/`first_token_ms` by the same two-delta, 500ms gate
`meta.live`'s own `tokens_per_second` already uses.

A running prompt's own Tape row (`screens::tape_running_prompt_what`/
`tape_running_prompt_tokens`) reads this attempt's own elapsed
(`live.elapsed_s`, since its own `started_at`, winning over the row's
own `duration_s`, which counts an earlier attempt's backoff too) and,
on a retry, `attempt N+1`, first: the row's own `what` column is cut
from the right the moment the pane is too narrow for all of it, so
whatever reads first is the only part guaranteed to survive. After
that: `waiting for the first token` before the first delta, else the
received count (exact once the provider has reported one, `≈` while
it is still the running estimate) and the rate once there has been
enough of a second delta to measure one; a non-streaming adapter has
none of that to show, only the elapsed time and attempt. The token
column's own bar and figure (`TapeTokens::live`) draw in the faint
style for exactly as long as they are this running estimate, settling
to the normal style the instant the same row's own tokens next read
the node's finished, exact pair instead -- the two are mutually
exclusive by construction (`render::build_header`'s own `tokens_sent_
live_estimate`/`tokens_received_live_estimate`, summed only over a
node `model::is_running_prompt` still holds true for, add to the
Totals row the same way, in the same faint style, and read zero again
the moment a node it was counting finishes), so a prompt that ends
between two polls is never counted as both running and done. The
detail pane's own Live block (`tui::tape_normal_detail_lines`) shows
First Token, Received, Sent and Attempt in place of the Tokens line
(which has nothing to show for a node with no exact figures yet
anyway), or `no live figures from this adapter` for a non-streaming
one; a node whose own `meta.live` fails to parse at all shows as
before, the bare elapsed time, never a crash. None of the nine
`tape--*` frame goldens changes: the design's own fixture run has no
running prompt with a `meta.live` of its own to begin with.

**A running or finished agent's own activity** (issue #703, on top of
#637's `meta.live`/`meta.raw`). A running agent's own Tape row
(`screens::tape_running_agent_what`) reads the elapsed time, the
turn (`repair` in its own place during the repair turn, an `attempt
N` segment added on a retry), the current tool with its own elapsed,
and context usage (`/200k` once `context_window` is known) -- in
that order, so the row's own cut-from-the-right rule (`tui::
tape_agent_what_text`) drops exactly this list's own tail first,
a whole ` · `-separated piece at a time, never half of one (`·
ct⋯`) the way a prompt row's own reply preview still cuts by
character -- only once a single remaining piece still doesn't fit
on its own does this fall back to cutting it by character too.
Before the agent tool's very first `meta.live` write, the elapsed
time alone; a stale leftover (`AgentLive::is_recent` false, #637's
interrupted-run case, including one an interrupted run itself left
behind) reads `stopped` in place of the tool and context figures.
A finished agent's own row (`screens::tape_finished_agent_what`)
reads its turns, tool calls and files/lines changed from
`meta.raw`, same cut-from-the-right rule; a failed session keeps
the existing error text, never this summary.

One Agent block (`tui::agent_block_lines`) is shared by every pane
that shows a running or finished agent step: the Tape's own detail
pane (`tape_normal_detail_lines`, right after its own Tokens/Usual/
Share lines, which still read an agent step's totals the same as any
other tool), the full-screen step shared by the Signal and the Store
(`draw_focused_step_detail`), and the Flow screen's own Selected pane
(`flow_selected_step_lines`) -- replacing that pane's own former
`agent_live_lines`, which only ever covered a running session.
`main.rs`'s own `tail_focused_transcript` tails whichever of the
four screens is actually on display, the Store included. Each field
(Asked, Turn, Tool, Tools, Context, Files, Tests, Said -- the
transcript's own `last_reply` -- and Cost) reads from `meta.live`
while running, the matching `meta.raw` field once finished, and is
left out entirely when the engine never reported it, never shown as
a bogus zero. Below the fields, the transcript tail
(`observe::tail_transcript_lines` plus `transcript::
build_transcript_view`) lists calls oldest first, a running one with
its own elapsed, an ended one with `✓`/`✗` and its own duration --
kept to as many of the newest calls as the caller's own row budget
allows (the pane's own height, minus the fields above it), with a
leading `…` once either the read window or that budget cut the list
short.

The Flow rail's own stations (`rail::prompt_headline_detail`/
`tool_headline_detail`) grow the same live figures: a running
prompt's headline is its received figure (`Headline::Received`,
`↓≈212 t`), its detail the rate (`Detail::Rate`), `waiting`
(`Detail::Waiting`) before the first delta, or the attempt number on
a retry (`Detail::Attempt`) -- a state-backed, non-streaming adapter
never gets a first token at all, so its own station reads elapsed
time and attempt only, the same shape the Tape's own row already
reads for one; a running agent's headline is its turn
and current tool (`Headline::AgentTurn`, `turn 4 · bash`), its detail
context usage or files/lines changed, whichever it has
(`Detail::AgentActivity`); a finished agent's station reads exactly
like a finished prompt's, tokens and the bar. While following
(`screens::default_follow_path`), the Selected pane lands on
whichever station is actually running, not only on the default pass
-- the default pass still stands in when that running step is itself
inside a loop pass, or when nothing is running and nothing is
selected either.

None of the frame goldens (Tape, Signal, Flow, Store) changes: the
design's own fixture run has no agent step at all, and electricity
has no agent tool to begin with.

**A running agent's own tokens count the same way everywhere**
(issue #702, on top of #637's `meta.live` heartbeat and #675's
prompt figures above). `model::step_tokens_so_far` is the one shared
read every sum and share now goes through: a node's own exact
`node_tokens` once it has finished, else a prompt's
`PromptLiveFigures` or an agent's `AgentLive.tokens_sent`/
`tokens_received` -- read exact, never an estimate, even mid-turn; a
stale leftover (`AgentLive::is_recent` false, the interrupted-run
case #637 already parses) still returns its last known figures, just
with `live` read `false` rather than `true`. `tokens_received` stays
`None`, never `0`, for Claude Code's own mid-turn `null`.

`render::build_header`'s own `tokens_sent_live_estimate`/
`tokens_received_live_estimate` now sum a running agent's tokens the
same way they already summed a running prompt's, gated on the same
`AgentLive::is_recent` recency check `step_tokens_so_far` uses --
never merely on whether the run as a whole still reads `Running`, so
an agent whose own heartbeat has gone stale while the rest of the
run carries on stops counting as live at the same moment `step_
tokens_so_far` would call it stale. A leftover whose own heartbeat is
stale (the interrupted-run case #637 already parses, or simply a
still-running process whose one agent went quiet) instead folds
straight into the exact pair, since nothing is updating it any more
to call it live -- `runtime.last_run.totals` never carries it (the
attempt never finished), so without this a cancelled run's own agent
tokens would simply vanish from the Totals row. Every share (the
Tape detail pane's own Share line, the Store table and Readings) now
divides by this same live-inclusive total, never only what has
actually finished, which previously let a running agent's own share
read far past 100% (a run with 35 t finished and an agent already at
6,650 t once read `19000%`); the Store's own denominator
(`store::build_store`) reads the identical header figure the Tape's
share already does, rather than re-deriving it from state, so the
two screens can't disagree about the same step's own share even for
a cancelled run whose agent never finished an attempt. The Store's
own tiles (`store::build_leaf`) read the same helper, so a stale
leftover there also stops drawing faint once it stops updating. The
Tape's own token column (`screens::tape_row_tool_tokens`) now draws
for any `tool`/`use`/`yield`/`dynamic`/`finally` row with tokens, not
only a `prompt` row -- an agent's finished row reads a bar and figure
like a prompt's; a running one draws faint -- and the run summary's
own Largest (`tape_largest_tokens`) picks it up the same way. The
detail pane's own Tokens line (`screens::build_tape_selected_
detail`) carries `None`, not a false `0`, for whichever of sent/
received an agent's own CLI has never reported at all (Claude Code's
mid-turn `tokens_received: null`), drawing `—` for it rather than a
wrong figure; the Store's own tile (`store::build_leaf`) carries the
same distinction for the same reason, wherever a running agent's own
sent/received split is shown on its own, rather than summed into a
screen's own totals (which still read a missing figure as `0`, the
same rule every sum and share in this section already follows). The design always prints the count in full, with a
space before it: the bar's own 16-cell budget now gives up cells as
the count widens, dropped entirely once even one cell would no
longer leave room for both it and that one required space, rather
than let the pane's own clipping cut the count instead (an agent's
own total, cached input tokens included, routinely reaches six
digits or more).

**The Signal's own agent trace** (issue #702, replacing the single
Now-column estimate an agent otherwise would have gotten).
`RunModel` keeps the last live totals seen per running agent path
(`observe_agent_live`, called from `observe` for every flattened
node): each increase since the last sighting becomes its own entry
(`AgentLiveSighting`), at the live record's own `updated_at`, and
adds to `agent_live_plotted_total`, a running total of everything
already turned into a sighting for that path that a retry never
clears; once the session ends, only whatever `node_tokens`' own
final total still owes beyond that running total is added, at its
own `completed_at` -- so the sum of one path's own sightings always
equals its final tokens exactly, never double-counted, retries
included: an abandoned attempt's own tokens are already in its own
sightings, so the retried attempt's own eventual remainder
(`node_tokens`'s own `_total`-first pair, which folds both attempts
in) subtracts them rather than counting them twice. A session `osi`
never once saw running (`osi watch` of an already-finished run, an
attach after the fact, or one shorter than one poll) has nothing in
`agent_live_plotted_total` yet, so its whole `node_tokens` becomes
the remainder the moment it's seen finished. `build_signal` plots
one column per sighting (`RunModel::agent_live_sightings`), sent
above and received below, never marked live (an agent's own figures
are the provider's exact count, never a character-count estimate
the way a running prompt's are) -- unlike a prompt, whose own
running estimate only ever occupies the current Now column,
overwritten on every redraw. Peak, Largest and Quiet read the
trace's own columns up to the cursor; Sent and Received instead
always read the same live-inclusive figure the Totals row does,
never cut at the scrub cursor, since the trace's own columns alone
leave out anything that never earns a column of its own (a
model-mode `if`'s own tokens, an unnamed loop's earlier passes, a
declared-outputs `use` child's effects). `sent_live`/`received_live`
mark whether either side still has a live part in it, the same way
the Totals row's own headline does, so a front end can draw the `≈`
marker and the faint style for exactly as long as that holds. A
finished agent session counts toward Largest the same way a finished
prompt already does.

None of this changes a run with no agent tool at all, or an
electricity run (electricity has no agent tool): the Tape, Signal,
Flow and Store frame goldens stay cell-exact, the same as this
section's own note above -- none of the design's own fixture runs
has an agent tool in it.

**Keys.** `0`-`3`, `?`, `q`/`c`/`esc` are unchanged. Theme cycling
moves from `t` to `T` (issue #535's own key); the Tape's own `t`
swaps its token column for the deviation-from-usual view instead,
and `p` toggles prompt previews (`keys::App::tape_tokens_view`/
`tape_previews_on`). `enter` on the Tape opens the selected row's own
step full-screen (`tape_fullscreen_step`), in place of the
Overview's old "open Flow with the running step selected". Every
movement key (`j`/`k`, the arrows, `PageUp`/`PageDown`, `Home`/`End`,
`g`/`G`, `/`, `e`, `f`) moves over `keys::visible_rows_for_tape`'s own
list -- the Tape's own draw order (finish order, then running, then
rows ahead), never the plan's depth-first order `visible_rows`
returns for everything else, which put a loop's later passes and the
plan's own root in places the Tape never draws them. On the Tape
only, a manual move that lands the selection right back on the row
follow would itself have picked turns follow back on, rather than
needing `f`; the design names no key for this on the Tape, so it
reuses the shared `f` toggle every screen already has, and every
other screen's plain "any move turns follow off" is unchanged. `Tab`, `z`
(fold) and `v` (full value) go from the Tape entirely -- they
reached a pane or a fold state the design gives it no key for at
all -- as do `Left`/`Right` (collapse/expand), since its rows have
no collapsed state of their own; `s`/`l` were already scoped to
Flow. All five stay live on Flow/Time/Cost, which keep their own
shipped panes, until their own part of the redesign; each of those
three screens' own Help overlay (`tui::screen_help_text`) lists its
own keys, since the Tape's no longer does.

**Sizes.** 100 columns and up: the list beside its detail pane,
preview rows on by default (off at 24–29 rows, to leave more height
for the Now band and what's ahead of it); §5 also asks for a
shortened title at 100–119, which this build does not yet give it
-- the full title still draws there, ratatui's own border truncating
it if it runs long, a cosmetic gap from the design rather than a
broken layout. 80–99: the list alone, full width, previews always
off. 60–79: the list without its
clock column, with the name column absorbing the freed width and a
fixed 16-cell token field, a shortened `Tape · done/planned · ▰ =
unit t` title, and the Run Started/Run End rows, the status line and
the Totals row all dropped to their own bare figures. Below 60×24:
`tui::draw_tape_too_small` -- the `osi` word, `Terminal too small`,
a Width/Height row each against what the terminal needs with a
check or a cross, the plain-language line naming both and
`osi --log`, and a `q Quit` key bar, matching `design/frames/tape--
toosmall--60x16.mock` cell for cell. The existing screens' own
breakpoints (§6.3's `tui::thresholds`) are unchanged.

**Header band, status line, key bar, Totals row.** This chrome
(`tui::draw_tape_header_band`/`draw_tape_status_line`/`draw_key_bar`/
`draw_totals_row`) now wraps every screen, not only the Tape: `osi
<document> · <status> <elapsed>` on the left of a `statusbar.bg`
band, with the scope icon's own trace taking the `osi` word's cells
while it animates (issue #677's settled §9 point 1) and the screen's
own chip (`TAPE`/`TIME`/`FLOW`/`COST`) and the clock on the right; a
status line (the run's own latest report when there is one, in its
level's colour and collapsing to a bare `▸` when it would not fit,
else the newest line already in the run's own log, until Flow/Time/
Cost get a bespoke one of their own); a key bar in `keyhint.key`/
`keyhint.desc`, each screen's own keys, right-aligned so `? Help  q
Quit` always sits flush against the bar's own right edge and a left
item that would not fit is dropped whole (never cut mid-word) with a
trailing `⋯`; a Totals row (tokens sent/received and the total, its
own deviation from the whole run's usual tokens once history has
one, effects done of planned, the ETA and --- not yet wired to a
real figure on any screen --- the run's own cost). Flow/Time/Cost
keep their own inner panels and their own keys exactly as shipped;
`tui::draw`'s own outer `Layout` just gives them 4 fewer rows of
body than before to draw them in, and their own small-terminal
thresholds (`tui::thresholds`) are 4 rows lower than their own named
size to compensate -- a panel still named "80×20" is still checked
against an 80×20 terminal, not 80×24. Below 60×24, every screen
now reads the Tape's own too-small notice (`tui::draw_too_small`,
renamed from `draw_tape_too_small`), not only the Tape; several of
Flow/Time/Cost's own narrower-terminal goldens (`_at_70x20`,
`_at_80x20`) now pin that notice instead of the sub-24-row layout
they used to, since nothing renders below the shared floor on any
screen any more.

**The Signal** (`screens::SignalScreen`, `build_signal`; `tui::
draw_signal`, issue #688, design v1 part 2) replaces Time as screen
`1`, built from the same inputs `build_tape` already has, plus
`column_ms` (§3.5/§4's own zoom, 50-1,000 ms, 100 the frame's own
default) and `cursor_col` (§4's own scrub, `None` while following
Now). Its own trace is one column per `column_ms` since the run's
own start (`SignalScreen::buckets`): every finished prompt's own
sent/received pair lands in the column holding its own end time,
summed with whatever else finished in the same column; a running
prompt's own live-or-estimate pair (issue #675) lands in the Now
column instead, marked `sent_live`/`received_live` so a front end
draws it in the faint style, settling the instant the node's own
exact pair takes over. The axis top on each side (`screens::
signal_axis_top`) is 1.5x the usual largest step's own sent (or
received) figure from history -- a new `history::StepStats::
tokens_sent_p50`/`tokens_received_p50`, pooled the same "earlier
versions" way `tokens_p50` already is, split into its own two
halves -- rounded up to the nearest of a doubling series (`300, 600,
1200, ...` sent, `40, 80, 160, ...` received); without enough
history, `600`/`80`, doubled exactly once if the run's own largest
single column has already passed it (never the run's own running
total, which almost every run passes long before any one column
does), and never again. `SignalScreen::
step_rows` is one entry per top-level row that has started, in plan
order, at its own start column: a finished prompt's tokens and
deviation and its reply's own first words, a tool as a bar from its
own start to end column (`SignalStepKind::Tool`), a decision with
the steps its own taken branch released and their tokens/deviation,
a loop with its own slots-rule text; the plan still ahead (the
current loop's own next undispatched pass, every top-level row still
pending, `■ End` while the run is live) trails the *last* such row,
faint, rather than getting a row of its own, since it has no real
column yet to sit at. `SignalScreen::now_pane`/`readings` carry the
Now pane's own running/waiting rows, the active loop's own summary,
the last burst and next signal, and Sent/Received/Ratio/Peak/
Largest/Quiet, all read off the same bucket array and `rows`/`flat`
the trace itself is. `missing_loop_pass_rows` (already `build_tape`'s
own fix for an undispatched pass's own row never existing in `rows`
at all) is reused here too, for "waits for a slot" and "Next
Signal" to see a pass neither the loop nor the pass itself has a
`Row` for yet.

**The trace's own visible window.** A run that outgrows the pane's
own width (past about 11 s at the default 100 ms per column, at 120
columns) keeps scrolling so the cursor -- Now, while following it
-- never runs off the right edge: `tui::signal_window_start` keeps a
quarter of the pane's own width ahead of it once the run is longer
than that, buckets, marks, step rows and the seconds row all reading
at `start_col + col` from then on. A step row that started before
the window's own left edge simply isn't drawn (off screen, same as
anything before column `0` always was); a tool's own bar still
draws, clipped to the window's own edge, for as long as it is still
running past it.

**Keys.** `←`/`→` (`keys::App::signal_scrub`) move the scrub cursor
one column at a time, seeded from `signal_now_col` (a cache of the
latest redraw's own Now column, the same way `tree_viewport` already
caches a height for the next key to clamp against) and clamped to
`[0, now_col]`; `n` (`signal_now`) clears it, following again. `+`/
`-` (`signal_zoom_in`/`_out`) step `column_ms` through five levels
(`50, 100, 200, 500, 1000`), clamped at both ends, and reset a
scrubbed cursor back to Now, since a column's own width just changed
under it. `⏎` toggles the same `tape_fullscreen_step` flag the
Tape's own `⏎` does, only while `SignalScreen::selected` (the step
under the cursor) is actually `Some` -- switching screens resets the
toggle too, so the Tape's own open step view can never reappear as
Signal's, or Signal's as the Tape's -- opening it full-screen
(`tui::draw_signal_detail`, a plainer block than the Tape's own:
Status and, for a prompt, its own Reply in full -- no history/share
lines, since Signal's own model carries none of that per row). The
scrubbed cursor itself is drawn (`tui::draw_signal_chart`'s own
`selection.bg` tint down the trace's whole height, distinct from the
Now sweep's own bold accent whenever the two differ) and moves which
column Readings' own Peak, Largest and Quiet read up to -- Sent,
Received and Ratio stay the run's own running totals, and the step
rows and the Now pane keep showing the run's own current state
regardless of the cursor, short of the full historical
reconstruction that would need (re-truncating `flat`/`rows` to the
cursor's own timestamp, the way the goldens' own test harness
truncates a fixture); see "Not yet shipped" below. The Help overlay
(`?`) is Signal's own dedicated one (`signal_help_lines`), the same
fixed `2,5,72,18` overlay the Tape's own `Help · Tape · Key`/`· Keys`
uses, listing exactly `←→`, `+/-`, `n`, `⏎` and the globals every key
bar shares, plus its own Key (issue #738) -- never the shared,
centred overlay Flow and Cost still fall back to.

**Sizes.** 120 columns and up: the trace sits above the Now pane and
Readings, side by side (roughly 52/48); 80-119: Readings drops, the
Now pane keeps only the running rows, and the title shortens to
`Signal · Sent Above · Received Below · Fixed Scale`; below 80, the
Tape's own key bar drops `0-3` to `0` (§4), same as before Signal
existed to switch to at all.

**The Flow** (`rail::build_rail`, `screens::build_flow`; `tui::
draw_flow`, issue #691, design v1 part 3) replaces the tree as
screen `2`: a left-to-right rail of stations and forks
(`rail::RailItem`), built once per redraw straight off `render::
Row`s (always the unfolded ones -- the tree screen's own breadcrumb
and quick-steps folding plays no part here at all) rather than
keeping any tree shape of its own. A `Station` holds §2.3's own
three lines (name, headline, detail); a decision is a two-lane
`Fork` (the taken branch, then the untaken one's own ghost lane --
`· <else-name>`/`not taken`) -- a named `if` reads its own branch
from `meta.branch`, straight off its own row the same way `rows`
already nests it; an *unnamed* one writes no state node, and so no
row, of its own, so its own decision is spliced in here instead
(`build_unnamed_decision`, ported from the removed tree screen's
own `unnamed_branch_state`/`push_unnamed_decision`): inferred from
which branch's own paths actually started (`PlanTree::
unnamed_if_decisions`, concretised against the real pass it's
inside when it is one, since the decision's own host is recorded
with a named loop's `iter_*` placeholder, shared by every pass), a
still-pending one draws both sides as ahead ghosts, a settled one
where neither side ever ran reads `→ none` and ghosts both sides
`not taken`, and two unnamed `if`s sharing one host still get their
own distinct `.__if`/`.__if_1` paths; selecting one resolves
against the rail itself, there being no row for it in `rows`, and
the Selected pane adds a line saying which side's own steps
actually started (or that neither one's did). A parallel `dynamic`
(`flow: tree`) forks into one lane per named child the same way a
loop does; a sequential one (`use`, a chain-flow `dynamic`, a `try`
body with its own `finally`) inlines its own children as plain
stations in plan order instead, each still carrying its own
container's breadcrumb (`gate › reply`) in the Selected pane and the
status line; a container with no child known yet is one station
whose headline is its own type and whose detail is whichever child
it resolves to, once that's known. A loop is one lane per *pass*,
never per concurrency slot -- the frames draw it that way
throughout, and the folded band's own `● n run · m wait` reading
only makes sense that way (a lane that *waits* is a pass that
hasn't started; an idle concurrency slot is not a thing the rail
ever draws) -- the observed real passes first, then an
`ahead_pass_lane` per further pass the loop's own count promises
(`progress.total`, or a named `each` loop's own input read before
any event at all), reading its own position's history for a label,
a usual token figure and a queued tool's own command (`cats`/`usual
155`/`sleep 1`, §2.3's own folded-loop frame) -- never `iter_0`'s,
the ahead lane's own template, whose history is a different slot's;
with not even one pass observed yet, the template is read straight
from the plan's own loop body instead. At most three of an open
loop's own lanes ever draw at once -- the window slides to keep the
selected lane in view the same way the rail itself slides to keep
Now in view, defaulting to the first running lane with nothing of
this fork selected. Selecting a pass (`screens::build_selected_pass`)
or a plain station/decision (`build_focused_step`/`SelectedDecision`)
drives the Selected pane the same way the Tape's own full-screen view
already does, falling back to the default pass only when nothing at
all is selected, or the selection already names one of that loop's
own passes -- never for an unrelated station or decision, which
must show its own pane instead; the Story pane (`flow::build_story`)
adds one synthesized line per loop start (`⟳ drafts · 3 passes · 2
wide`, an unbounded loop's own width reading its pass count instead
of a `max_concurrency` it has none of; `--log` writes no event for a
loop merely starting) and one per unnamed decision's own branch,
since nothing else ever will for either.

**Keys.** `←→` (`keys::App`'s own `flow_scroll`) scroll the rail,
turning follow off -- `tui::draw_flow` itself is the only thing
that knows the rail's own real width, so it (not the key handler)
turns follow back on once a manual scroll lands Now's own arrow
back in view, the same way the Tape's own selection rule already
does. `↑↓`/`j`/`k` move the selected pass over the rail's own `Pass`
rows, turning follow off the same way, resuming it on their own the
moment the selection lands back on the pass the Selected pane would
otherwise default to; `PageUp`/`PageDown`/`Home`/`End`/`g`/`G` are
no-ops on Flow (there is no tree left to page or jump through), and
never touch follow either. `+`/`-` (`flow_open_lanes`/`_fold_lanes`)
open or fold the *selected* fork's own lanes three at a time --
whichever loop a selected pass belongs to, or the selected loop
station itself, falling back to the first loop on the rail with
nothing selected at all -- never every loop at once. `f` follow;
`⏎` the same full-screen toggle the Tape and Signal already share,
armed only while something is actually selected (the same guard
Signal's own `signal_has_step` is). `z`, `v`, `s`, `l` and `Tab` are
gone from the Flow, per §4: quick-steps folding and the full-value
overlay have no Flow-side anatomy to toggle, and there is only ever
the rail's one list.

**Sizes.** 100 columns and 30 rows both: three panes, the rail above
Selected and Story, roughly 31/60 of the lower band's own width
(scaled from §2.3's own 120-column frame's 62/58 split); below 100
columns, or a loop four or more lanes wide, those lanes fold into
their own band (§2.3's own `══ 3 lanes + ══`) regardless of height;
under the three-pane threshold the Story pane drops, Selected taking
the whole lower band; below 80, the Tape's own key bar drops `0-3`
same as every other screen. `↑↓ Lane` itself drops from the key bar
below 100 columns too, alongside `Following`, since there is no
longer room to show either.

**The Store** (`store::build_store`/`layout_store`/`build_store_table`/
`build_store_readings`; `tui.rs`'s `draw_store`, issue #695, design
v1 part 4) replaces Cost as screen `3`: an area diagram of filled
tiles, one per store (a leaf step) that has used tokens so far, the
Largest Stores table with a cursor, and Readings -- drawn self-
contained, the same way the Tape, Signal and Flow already are.

**The model.** `store::build_store_tree` walks the real state tree
(`direct_children_of`, kept here now that `treemap.rs` is gone)
rather than `rows`: a decision, loop, `dynamic` or `use` step is
always a container, holding its own children's tiles inside a
darker surface; anything else with real children of its own (a
`try`/`finally` body, chiefly) is transparent, splicing its own
children straight into its parent's list instead of drawing a tile
of its own, the same treatment the Flow rail already gives it. A
loop pass (`iter_N`) folds into its own single token-bearing step
when it has exactly one -- `drafts › iter_0` reads as `iter_0 ›
draft`, not a container one level deeper for nothing -- and stays a
plain container, named `iter_N`, once it has more than one. Nothing
is drawn for a store, or a whole container, that has not used any
tokens at all; a running container's own title carries `●` and the
accent colour until the run itself ends, even if its own node never
gets a `completed_at` at all (an aborted run). A container whose own
node carries tokens of its own -- a model-mode `if`'s or `while`'s
own condition, evaluated by a model rather than CEL -- draws that as
a further leaf alongside its real children, named `check` the same
way the old Cost treemap's own `container_own_value_node` did. Children sort in plan
order (`store::sort_in_plan_order`, built on `plan::
normalize_pass_segments` and `PlanTree::all_paths`) -- `direct_
children_of`'s own walk reads alphabetically by path, never the plan
order DESIGN.md's own frames draw (`kind, gate, drafts`, not
`drafts, gate, kind`); a path the static plan never named at all (a
dynamically discovered `use` child) keeps its own original relative
order, stably, at the end. A running prompt or agent gets a tile
once it has live figures (`meta.live` read the same two ways the
Tape's own running row already is); the estimate draws in the faint
style, settling once the node's own exact totals replace it. A step
still running with nothing to show yet gets no tile at all.

**Layout** (`store::layout_store`, pure integer geometry, same
spirit as the old `treemap::layout`): siblings alternate columns
(even depth) and rows (odd depth), areas by largest-remainder
apportionment of each node's own tokens (`store::apportion`) -- the
same rule at every depth, which is what gives a loop's two passes
6/6 of 12 rows at 120×30 but 3/4 of 7 at 80×24 (the extra row to the
larger pass) with no special case for either frame. Tiles start one
column inside the main pane's own left border and stop two short of
its right, with a one-cell gutter, the pane's own base colour,
between siblings; a container's own children sit inside a one-cell
padding on both sides, under its own title row and above its own
bottom padding row. A leaf's own text drops lines from the bottom as
its tile runs out of room: `name · N t` on one line where it fits,
else `name` then `N t` on their own two; then `+6% vs usual`, absent
without history; then `↑212 ↓3`. A container whose own children
would fall under six columns or one row collapses to its own title
and `<n>/<m> with tokens` instead of drawing any of them; too-small
*top-level* stores fold together into one trailing `… N more · T t`
tile instead, sized by their combined tokens -- neither case is in
either frame, so both are read off issue #695's own rule 5, not
pinned by a golden. The text promises a tile highlight for the
table's own selected row (§4 above); the frames draw none, so this
ship without one (**not yet shipped**, below).

**The table and Readings.** The Largest Stores table
(`store::build_store_table`) ranks every leaf by tokens descending,
ties in plan order, each row's own full path from the run's root
(`drafts › iter_1 › draft`, a flat table needs it, cut on the
*right* with `⋯` -- `tui.rs`'s own `store_cut_label`, unlike the
Tape's own breadcrumbs, which cut from the left), share of the run's
tokens so far (the Tape's own rounding, `format::format_share_pct`),
and deviation. Weights follow the frames, not §3.6 literally: a
tile's own deviation is bold at every band, including 0-25% (§3.6
itself only bolds the outer two); the table's own stays regular at
every band, including the outer two; the table's own token figure is
bold only on its first (and, in both frames, largest) row. Readings
(`store::build_store_readings`) read the same table and the already-
folded tree: Largest (the table's own first row), Stores (`N with
tokens so far`, counting leaves), Ratio (sent to received, the
Signal's own rounding), and the running loop's -- or the last one
that ran's -- own `name · n of m passes have tokens`, absent with no
loop in the document at all.

**Keys.** `↑↓`/`j`/`k`/`PageUp`/`PageDown`/`Home`/`End`/`g`/`G` move
the table's own cursor -- the shared `keys::App::selected_path`
(issue #704; `keys::App::store_selected` is gone), resolved to the
nearest ancestor in the table with tokens of its own when the
effect itself has none, sticking to it as the table's own rows
reorder; defaults to the first row with nothing in its own lineage
holding tokens yet; `enter` opens the selected store's
own step full-screen, the same shared toggle the Tape's, Signal's and
Flow's own already share, consumed either way so it never falls
through to the generic pane's own quick-steps expand; `esc` closes
it. `i`/`o`/`$` (Cost's own measures), `Tab`, `z`, `v`, `e`, `/` and
`←→` are all excluded from the Store too, same as §4 asks. `i`/`o`/
`$`, `Tab` and `z` are now bound on no screen at all; `v` still works
on the Signal, `e` and `/` on the Tape, Signal and Flow, and `←→` on
the Signal and Flow -- the Store alone has none of these
(**not yet shipped**, below). The Store's own Help overlay
(`tui::store_help_lines`) is the same fixed `2,5,72,18` overlay the
Tape's, Signal's and Flow's own already are, listing exactly these
keys.

**Sizes.** 100 columns and up, 30 rows and up: the chart above the
table (about 62% of the lower band's own width) beside Readings;
below 30 rows, still 100 columns and up: Readings drops and the
table keeps three rows instead of four; 80-99 columns: one pane, no
Readings, the table at the pane's own full width, still three rows;
below 80 columns, the Store bounces to the Tape, the same way the
Signal already does. The title drops `Tokens by Store` below 120
columns (`tui-design.md`'s own 80×24 frame drops it although the
full title would fit there; read here as covering §5's 100-119 rule
too, not pinned by a 100-119 golden of its own).

**Goldens.** Both `store--*` frames (Normal, at 120×30 and 80×24) are
cell-exact goldens the same way the Tape's, Signal's and Flow's own
are, in `tui::tape_frame_goldens::store_normal_120x30_matches_the_
frame`/`_80x24`, replayed to the same frozen instant and history,
the cursor on `prime.gate.reply`. Each pins only the same status-
line row the other three screens' own goldens do, the `gate ›
reply`-vs-`drafts.iter_1.draft` reason §3.2 states -- every other
cell matches exactly. Getting there needed two changes to the shared
comparison itself, not to either frame: a bare `{on:bg-role}` span
with no role of its own (the Store's own container padding and inset
rows, always blank spaces) is now a tag `parse_tag` accepts, read as
`theme.text`; and a space cell -- whichever frame it's in -- now
compares only its own background, never its foreground, weight or
dimness, none of which a space can actually show (the two `store--*`
frames bleed a neighbouring tile's own foreground and weight across
its own gutter and padding cells, issue #695's rule 14, which this
avoids reproducing or allow-listing by coordinate). Both changes were
checked against the Tape's nine, the Signal's two and the Flow's
three goldens before either landed; all fourteen still pass
unchanged. `role_color` also grows `table.header`, the Store
frames' own role for the table's own header row, `theme.text` in
every theme (§6's own table already lists it). The Help overlay's
own `tape--help` frame no longer allow-lists its own `0 1 2 3` row's
`Store` cell (5 columns): the screen is actually named that now.

**From the frames, not from the design text.** The Store's own
column widths and the `⋯`/padding rules around them are read
entirely from the two frames, not derived from any stated formula --
`tui.rs`'s own `store_table_columns`/`store_table_tail_widths`
document each number against the frame it came from. A label that
exactly fills its own field still gets cut with `⋯` (`drafts ›
iter_1 › draft`, 23 characters, in a 23-wide field at 80 columns),
never just left alone the way a label strictly under width is; the
cut text itself leaves one more trailing space than the table's own
header row does (the header's own `TOKENS` lands one column to the
left of where the data rows' own tokens column starts, not aligned
to it). The deviation line inside a tile is bold at every band
(§3.6 itself only bolds two of the four); the table's own stays
regular at every band §3.6 says is bold.

**Not yet shipped.** The tile highlight §4's own prose promises for
the table's selected row (`tui-design.md`'s own frames draw none;
which shade it should take is still open). The too-small-container
and too-small-top-level-tile cases (issue #695's rules 5), read off
the design text alone, since neither frame exercises either. `Tab`
(pane cycling), `z` (fold on/off) and the plain, non-quick-steps row
collapse/expand on `←`/`→` are all still in `keys.rs`, but now excluded
from every one of the four screens -- kept rather than removed, since
nothing says a later screen (Select or All, part 5) won't reuse the
tree pane any of them acted on, but none is reachable from any
screen today.

**Goldens and allow-listed cells.** Every `tape--*` frame in
`design/frames/` is a cell-exact golden (`tui::tape_frame_goldens`:
symbol, fg, bg, bold and dim per cell). The design's fixture run
(`design/fixtures/run2`) is replayed to the frames' frozen instant,
3.62 s after its `run_start`, in the frames' zone, with history built
from `design/fixtures/hist/run-h1..3`, for the Normal, Usual, Help,
80×24 and 60×24 frames; the Starting state, the Ended state (`run2`
at its end) and the Failed state (`design/fixtures/run`) have their
own frames, as does the too-small screen. The three states' detail
panes are built: Engine, Plan, Shape, Usual, ETA and the first step
before the first event; Outcome, Tokens, Usual, Effects, Decision,
Largest, the handled failure and the run's summary at the end; the
error in full and what was not reached after a failure. A loop whose
`each.in` is a document literal counts its passes from the plan
before any of them runs (`plan::resolve_each_len`), so the effects
count reads `/14` from the first frame. A few cells are skipped by
coordinate, each documented at the test's call site: the three history
runs' own median wall time reads differently depending on which clock
measures it -- osi's own history records `last_run.started_at` to
`completed_at` (5.0565 s median, rounding to `5.1s` under
`format_duration`'s round-half-up rule), while the design's own
fixture summary pools the runs' `run_start`/`run_end` *event*
timestamps instead (5.049 s median, rounding to `5.0s`, what the
frames read); one figure in each of the Starting, Ended, Normal and
Usual frames. The status line's own row, in the Normal, Usual,
80×24 and 60×24 frames (and the Help frame's own copy of it, below
the modal): §3.2 reads the newest finished step, loop passes
included, and `drafts.iter_1.draft` finishes 2 ms after `gate.reply`
in `run2/events.jsonl`, so the renderer now reads that pass as the
newest -- the frame still reads `gate › reply finished`, the step
that finished 2 ms earlier. Also skipped: the Help overlay's `0 1 2
3` row's own `Store` cell (5 columns), where the frame names the
Store screen part 4 of the design introduces while this build still
has Cost; `Signal` itself, now shipped, matches the frame exactly
here, so every other cell on that row does too. Also skipped: the
last column of the header and key bar in the Help frame, where the
`.mock` parser pads a trimmed line's own rest out with a bare
`fg.default` span rather than this renderer's dimmed base fill.
Everything else matches exactly.

Both `signal--*` frames (Normal, at 120×30 and 80×24) are cell-exact
goldens the same way, replayed to the same frozen instant with the
same history, in `tui::tape_frame_goldens::signal_normal_120x30_
matches_the_frame`/`_80x24`. Both pin the status line's own row as
allow-listed, the same `gate › reply`-vs-`drafts.iter_1.draft`
reason as the Tape's own goldens above -- every other cell matches
exactly, no further allow-list needed.

All three `flow--*` frames (Normal at 120×30 and 80×24, Folded at
120×30 -- the lanes forced shut with `app.flow_lanes_open`, the key
`+`/`-` itself would otherwise need a live redraw loop to reach) are
cell-exact goldens the same way too, in `tui::tape_frame_goldens::
flow_normal_120x30_matches_the_frame`/`_folded_120x30`/`_normal_
80x24`, replayed to the same frozen instant and history. Each pins
the same status-line row the Tape's and Signal's own goldens do,
the same `gate › reply`-vs-`drafts.iter_1.draft` reason (§3.2's
status line is shared verbatim) -- every other cell, across all
three, matches exactly, no further allow-list needed.

Engine parity for the rail follows the same pattern issue #635's
own acceptance item set for the tree (`oscilloscope-core/tests/
flow_parity.rs`): `flow_parity_if_taken`/`_untaken` replay both
engines' own recorded fixtures for a *named* `if` (`do.yml`'s own
`then`/`else` both naming their one step `chosen`, so the taken
side's own real station must still show, never hidden alongside
the untaken side's identical name) and find the same `gate` fork,
branch and untaken ghost on both; `flow_parity_if_unnamed` does the
same for an *unnamed* one, both engines inferring the same branch
with no `meta.branch` to read at all. `rail`'s own unit tests cover
two unnamed `if`s sharing one host directly: neither anchored yet
(both fall back to `by_host_after`, still getting distinct
`.__if`/`.__if_1` paths) and the first one resolved while the
second stays pending.

**From the frames, not from the design text.** A few behaviours the
design document's own prose never states, read off the frames
themselves instead, so a later frame change could change them: the
80–119 column floor keeps one row of slack at the bottom of its own
scrollable list that 60–79 and 120 + do not, once a run has ended or
failed (both fit `Run Started` through `Run End` in the same height;
only 80–119 draws a trailing blank line before it) -- while a run is
starting or live, this slack is folded into the centred window's own
blank padding instead, the same at every width; rows read in plan
order, not real finish order, once a run has ended (finish order would scramble a loop's
own passes by whichever of two near-simultaneous steps the engine
happened to write first); and prompt previews are off for the
Starting, Ended and Failed states. The Help overlay (§3.7's glyph/
line/colour Key, §4's own keyboard shortcuts) sizes its own popup to
its content, capped at the screen, and splits onto as many pages as
a small terminal needs (issue #738, also settling #684's own
frame-locked blank rows), so the close row -- the only hint a second
page exists -- shows on every page down to 60x24. `?` always opens on the Keys page
(page `0`, `App::help_page`'s own default), the same order the merged
page reads in; `←→` moves a free-running
counter the draw side wraps into whatever range the current page
count actually is, so pressing it keeps working the same way however
many pages that turns out to be. The close row names the other page
when there are exactly two (today's common case: a screen's own Keys
rows and Key rows each fit one page on their own, but not combined),
or reads `page N of M` once either needed more than one page to
itself. When both groups together fit one page (Tape at 120 columns
and up, every other screen from 80 up), they merge onto it and the
title carries no page suffix at all. A row whose own meaning
overflows its column -- not just an overlay narrower than both
columns together -- gets a full-width line of its own, and so does
the other pair of that row, so the right column never shifts and
nothing is cut at the border. For the Signal: a
decision row's own trailing suffix (`·
drafts ×2 · 261 t`) names the very next top-level row's own progress
when that row is an already-active loop -- the design's own prose
never says a decision's row shows anything about a *different*
top-level entry at all, only `run2`'s own frame does; a step row's
own trailing blanks, and every row of the Now pane and Readings,
continue that row's own last span's style to the pane's own right
edge, rather than the plain border-coloured blank every other
unfilled cell in this design gets; a step row cut off at the pane's
own right edge is a plain hard cut, never `⋯` (§3.8's own rule, for
this one case); the sent bar's own blank cells, before a column's
own first real glyph, read `fg.faint` on a labelled row (`600 t`,
`300 t`, `80 t`) and the plain border colour otherwise, then the
row's own accent from the first glyph onward, through any gap to the
next one, not just the glyphs themselves; and the Now pane has no
deliberate blank row between the active loop's own summary and Last
Burst, only the one unused row the panel's own height leaves after
Next Signal. The design's own frames never show two loops running
at once, nested or side by side, so the Now area's own tree for that
case (issue #737) is read off the frames' own single-loop form
extended, not stated outright: a running leaf appears as its own
row; a running loop appears once, as its own row, with its running
children indented two cells per level under it; a container with
nothing of its own (a `dynamic`, `use`, `try`/`finally`, a loop pass) is never
a line of its own, only a breadcrumb segment on whichever line ends
up where it would have sat; a running loop's own line carries its
counts (`busy of slots Slots Busy`, `· N Pass(es) Queued` past zero)
in its own `what` column rather than a separate rule; that text
never loses a digit to the column it sits in -- first spending the
token column's own cells when the loop has none of its own to show
there, then, only once the full text still doesn't fit, a short
form (`N/M busy · K queued`); at a narrow width with no `what`
column of its own at all (§5) the loop's own name field gives up its
own unused room to the same counts instead, in whichever form fits;
and a finished or ahead row's breadcrumb runs from the outermost
enclosing loop's own pass down to the row itself, every parent
segment and `›` in `fg.muted`, the step's own name in the row's
colour -- a tree row's own label drops whatever prefix its own
ancestor row in the tree already said. The name column widens to
fit the longest label on screen, up to a third of the list's own
width, always with at least two cells before the next column, never
glued to it; if two rows would still read the same there, the
column may widen further, up to 40% of the list's own width but no
more. A label still too long loses segments in this order: a plain
container's own name (`build`) first, then a loop's own name, then a
pass (`iter_N`) only as the very last resort -- the first segment
and the step's own name are never the part that gives way. For the Flow: a loop forks into one lane per *pass*,
never per `max_concurrency` slot -- §2.3's own prose reads "one lane
per slot (`max_concurrency` or the pass count when unbounded)", but
the frames draw a lane per pass throughout (a loop 3 passes wide, 2
of them concurrent, still draws 3 lanes, not 2), and the folded
band's own `● n run · m wait` reading only makes sense that way; and
`↑↓ Lane` and `Following` both drop from the key bar and the title
below 100 columns, the design's own prose naming only the Story
pane's own drop there.

**From the frames, not from the design text: the Key's own contents
(issue #738).** §3.7 and §4 name the glyphs and keys; which of them
each screen's own Key page actually lists is read off the frames and
the renderers, not stated in prose anywhere else, and every symbol is
drawn in the real style the screen draws it in, never a generic muted
placeholder. At 120x30 the Tape's own Help has the room to merge every
key and its Key onto one page; every screen's own Help sizes to its
content that way, capped at the screen, and otherwise pages between
its own Keys and Key (and, below either group's own room on its own,
onto still more pages) with `←→`. `?` always reopens on the default
page regardless of where a previous Help left off.

Every screen's Key starts from §3.7's shared glyphs it actually draws
(never `❯`, the Select/All/Store screens' own row cursor -- every
other screen highlights a row's own background instead) and adds: the
Tape's own Now band, a loop line's `slots`/`queued` (§3.4's `2 of 2
Slots Busy · 1 Pass Queued`), breadcrumbs and preview lines (both
`›`), the token bar, the Totals row's own `↑↓` sent/received arrows,
the vs-usual column's `+25%`/`below` deviation colours and the `t`
view's own filled `▓` tick, and a loop's own pass sparkline (`▁`–`█`);
the Signal's own `┃` Now sweep over the axis's `╌` dashes ahead of
it, a step's own done/failed/running/ahead marks and a loop station,
`sent`/`received` above/below the axis, a tool's own bar, the stacked
columns' own partial cells (`▁`–`█`, `▀`/`▔`), the released tokens'
own deviation colours, the fixed scale, `■` Run End and `›` in a
step's own name; the Flow's own station glyphs, ghost lanes' own `·`
and off-screen `◂`/`▸` markers, folded lanes' own `+N`, the token bar
at its real unit, `▾` Now and the rail's own `━` position; the
Store's own tile area, the `check` tile, `… N more`, a running
container's own accent `●`, a live leaf's own dim (italic in
`terminal`) style, the Largest table's own `❯` cursor, the status
line's own `✓`/`✗`/`›` and its own `↑↓` sent/received tokens; and
Select's, All's and the Combined
Signal's own `●` running mark (per-run colour on All and the
Combined Signal, the accent colour alone on Select, which keeps no
run colour of its own), `—`/`gone`, the shared scale and a run's own
name -- plus, on Select, Recent Runs' own `✓`/`✗` and `❯` cursor, and
on the Combined Signal, its own stacked bars, legend swatch and Run
End mark.

**Not yet shipped.** §2.2's own "the step rows and the Now pane
follow the cursor's column" (the scrubbed-cursor half of §6.3e's
own "Keys" above): ←/→ move the cursor, draw it, and drive
Readings' own Peak, Largest and Quiet, but the step rows and the
Now pane keep showing the run's current state regardless, not the
state as of that past column. The Totals row's cost figure reads `—` on every
screen until a run reports one (`meta.cost_usd`,
`runtime.last_run.totals.cost_usd`); the frames' scripted adapter
never does, so every frame reads `—` too. §3.5's own "100–119 columns
shorten long titles and labels" is not yet given its own
title-shortening rule at that width specifically -- the detail pane
does show there, same as 120 +, just with whichever title ratatui's
own border truncation gives a long one. Below three finished runs,
the Run End row reads `ETA — · no history yet` (§3.5); the Key
page's own `usual` line states the run's own actual unit and, once
there have been enough runs to pool, the real count pooled, not
fixed `50 t`/`3` figures.

**Part 5 groundwork (issue #698): the run registry, discovery and
history's `config`/`run_dir`.** Part 5 of the epic (#677) is the
Select and All screens and the Combined Signal, all three now
shipped (below). What shipped first is the groundwork those screens, and
scripts, read from: `oscilloscope_core::registry` writes one `0600`
JSON entry per run `osi` launches or watches, in a `0700` directory
at `$OSI_RUNS_DIR`, else `$XDG_RUNTIME_DIR/circuitry/osi/runs`, else
`$XDG_STATE_HOME/circuitry/osi/runs`, else
`~/.local/state/circuitry/osi/runs` -- one root next to history's own
(§6.3c), named by the run's own `run_id`. The entry is written once
the run's `run_start` is known (`document`, `config`, `engine`,
`pid`, `launcher` -- `"osi <version>"` or `"osi <version> watch"`,
`run_dir`, `events`, `live_state`, `started_at`, document and config
as absolute paths) and removed again at the run's own end, at a
detach, or at osi's own exit -- including every signal path, and the
two places (`run_tui`/`run_tui_watch`'s own tail) that end the
process with `std::process::exit` rather than a plain return, which
skips every other value's own `Drop` still on the stack: those two
call the handle's own removal explicitly, right before. A pid alone
can be fooled by a recycled one, so liveness (behind an injectable
`Liveness` trait) means the pid exists *and* `ps -o args= -p <pid>`
still names `cof` or `electricity`; an entry whose pid fails that
check is pruned (removed from disk, not just skipped) the next time
anything reads the registry.

`oscilloscope_core::discovery::discover_runs` merges three sources,
most trusted first: the registry; one `ps -axo pid=,args=` scan per
call for `cof run`/`electricity` processes carrying `--events`/
`--live-state` (a relative path among them resolves against that
process's own working directory where the platform exposes it --
macOS does not, so there it is shown as given); and a
`state.live.json` under the current directory, or one level of
subdirectories, modified in the last ten seconds. A run named by more
than one source counts once (its pid, then its `--live-state` path).
Both the process scan and the live-state scan are pure, injectable
(`ProcessScanner`/`ProcessCwd`), so no test ever depends on the real
machine's own processes. Bare `osi` with no orchestration argument,
without a TTY (or with `--log`), now runs this discovery and prints
one plain line per result -- document, name, config, engine, pid, run
directory, `"—"` for anything not known -- then exits 0; on a TTY
with no `--log` it opens the Select screen (below).

The history record (§6.3c) gains two optional fields, `config` (the
path as given on the command line, never resolved further) and
`run_dir` (already absolute), format `v` 1 with serde defaults so
every record written before this still reads; only the paths are
ever recorded, never a config's own contents. `run_start`'s own
event also gains an optional `config` field, read already for when
either engine starts writing one (issue #697, not yet shipped by
either); `registry::resolve_attached_config` gives the fallback order
a later screen showing an attached run's config will use: the
registry entry's own `config`, else `run_start.config`, else the
process's own arguments, else nothing.

**The Select screen** (issue #698, part 5 of the epic -- the running
list and the empty state's Recent Runs table). A bare `osi` on a TTY with
no `--log` runs the same discovery as the groundwork above
(`oscilloscope_core::select::build_rows`) and shows one of three
shapes: with exactly one running orchestration, attaches to it
straight away, the same as `osi watch <run_dir>` (read-only, `c`
inert); with none, the empty Select (below); with more than one, the
running list -- `All` first (`N runs · tiled`), then one row per run
with the document and config basenames, the engine, and a `●` (every
row here is live by definition); the cursor starts on the first run,
not `All`. `↑↓`/`j k`/`Home End`/`g G` move the cursor, kept across
every leg of a Select<->attach round trip rather than reset to the
first run each time Select is redrawn; `enter` attaches to a run row,
or opens the All screen (below) on its own row; `q` quits at once (no
confirm: leaving Select only ever stops watching, never cancels
anything), `?` shows Select's own Help (which lists `c` as inert
here, same as the key bar), `0`-`3` and a plain `c` do nothing on any
row, `All` included (Select attaches nothing of its own to switch a
screen or cancel). A raw Ctrl-C
(`Char('c')` with the `CONTROL` modifier, checked before `?`'s own
Help dialog gets a look at it, same as every other screen) quits with
exit 130; a forwarded SIGTERM/SIGHUP quits with 143/129, the same
128+signal convention `osi watch`'s own local-signal case uses
(`forward_signal_exit_code`, shared by both). `esc` on a run attached
from Select returns to Select (§2.5's own text: "`esc` from the run
comes back here") -- a new `App::opened_from_select` flag and
`keys::Action::BackToSelect`, set only for an attach reached this
way; `esc` on bare `osi`'s own direct single-run attach, or on a
plain `osi watch`, still does nothing, as before. The key handling
itself lives in a pure `keys::SelectState::handle_key`, unit-tested
with no real terminal; `main.rs`'s own loop only translates its
`SelectKeyOutcome` into whichever list it indexes.

An attached run's own Help, on every screen, now reads correctly for
a watch (`App::is_watch`, true for every attach reached through
Select, same as a plain `osi watch`): `q` "quit at once", not
"confirms if running" (a watch owns no process of its own to ever
confirm quitting against), and `c` "inert · launched only", not
"cancel · confirms" (only a run `osi` launched itself can ever be
cancelled). `signal_help_lines`/`flow_help_lines`/`store_help_lines`/
`tape_help_lines` each take an `is_watch` flag for exactly this (the
Tape's own Keys page, issue #738, now lists `c` too).

With no running orchestrations, the empty Select shows where
discovery looked, and a Recent Runs table built from
`oscilloscope_core::select::build_recent_rows`: every history record
across every document (`history::read_all_records`, new), newest
finished run first, with DOC, CONFIG (`—` for a record with none),
STARTED, TOOK, TOKENS and OUTCOME -- `ok` alone with no step recorded
`ok: false`, `ok · N handled failure(s)` for one that still ended ok,
`failed · <step>` naming whichever recorded step actually has `ok:
false` for a failed run (never the raw error text, which this module
never reads at all -- the same privacy rule history's own module
doc already states), and `gone` once the record's own `run_dir`
existed but no longer does on disk. A record with no `run_dir`
recorded at all (an older, pre-#700 record) is distinct from that:
its own OUTCOME still reads its real outcome, since history already
knows it, just with `RecentRow::openable` false (`enter` is inert)
rather than the misleading `gone`, which this data note reserves for
a directory that genuinely vanished. `enter` on an `openable` row
opens `osi watch <run_dir>` read-only, same as a running row's own
attach. Both lists' own windows scroll to keep the cursor in view
once there are more rows than the terminal has room for (the same
rule the Store table's own cursor already follows), and the empty
Select's own history read only re-parses when the directory's latest
`.jsonl` mtime has actually moved since last read
(`history::history_dir_signature`), not on every two-second refresh
regardless.

From the frames, not from the design text: the running list's own
two rows for the same document (`triage.yml` twice in the fixture)
are told apart by their config column alone, so basenames suffice
there; a document whose (basename, config-basename) pair collides
with another row's own gets the parent directory added to the
document column only (`select::build_rows`'s own tie-break,
basenames everywhere a row can be told apart some other way, the
parent directory added only where two rows would otherwise read
identically -- not shown in either frame, since the fixture's own
rows never actually collide, but implemented and unit-tested
against a synthetic case). The running list's own box is 56 cells
wide at 80 columns and 60 at 120 and up; the empty Select's own
panel is always the full body width and height, with no centring.
The Select header's own `osi` word never animates (there is no
single run's own process to trace) -- a deviation from every other
screen's header, which this screen's own frames don't contradict
(neither was captured with animation off or on specifically). Select
draws the shared too-small notice below 80 columns, not the other
screens' shared 60 (§5: 80x24 is already its own narrowest frame, so
nothing narrower is ever shown); the notice itself still names
whichever floor actually applies (`draw_too_small_with_min_width`).

An attached run's own config, shown in the chrome, comes from
`registry::resolve_attached_config` at attach time (the registry
entry's own `config`, else `run_start.config`, else the process's
own arguments, else nothing) -- but a run `discover_runs` first found
with none of those known yet (the live-state-in-cwd source, before
its own `run_start` event has arrived) still gets its own `run_start.
config` recorded in history once it finishes: `HistoryHandle::write`
falls back to `model.run_start_config()` when its own `config` is
`None`, read by then even though attach time couldn't have known it.

Select and an attach share one real terminal (the `manage_terminal`
paragraph below): each fresh `Terminal` either side builds
(`run_select_screen`'s own, and `run_tui_watch`'s own whenever
`manage_terminal` is false) is cleared once, right away, since its
own diffing buffers start blank but the real screen underneath can
still carry whatever was drawn there last -- without this, switching
between Select and an attach left the previous screen's own stale
text behind under the new one. The same split also means `run_tui_
watch`'s own tail can't write `finish_watch`'s summary and `exit
{code}` line straight to stdout when `manage_terminal` is false --
that terminal is still in the alternate screen Select's own
`TerminalGuard` owns -- so it buffers that text into a plain `Vec<u8>`
and hands it back through `WatchOutcome::Exited`'s own second field
instead; `run_select` prints it, once its own `TerminalGuard` has
actually dropped, only when that `Exited` is the one that ends the
whole loop (a `BackToSelect` leg just discards it, since Select
redraws the whole screen again anyway).

Goldens: `select--normal--120x30`/`select--normal--80x24`
(`tui::tape_frame_goldens::select_normal_120x30_matches_the_frame`/
`_80x24`), from the design's own six-run fixture
(`design/fixtures/multi/snap6`, already on disk from the groundwork),
and `select--empty--80x24` (`select_empty_80x24_matches_the_frame`),
from `run`, `run2` and `hist/run-h1..3` replayed into
`HistoryRecord`s the same way `demo_history` already does for the
Tape's own "usual" goldens. One cell is allow-listed there:
`build_record`'s own `wall_s` (the state's `started_at`→
`completed_at` span) for `hist/run-h1` is 5.0565s, rounding to
`5.1s`, one tenth past the frame's own `5.0s`; the frame's TOOK
column instead matches the fixture's own events (`run_start`→
`run_end`) for every row, 5.049s for this one. The fixture's state
and events disagree by a few milliseconds, as parts 1-3 already saw
for `gate › reply`, reported rather than bent into the rounding
rule.

A real pty: toggling raw mode and the alternate screen off and then
back on, within one process, left the terminal's own write side
returning an I/O error for the rest of the process -- the first
place in this codebase that ever re-enters `TerminalGuard` more than
once per process (an attach opened from Select, then `esc` back to
Select, then another attach). `run_tui_watch` now takes a
`manage_terminal` flag: `true` keeps its existing, self-contained
lifecycle (`osi watch`, and bare `osi`'s own single-run attach);
`false`, only for a run attached from Select, skips `TerminalGuard`
entirely and draws into a fresh `Terminal` built on the one guard
`run_select` itself holds for the screen's whole lifetime (every
attach and every return to Select, in one process), never toggled a
second time. The one shared event-reader thread (`spawn_event_reader`,
the groundwork's own `EventReader`) is handed back and forth the same
way, never a second one spawned.

Three PTY end-to-end tests, in their own file (`e2e_tui_pty_select.rs`,
not `e2e_tui_pty.rs`): two live runs show Select listing both, `enter`
attaches to the cursor's own run (never either one), `esc` returns to
a freshly drawn Select with no stray Tape text left over, and `q`
quits at once; exactly one live run attaches directly, Select never
drawn at all; no live runs shows the empty Select's own Recent Runs
table, built from a run recorded in this test's own history. Every
`osi` these spawn sets `OSI_PROCESS_SCAN=0` (below), so none of them
turns on `discover_runs`'s own machine-wide `ps` scan at all -- a
run registry scoped to the test's own temporary `HOME` already finds
*exactly* what the test itself started. They still live in a file
with nothing else in it, serialised among themselves with a shared
lock, as a defence against one test's own leftover process (a stray
panic mid-test, say) being visible to a *later* test in the same
file -- `cargo test` runs each test binary in turn, but a single
binary's own `#[test]` functions run concurrently across threads by
default.

**The All screen** (issue #698, part 5 of the epic -- the tiled
wall). `enter` on Select's own `All` row opens it; it tiles every
running orchestration `discover_runs` finds, one full osi per run,
so a single `0`-`3` press switches every tile's own screen at once.
`oscilloscope_core::multi` carries its own pure logic -- the grid
layout (`tile_grid`), a tile's own Tape folding (`fold_tape_tile`),
the shared Signal scale (`shared_signal_axis_top`,
`format_shared_axis_label`), the tile's own two most recent finished-
prompt label groups (`signal_tile_finished_labels`), its own Now
pane (`tile_now_pane_rows`, read back off the Tape's own built
items rather than re-derived), the cross-run status line
(`newest_finished_across_runs`) and one run's own tile build
(`build_tile`, the same `render::build_folded`/`screens::build_*`
calls a single-run attach makes, minus any row selection of its
own) -- so both `main.rs`'s live loop and the frame goldens below
call the identical functions. `main.rs`'s own `AllRunSlot` is the
live side of this: one `LiveStatePoller`/`EventsTailer`/`Differ`/
`RunModel` per discovered run, refreshed on the Select screen's own
two-second cadence, reconciled by the pure, unit-tested
`match_existing_slots` keyed by the run's own `--live-state` path
(the same key `discover_runs` itself already dedups two sightings
of a run by, never the run directory alone) -- a run ending drops
its slot, a newly discovered one gets a fresh one, and an existing
slot's own live state carries over under its run's new identity.
Unlike a Select-opened single-run attach, each slot also compiles
the real plan from the registry's own orchestration/config paths,
since a tile needs it to show its own rows ahead at all. The
header's own elapsed time, the cross-run status line and the
Signal tiles' own shared scale are all computed from every
discovered run, never only the page currently drawn -- paging never
makes any of the three read as if the hidden runs didn't exist.

**The grid** (`multi::tile_grid`, §2.6/§5): as many 56-cell columns
as the width fits, as many rows as `⌈n ÷ columns⌉` needs, then one
fewer column at a time while that still strictly reduces the grid's
own empty cells, considering only column counts that fit every run
on one page at all (falling back to comparing capped-and-therefore-
equally-paging candidates only once none do) -- six runs at 180
columns stay 3×2 (no empty cells either way), five also fit 3×2
(one empty cell, never paged in favour of a 2×2 grid that would
have to page one run away), four rebalance to 2×2, three fit one
row. Each tile row's own real height is a tile panel (at least 20
rows) plus the one-row document band above it; more tiles than a
page's own rows×columns can hold page, the last slot showing `… n
more` instead of a real tile, turned with `PageUp`/`PageDown`. A
grid with room for only one tile-sized cell at all (80×24, the
common default terminal) still shows one real run per page -- never
a reserved `… n more` slot standing in for every run with nothing
real drawn; the remaining count moves into the status line instead
(`… 2 more · PageDown`), and `PageUp`/`PageDown` step one run at a
time.

**Keys** (`keys::AllState`, pure and unit-tested the same way
`SelectState` is): `0`-`3` set every tile's own screen at once;
`←→↑↓` move the focus within the current page's own grid, wrapping
at its own edges (never crossing a page -- that's `PageUp`/
`PageDown`'s own job); `enter` opens the focused tile full-screen on
whichever of the four screens every tile was already showing,
through the same `attach_discovered_run` Select's own run rows use
(now carrying a starting screen), so `esc` from it comes back to
the grid rather than to Select (one more leg of `main.rs`'s own
`run_all_loop`, the same round-trip shape `run_select`'s already
is); `esc` on the grid itself returns to Select; `q` quits at once
everywhere on this screen (watching only, never a confirm); `u`
opens the Combined Signal (below).

**Tile content:** a Tape tile keeps the Tape's
own order (Run Started, the finished rows, the Now band, the
running rows, the slots rule, the rows ahead, Run End, the Totals
row) without the clock column or a detail pane, folding the oldest
finished rows into `… n earlier` and the farthest rows ahead into
`… n more` once the tile's own fixed height runs out
(`multi::fold_tape_tile`); unlike a single run, a tile always draws
one rule between its own running rows and whatever's ahead, labelled
with the slots rule's own text when a loop is running there, a bare
rule otherwise. A pending loop row drops its own `each <name>`
clause once the plan already knows its total (`loop · N passes`
alone, a tile-only simplification of the shared `tape_loop_each_
text` string, since a single-run Tape frame keeps the fuller
`loop · each <name> · N passes` text that `multi::
simplify_tile_pending_loop_text` only ever rewrites for a tile).

A Signal tile is the fifteen-row chart (`draw_signal_chart`'s own
fixed height here, never grown the way a single run's own is) on
the shared scale every tile has been overridden to share. Under the
chart, two rows carry the tile's own most recent finished-prompt
label groups (`✓ name [+N] · T t`, `fg.muted`), each sitting at its
own trace column rather than flush against the tile's own left
edge; finishes that land in the same 100 ms column merge into one
group, named for its own first member in plan order (not
necessarily its earliest finisher, when two land in the same
column), `+N` for every other one in it, tokens summed. A group
scrolled off the window's own left edge isn't drawn at all, never
clamped to column 0; too narrow a budget drops the `· T t` part
before cutting the name itself with `⋯`. Below that, the tile's own Now pane lists the
running leaf rows (never a loop's own container row) then the rows
ahead, each read straight off the Tape's own built rows rather than
this screen's own breadcrumb-based running/waiting rows -- a loop's
own slots rule never names a queued pass here, unlike the Tape
tile's, and an ahead `if` row drops its own condition text the same
way the Tape tile's does. Flow and Store tiles are the ordinary rail
and area diagram, clipped to the tile -- snapshot-tested text-exact
(characters only, no style) against the first tile's own known
content (`all_flow_tiles_draw_real_content_for_every_run`/
`all_store_tiles_draw_real_content_for_every_run`), since neither has
a frame of its own.

**Not yet shipped:** a tile's own "usual"/ETA figures aren't wired
to history yet, unlike a single-run attach's own `apply_history_
eta` -- every tile reads `ETA — · no history yet` regardless of how
many real history runs exist (`run_all_grid_screen`'s own doc
comment). `no_history`/`--no-history` is accepted on this screen but
not read for that reason: there is nothing yet for it to turn off.

**Run colours:** a Signal tile's own sent
trace draws in its run's own role, one of six (`Theme::run_colors`,
read off roles each theme already carries elsewhere --
`accent_primary`, `warning`, `accent_secondary`, `status_info`,
`status_done`, `text` -- so a later categorical palette, §9.5, is a
change to that one list, not to every theme's own fields), by launch
order, repeating past the sixth run. On Terminal and Dracula,
`status_info` is the same colour as `accent_primary` elsewhere in
that same theme; `run_colors` falls back to the next of `error`,
`fg_muted` or `highlight` that isn't already taken (`error`, for
both), so all six always read as distinct colours in every theme
(`run_colors_are_all_six_distinct_in_every_theme`). Nothing else on
a tile -- received bars, the running glyph, the axis sweep -- varies
by run.

**The shared Signal scale:** one round top at or above 1.5× the
largest 100 ms-bucketed sent or received column anywhere across
every tile -- the same figure a single run's own axis already sizes
itself from (`SignalScreen::largest_col_sent`/`largest_col_
received`, finished tokens plus a running prompt's own live
estimate, never a raw sum across unrelated effects that merely
share a column by coincidence -- but it is that sum, since a bucket
is a bucket either way), never a run's own largest *single* effect
(which can leave a bucket with more than one effect in it clipping
its own axis). The smallest of `{1, 2, 2.5, 4, 5, 8} × 10ⁿ` at or
above that, growing only, and never 0 even before any column exists
anywhere (the smallest value in the series at or above the single-
run Signal's own no-history starting axis, 600 sent/80 received --
800 and 80, `shared_signal_axis_starting_top`); labelled with one
decimal place in `k` from 1,000 up (`format_shared_axis_label`), the
plain token figure below it unchanged. Pinned on snap6's own real
columns (`review.yml`'s own largest, 4,440 sent and 206 received):
`shared_signal_axis_top_pinned_on_snap6s_real_columns` gives `8.0k`
sent and `400 t` received. The `all--signal--180x48` frame itself
draws `5.0k` sent, which doesn't follow from any column in this
fixture; its own `400 t` received does. The frame's own tops (`5.0k`
sent, `400 t` received) are injected into the golden directly,
rather than the rule's own output, so every other chart cell can
still be compared against them strictly.

Goldens: `all--tape--180x48` and `all--signal--180x48`
(`tui::tape_frame_goldens::all_tape_180x48_matches_the_frame`/
`all_signal_180x48_matches_the_frame`), replayed from the Select
screen's own six-run fixture (`design/fixtures/multi/snap6`) to its
single frozen instant, the orchestration documents and configs read
from beside it (`design/fixtures/multi/*.yml`). `classify.yml`'s own
loop dispatches two children per pass (a quick prompt, then a
one-second tool) at different speeds, so several passes finish at
once and its own finished rows genuinely interleave by real
timestamp, not the frame's own hand-chosen pass-paired order; that
block (`assert_rows_match_as_a_set`) compares as a set of whole
rows instead of cell by cell at a fixed position, each row still
cell-exact, with the row count checked to match. Everything else carries allow-listed cells, each with its own reason
at the constant that names it: a loop's own token sparkline scales
by the panel's own absolute unit rather than relative to the loop's
own passes, the same existing `tape_token_column` rule the single-
run Tape's own `drafts` loop (`3 passes · 2 wide`, `▃▃ 130 t/p`)
already pins; the Totals row's own `N/M Effects` count, where every
snap6 frame counts exactly one more finished effect than this osi's
own walk of the fixture's real state and events finds (reported
here rather than bent to match the frame's own extra one -- the
part-1 fixtures' own frames count exactly, so this is a property of
snap6's own frames, not of the counter); the cross-run status line
ties the same way a single run's own `gate › reply` timestamp tie
already does in parts 1-3 -- here, the fixture's own events log two
finishes to the same millisecond, but the run's own live state
carries real microsecond precision underneath that shows they are
not actually tied, so the newest-finished pick (earliest plan order,
only ever breaking a genuine tie) reads the later one, not the
frame's own hand-chosen row; a Signal tile's own axis `✓` mark, which
the design text says belongs wherever a prompt finishes, at a column
the hand-drawn frame simply never marked; its own dashed "ahead"
rule stopping one cell short of the tile's own right border, where a
single-run Signal frame always draws it flush to its own edge
instead; and one received-row cell where `review.yml`'s own tiny
(0.03 of a row) remainder past its one full block is left blank,
where the same size remainder is drawn solid elsewhere in the same
frame -- a hand-drawn inconsistency, not a rule.

**From the frames, not from the design text.** A run's own role
(one of six colours, by launch order, repeating past the sixth run)
is read off the two All frames and `all--combined-signal--180x48`'s
own bar colours, never stated in the design's own prose. A tile's
own loop sparkline keeps the single-run Tape's absolute-unit rule
rather than the All frames' own relative-to-the-loop's-own-passes
scale, for the same reason the single-run goldens pin the absolute
rule and nothing in the text says otherwise for a tile.

A PTY end-to-end test (`e2e_tui_pty_all.rs`, the same per-test
isolation `e2e_tui_pty_select.rs` uses, plus `OSI_PROCESS_SCAN=0` so
neither test ever reads the real machine's own process table, only
the registry each spawned `osi` writes under its own temporary
`HOME`): two live runs (both fit on the grid with no paging, so an
arrow key has somewhere real to move focus to), Select lists both
and `All`; `g` then `enter` opens the grid; `1` switches every tile
to Signal; `→` moves the focus to the second tile; `enter` opens
whichever run is focused, on Signal, proving the move really landed
there rather than merely that the key was accepted; `esc` returns to
the grid; a second `esc` returns to Select, both runs still listed;
`q` quits at once.

**The Combined Signal** (issue #698, part 5 of the epic's own last
screen -- `u` on the grid). Every running run on one trace, stacked
by run in launch order, sent tokens above a shared axis and received
below it, one legend row per run, cross-run Totals, a Per Run table
and Readings -- `oscilloscope_core::combined` carries the whole
screen's own pure logic (`CombinedSignalScreen`/`CombinedRun`,
built by `build_combined_signal`), read fresh from every discovered
run's own `Row`/flattened-state pair regardless of whichever of the
four screens the grid's own tiles happen to be showing -- the
Combined Signal always needs every run's own Signal-shaped data at
once, built with `render::build_folded` the same way a single-run
attach is, from `unfolded_rows` rather than the folded `rows` a
tile reads, since a loop's own several passes fold into one
`QuickSteps` row there, dropping the individual rows this screen's
own finished-prompt and running-step scans need. The chart's own
time axis is anchored to the earliest of every run's own
`run_start_ts` (`earliest_run_start`), not each run's own start: 100
ms per column, a tick every second, the Now sweep and the dashed
axis ahead exactly as the single-run Signal draws them, just
stacked. The window holds Now wherever it falls until Now would sit
past three quarters of the way across it (fifteen seconds ahead of
Now at the golden's own 180 columns); from there the window scrolls
to keep Now pinned at that three-quarter mark, so a run of any
length still shows its own latest activity rather than only ever
the first fifteen seconds. Legend rows carry every finished prompt,
so the axis itself never draws a `✓` mark the way a single-run
Signal's own does.

**Stacking** (`combined::stack_column`): a column's own `rows` cells
fill from the axis upward, in the caller's own run order -- a run's
addition that completes a whole row claims it (the run active when
the shared cumulative total first reaches that row's own
threshold, the same rule a single run's own column already uses,
just walked across every run in turn), drawn solid -- unless that
row is the first one the run's own addition completes *and* the
cumulative total just before it wasn't itself an exact multiple of
the row's own unit, meaning an earlier run already left something
in it: that one row draws one eighth short of solid, to mark the
blend, while every row after it from the same addition stays solid;
the one partial cap row, when
the column's own total never completes a whole row by itself, goes
to whichever run contributes the larger *share of that one row*,
not simply the last one added -- a column where two runs' tokens
both land inside the same partial row, neither alone enough to fill
it, still reads as the larger contributor's own colour. The shared
scale (`multi::shared_signal_axis_top`, already built for the All
grid's own Signal tiles) sizes off the largest *stacked* column
across every run, sent and received separately, persisted across
redraws through `keys::AllState`'s own `combined_sent_top`/
`combined_received_top` fields so a Combined leg re-entered after
an opened run's own `esc` keeps growing from where it left off,
rather than collapsing back to the no-history floor.

**The legend** (`combined::finished_legend_text`): one row per run,
a `■` swatch in the run's own role colour, the document, the first
finished prompt or reflector by real finish time (`+N` for every
other one, the run's own total tokens so far appended only while
the whole label still ends strictly before the running label's own
column -- the token figure is always the first thing a cut label
loses, never the name or the `+N`) at its own combined-time column,
and the running label (`● <first running step>`, `+N` for the rest,
`combined::running_step_labels`) fixed at the Now column. The Per
Run table (`combined::join_steps_with_overflow`) joins every
running step the same way, cut to a fixed-width budget narrower
than the column's own full gap to TOKENS, inferred from the frame
(`review.yml`'s own three running passes cut to two plus `+1`;
`classify.yml`'s own two, never cut) since no single width the
frame draws pins the real one exactly.

**Totals and Readings**: Peak and Quiet both read only up to the
scrub cursor (Now's own column when not scrubbed), the same moment
the trace itself stops drawing at -- Peak is the largest stacked
column up to there (sent or received, whichever is larger) and its
own elapsed time from the anchor; Quiet is the time from the newest
column with anything in it, up to there, to Now. Sent/Received and
the grand total always read every run's own full header figures,
never cut at the cursor; Rate is that same grand total over the
elapsed time since the anchor. SHARE and the Largest reading both
read `combined::format_share` (`§3.3`'s own `<1%` rounding, not a
second copy of it). DONE uses the shipped effects counter
(`header.effects_done`/`effects_planned_full`) unchanged, same as
the All grid's own tiles.

**Keys** (`keys::CombinedState`, pure and unit-tested the same way
`AllState` is): `←→` scrub a column cursor, starting from Now the
first time, clamped to `[0, now_col]` -- it never scrubs past Now,
since there's nothing ahead of it to scrub to; `n` returns to Now;
`↑↓` move the Per Run table's own cursor, wrapping at its own
edges, the table scrolling to keep it in view once the runs outgrow
its own rows (the same way the Store's own Largest table already
does); `enter` opens the selected run full-screen on its Signal
screen (through the same `attach_discovered_run` the grid's own
`enter` uses, but always on Signal regardless of whatever the
grid's own tiles are showing), so `esc` from it returns to a fresh
Combined leg (`run_combined_signal_leg`'s own re-entry, the same
shape `run_all_grid_screen`'s already is); `esc` on the Combined
Signal itself returns to the grid, with its own focus and page
untouched, since the grid's own `AllState` never changes while a
Combined leg runs; `q` quits at once everywhere on this screen
(watching only, never a confirm); `?` shows this screen's own
Help, which lists `c` as inert, the same as the grid's.

**Sizes** (issue #713): below 120 columns, or below the height
`oscilloscope_core::combined::combined_body_rows_min` needs for the
current run count (plus the three chrome rows every screen already
carries), the shared too-small notice shows naming 120 columns --
`design/tui-design.md` §5 names no narrower rule of its own for
this screen. Above that floor, the chart panel (the trace, the
legend and Totals) and the bottom block (Per Run and Readings)
split the body's own rows by `combined::combined_vertical_split`,
restoring each piece in the reverse of the order it gives them up
in as the height shrinks: the bottom block's own blank rows
first, up to its own 180x48 size (13 of 45 body rows, a header row
plus up to 10 blank/scrollable table rows); then the legend's own
blank rows, up to its own 180x48 size (8, enough for up to 8 runs
without the chart growing for them); then the trace itself, up to
its own 180x48 size (19 rows: 14 above the axis, the axis, 4
below). Shrinking past all of that, the trace still has a floor
of its own (9 rows: 5 above the axis, the axis, 3 below) below
which every run's own legend row and the bottom block's own floor
(a header row plus up to 3 scrollable rows, or Readings' own 4
lines, whichever needs more) are still the last things given up --
only the separator row above Totals goes first, the one row this
floor never needs. Past the golden's own 180x48 size, every extra
row goes to the trace alone; the legend only grows past its own 8
rows for a run count that itself needs more than 8, and the bottom
block never grows past its own 180x48 size at all. TOKENS/SHARE/DONE
stay right-aligned to the Per Run table's own right edge at any
width, so they move further right as the table widens; NOW takes
whatever gap that leaves before them, capped at the golden's own 40
columns. RUN/CONFIG/ENGINE/NOW's own start columns stay pinned at
the same offsets regardless. A Readings line too long for a
narrower-than-golden panel wraps rather than truncates, since the
panel almost always has rows to spare for it well before its own
floor.

Goldens: `all--combined-signal--180x48`
(`tui::tape_frame_goldens::all_combined_signal_180x48_matches_the_
frame`), replayed from the same six-run fixture frozen at the same
instant as both All frames. Every allow-listed cell: four
received-side cells whose own single partial row never reaches a
whole stacked row in this fixture, rounded here by the combined
remainder's own share of the row's unit, which the frame rounds
each one differently and not consistently in one direction
(`COMBINED_SIGNAL_RECEIVED_PARTIAL_ROUNDING`, reported rather than
bent to whichever rounding each one would need); the same `N/M
Effects` counter discrepancy the All frames already carry, used
unchanged (`COMBINED_SIGNAL_EFFECTS_DONE_COUNTER`); the cross-run
status line's own `classify.yml` tie, broken by real microsecond
precision underneath the frame's own hand-chosen row, the same
reasoning the All frames' own status line allow-list already gives
(`COMBINED_SIGNAL_STATUS_LINE_PRECISION`); and the frame's own
`Largestsummarize.yml` reading, which lost the one space every
other label here keeps, kept here and allow-listed rather than
reproduced (`COMBINED_SIGNAL_READINGS_LABEL_GUTTER`). A shared full
row -- one run's own leftover completed by the next run's own
start, on the sent side -- draws one eighth short of solid to mark
that blend; every other full row draws solid, reproducing the
frame exactly rather than needing its own allow-listed group.

**From the frames, not from the design text.** The legend's own
finished-prompt label sits at its own real finish column, cut to
drop the token figure the instant it would reach the running
label's own column, never short of it. The Per Run table's own NOW
column cuts to two running steps plus `+N` at a width narrower than
the column's own full gap to TOKENS, inferred from the frame as
above. The lower half's own 136/44 column split is that exact
ratio, not a rounder percentage that only coincidentally matches at
180 columns. The axis itself carries no `✓` marks -- every finished
prompt is in the legend instead. The time axis extends fifteen
seconds ahead of Now at 180 columns, from the window rule above.
Stacking follows launch order, the same order the legend and the
Per Run table already share. Run colours and the shared scale both
already carry their own from-the-frames notes above (the All
screen's own paragraph).

A PTY end-to-end test, in the same file as the grid's own
(`e2e_tui_pty_all.rs`): three live runs; Select lists all three and
`All`; `g` then `enter` opens the grid; paging to the grid's own
second page; `u` opens the Combined Signal; `↓` selects the Per Run
table's own second row; `enter` opens that run full-screen on
Signal, its own document checked against that row, not the first's;
`esc` returns to the Combined Signal; `esc` returns to the grid,
still on its own second page; `esc` returns to Select; `q` quits at
once.

**Browsing every effect after a run, with one selection across
screens** (issue #704, beyond anything `design/tui-design.md` itself
describes). The Tape's own `TapeHead::Ended` state (`App::
tape_run_end_mode`, set once the instant the run ends, keyed off the
Tape's own default selection -- the run's handled failure when it has
one, else its last row) shows the run summary for as long as that
mode holds; moving the selection (`App::move_tree_selection`/`jump`)
clears it for good, and the detail pane then shows that row's own
detail instead, the same `tape_normal_detail_lines` the `TapeHead::
Normal` state already does -- `enter` opens whichever of the two is
currently showing. `App::selected_path` is now the one selection
every screen reads: the Store's own table cursor (`store::
build_store`'s own `cursor_path`, `store_cursor_path`) resolves to it
directly, or the nearest ancestor in the Largest Stores table when
the effect itself has no tokens of its own, and `App::store_selected`
is gone; the Signal's own scrub cursor (`screens::build_signal`'s
`selection_col`) sits at the selected effect's own column -- its end
time once finished, its start while it is still running -- whenever
the cursor is not actively being scrubbed with ←→, and a scrub still
moves `selected_path` to follow whichever step the cursor lands on
(`tui::draw_signal`).

The Flow rail's own stations are reachable from the keyboard
(closing #693's keyboard-selection item): with follow off or the run
ended, ←→ (`App::flow_select_station`, over `App::
flow_lane_stations`, `rail::lane_stations`) move between the
selected lane's own stations in plan order -- a plain station, a
named or inferred decision and a fork's own host (a loop, a parallel
`dynamic`) alike. A decision fork never really forks this sequence
(only one of its own two lanes is ever real), so it flattens its own
taken lane's stations right in; a loop's or a `dynamic`'s own lanes
stay their own separate zones, reached only by ↑↓ first, the same as
before. Following a live run, ←→ still scrolls the rail exactly as
it already did.

The full-screen step (`enter`, shared by every screen) now has every
section the run recorded, each only when the node has data for it:
Status, Asked (a prompt's own `prompt_sent` in full, or a tool's own
command line), Produced (`FocusedStep::value_full`, the node's own
value in full, never cut to a preview the way `FocusedStep::value`
already is), Error, Tokens (`FocusedStep::usual_tokens`/`share_pct`,
the Tape's own `selected_usual`/`selected_share_pct` figures, now
built for every screen's own selection inside `build_focused_step`
itself), Attempts (`FocusDetail`'s own fallback-attempt chain, shown
only past a single trivial entry, alongside `FocusedStep::
retries_used` when an earlier, now-overwritten pass actually
retried), Tool (`FocusDetail::Tool`'s own `exit_code`/`stdout`/
`stderr`), Decision (`FocusDetail::Decision`, the same data the
Flow's own `SelectedDecision` already carries, now reachable for an
`if` row selected from any screen), Loop (`FocusDetail::Loop`, one
line per pass read straight from state rather than from whichever
row list the caller has -- a uniform loop's own passes are routinely
folded into one quick-steps summary row on the Tape, with no `Row`
of their own left for a list-based reading), the existing Agent
block, and Meta (`FocusedStep::meta_extra`, every other key the node
recorded beyond every section above, folded by default, `m`
unfolds). The Tape's own `tape_normal_detail_lines` and the Flow's
own `flow_selected_step_lines` each append these sections after
their own existing content, keeping every frame golden cell-exact;
the Signal's and the Store's own full-screen step
(`draw_focused_step_detail`, never part of a normal-layout golden)
builds all of it together (`tui::focused_step_full_lines`). `[`/`]`
(`App::fullscreen_step`, over `App::flow_rows` -- every effect in
plan order, refreshed every tick regardless of which screen is on
display) step to the previous/next effect that has started;
↑↓/PgUp/PgDn/Home/End scroll the step's own content instead of
moving the selection underneath it, and both take over before any
screen's own bindings while the full-screen step is open. Every
screen's key bar reads this shared full-screen key bar instead of
its own ordinary one for as long as it stays open.

**The detail pane's own description, fields and sections** (issue
#716, shared by the Tape's own pane, the Signal's and the Store's
full-screen step). `PlanTree` grows `PlanEntry::description` (read
from a tool/prompt/`use`/yield leaf's own compiled `description:`;
`None` for a report, which has no plan row, or a container, whose
schema has no `description:` of its own); `FocusedStep` and
`TapeSelectedDetail` both carry it through, and `tape_normal_detail_
lines`/`focused_step_full_lines` print it in full, ahead of
everything else. A new shared renderer (`detail_field_lines`/
`wrap_detail_value`/`detail_section_rule`/`detail_key_style`) lays
out every key/value field (Status, Type, Tokens, Usual, Share, the
Agent block's own fields, and now a tool step's own Command and
Exit) in one right-aligned column, a value wrapping under its own
column with a hanging indent rather than running off the pane; every
section (Prompt Sent, Reply, Produced, Error, Transcript, Attempts,
Tool's renamed Output/Errors, Decision, Loop, Meta) now starts with
a titled rule (`── Reply · 41 t ───`) instead of a bare title line.
`detail_key_style` carries the dim modifier in every theme, so a
key reads differently from a value even in `terminal`, where
`fg_muted` alone is the same colour as the default foreground.

A tool step's own Command field reads the rendered command and args
(`meta.params_rendered`, the same source `FocusDetail::Tool::
command` already read) or, before the node has one yet, the plan's
own static command (`tool_rendered_command`, falling back to
`PlanEntry::tool_command`); a running tool's Status line also reads
its elapsed time against its usual busy time from history
(`FocusedStep::usual_busy_p50_s`, populated by `build_focused_step`
itself now, from the same `header.history_step_usual` the Tokens
line's own "usual" already reads). Once finished, an Exit field and
`── Output ───`/`── Errors ───` sections follow, each the run's own
last 20 lines of stdout/stderr -- neither shows before the tool
finishes, since neither engine publishes one before then -- except
for a running subprocess tool's own `meta.live` (issue #741,
`model::ToolLive`), told apart from the agent tool's own `meta.live`
shape by its own `stdout_tail` key (the same way `PromptLive`'s own
`adapter` key tells it apart from the agent's, and now excluded from
`AgentLive::from_value`'s own permissive parse too, so a subprocess
tool's running step never reads as a near-empty agent session to
`StepTokensSoFar` and friends). `NodeMeta` grows a third parse of
`meta.live`, `tool_live`, alongside `live`/`prompt_live`;
`FocusDetail::Tool` grows its own `live: Option<Box<ToolLive>>`,
read the same way `stdout`/`stderr` already are. `tool_output_
sections` shows `── Output (live) ───`/`── Errors (live) ───`
sections from `stdout_tail`/`stderr_tail` in place of the finished
ones while the node has no `stdout`/`stderr` of its own yet, cut to
the tail's own last-20-lines bound exactly like a finished section
is cut to 20 under `full: false` (nothing to cut further: the tail
is never longer than that to begin with); a muted `updated {n} ago`
line appears directly under the section's own rule, the same
`format_duration` phrasing a row's own age already uses, once
`ToolLive::is_recent` (`AgentLive::is_recent`'s same 5-second rule)
says the tail itself has gone stale. The
Flow's own simpler Selected pane, out of this issue's scope, keeps
its exit code in its own existing `produced:` line instead.

`Tab` moves focus between the Tape's own list and its detail pane
(`App::pane`, the same `Pane::Tree`/`Pane::Details` the Overview-era
code already had, now actually wired to a key): the pane gets the
accent border, and `↑↓`/`PgUp`/`PgDn`/`Home`/`End`/`g`/`G` scroll it
(`move_or_scroll`'s existing `Pane::Details` arm) while the list
selection stays put; `esc` or `Tab` again gives focus back without
leaving the screen. Switching screens drops the focus back to the
list. The pane's own top and bottom borders read `↑ n above`/
`↓ n more` once its content overflows it (`Block::title_top`/
`title_bottom`, new to this crate); choosing another row resets the
scroll to the top (the existing `details_scroll` reset already did
this); a live step's growing content follows the bottom only once
the pane was already scrolled there (`App::tape_detail_bottom`,
compared against the current scroll at the start of every draw,
before its own clamp runs).

**On a run longer than the window (issue #734).** The text is
silent: every frame is a short run, so none of this changes a single
one. While the run is live and nothing is being scrubbed
(`App::signal_cursor_col`, never `screens::SignalScreen::cursor_col`,
which the previous paragraph's own `selection_col` can leave parked
on a step that finished long ago), the single-run Signal's own
window follows Now at the same share of the trace's own width the
Combined Signal and the Signal tiles already use (`tui::
trace_window_start`, shared by all three rather than copied). An
ended run's window ends at Run End instead, at the pane's own right
edge, rather than leaving a quarter of it empty past a run that will
never fill it. A scrub still moves the window exactly as
`trace_window_start` already did, seeded from wherever the trace
last actually drew its own cursor only when that column is still
inside the window the latest redraw drew (`App::signal_window_start`)
-- never teleporting back to a stale, long-scrolled-past selection;
`n` returns it to following. A tool bar already running when the
window scrolls past its own start still draws its own label, readable
from the window's own left edge, its own fill sized from that same
edge rather than from the row's original, now off-screen start, and
clipped by the same per-span budget any other row's own over-long
text already is (`draw_signal_step_rows`). Second marks on the
single-run Signal, its tiles and the Combined Signal read in one
elapsed-time format shared by all three (`format::
format_signal_elapsed`): whole seconds under a minute (a decimal only
where one already showed, `0.5s`), `MmSSs` from a minute, `HhMMmSSs`
from an hour -- never a raw second count. The Combined Totals' own
`Peak … at` reads the same scale but keeps its own decimal under a
minute even on a whole second (`format::
format_signal_elapsed_with_decimal`, `at 3.0s`), the reading it has
always shown; `Quiet` keeps its own separate one-decimal reading,
unaffected. Their marks' own spacing (`format::signal_mark_step_s`)
widens past one mark a second only once a run's own longest label
would otherwise overlap its neighbour. A tick never overlaps a Now
or Ahead label on either side, with at least one blank column always
clear (`tui::mark_clears_label`, shared by the single-run Signal, its
tiles and the Combined Signal). The Now label itself, and the Ahead
label after it, are cut to the room left before the pane's own right
edge rather than overflowing it (`tui::cut_now_and_ahead_label`,
shared the same way): Ahead is dropped first, then the elapsed part
of the Now label, and the bare clock itself is never broken -- too
narrow even for that drops the whole label rather than ever cutting
it. The Combined Signal's own Per Run table cuts the RUN, CONFIG and
ENGINE columns with an ellipsis before the next one starts, rather
than letting a long label run over it, the same rule the Largest
Stores table's own `store_cut_label` already follows. Readings, on
both the single-run Signal and the Combined Signal, never wraps any
of its own lines: a long Largest label is cut with an ellipsis
instead, and the token figure after it, and the share after that on
the Combined Signal, always stay whole; every other line (`Runs`,
`Steps`, `Scale`, `Sent`, `Received`, `Ratio`, `Quiet`) is cut the
same way once the pane itself is too narrow for it, rather than
reading bare or losing a trailing word with no mark at all
(`tui::readings_cut_line`).

**A JSON reply or value reads as a short summary, never raw JSON**
(issue #739). One function, `oscilloscope_core::summary::
json_value_summary`, replaces every place a reply's or a value's own
`render::display_value` plus `first_words` once fed straight through
to a row: the Tape's own `tape_prompt_what`, the Signal's own step
rows, `render::details_for`'s own `value`/`received` blocks, and the
Flow rail's own selected pass pane (`screens::build_selected_pass`'s
`produced` field) -- the detail pane's full, untruncated `value_full`/
`reply` sections, issue #716's own concern, are untouched. A JSON
object's first present field among `summary`, `message`, `title`,
`reason`, `status`, `result` (in that order) leads with its own first
words; otherwise its first keys, alphabetical since `serde_json::
Map` is a `BTreeMap` without the `preserve_order` feature (never
added to any crate here -- issues #492/#549's own depth-safety
warning), each read by its own kind (`N items`/`N keys`, singular at
exactly one, a string's own first words, a number/boolean/null as it
is), joined with ` · ` and cut to the column the same way a plain
reply already is. A JSON array reads `N items`, its first item's own
first words following when that item is a string. A string is tried
as JSON first -- trimmed, and only if it starts with `{` or `[` and
actually parses as an object or array -- so a reply or value that
*holds* JSON as text (a tool's own captured stdout, an untyped
prompt reply) is summarised exactly as if it had arrived typed,
never shown raw either; `summary::is_json_shaped` is the same check a
caller uses to decide whether to route a value through the summary
at all. Anything else -- plain text, a number, a boolean, or a
string that only starts like JSON without actually parsing -- keeps
the existing first-words rule unchanged, and the Flow's own simpler
Selected pane (`render::details_for`'s `value`/`received` blocks,
the only callers of `FocusedStep::value`/`Details::received`'s own
snippet) keeps today's full, untruncated preview for exactly that
case rather than cutting a long plain reply at its own first
sentence: only a JSON-shaped value (object, array, or a string that
parses as one) goes through the summary there, everything else still
reads through `display_value`. An agent step's own rule
(`screens::tape_finished_agent_what`) leads with its duration, the
same as every other finished tool row's own "what" text, then issue
#703's own files-changed/turns/tool-calls fields from `meta.raw`
(files, with `+N −M` alongside it, then turns, then tool calls, with
only the parts actually present) -- it needs the whole node, not just
its value -- but once none of those three is recorded there at all,
it now falls through
to the same shared summary on the node's own value rather than
reading only the bare duration.

**Two layout slips since #744's own breadcrumbs** (issue #745), both
at 120x30 over a deeply nested loop. The detail pane's own title
(`tui::tape_detail_title_cut`) now cuts a long breadcrumb from the
middle with `⋯` once it would run into the border, keeping the
breadcrumb's first segment and as many of its own trailing segments
-- including the step's own name -- as still fit alongside the fixed
` · Selected` suffix, rather than ratatui's own hard clip with no `⋯`
at all; when even the first segment doesn't fit alongside the step's
own name, the first segment drops too (`⋯ › {name}`), so the step's
own name is never the part a hard clip eats into. `■ Run Started` and
`■ Run End` (`tui::draw_tape_list`, and the All screen's own tile
equivalent, `tui::draw_all_tape_tile`) now pad their own name field to
the same `name_width` every other row on screen already widens to,
rather than the old fixed 19-cell floor.

### 6.4 Tests

- **Golden tests, with `insta`.** Each fixture is a recorded run: plan, snapshot sequence with times, events, stdout/stderr and exit code. The fixtures are the §1 probes, with paths scrubbed. Two outputs are checked against goldens:
  - the status table after each observation;
  - the `--log` output.

  Cases cover every row of §2, including events-only, state-only and abort (no final snapshot).
- **TUI render tests (shipped, issue #434; the new layout and panes, issue #527; the Flow screen, issue #635).** `ratatui::backend::TestBackend` buffers (`oscilloscope/src/tui.rs`'s own `golden_tests` module, `insta`), one per state the acceptance list named: pending plan, running chain, a running tree loop with `max_concurrency`, a loop with two of its passes already done, queued-under-concurrency, failed, failed and handled, cancelled, aborted (no final state) and `osi watch` — at 100×24 (the two-pane layout: flow pane | selected pane, no story pane), since that is what most of these actually want to show. `pending_plan` and `aborted_no_final_state` stay at 70×20 deliberately, as this suite's own one-column-fallback coverage. `fallback_at_80x24_drops_the_state_pane` and `two_column_layout_at_100x30_shows_the_state_pane` cover the Flow screen's own three-size boundary: 80×24 is still two-pane (the story pane stays dropped), and 100×30 is the first size with all three panes, flow pane | (selected pane over story pane). `full_layout_with_state_pane_following`, at 120×40, is the most comfortable size, with every pane populated. `follow_picks_the_running_row_not_a_hard_coded_one` builds its own `App` and drives the selection through `App::follow_target`, the same way `main.rs`'s own redraw loop does, rather than hard-coding `selected_path` to a row that happens to be running — every other golden here does the latter, which (correctly) also turns follow off, since setting `selected_path` directly bypasses the key handling a real manual selection goes through. `a_long_plan_scrolls_to_keep_a_followed_effect_below_the_fold_visible` is a synthetic 48-leaf chain with the 45th leaf running and followed, at 70×20: the first leaf scrolls out of view, the followed one stays visible, and the border shows a `45/49`-style position. `folding_breadcrumbs_and_quick_steps` additionally covers a breadcrumb fold whose statuses agree staying folded (and one whose statuses differ staying apart), and three quick steps folding into one row with the right total. `flow_matrix_running`/`finished_at_120x40`/`100x30`/`80x24`/`70x20` cover the acceptance list's own matrix: a named decision taken with its untaken branch's dim summary, an unnamed decision inferred, a single-pass loop merged into one row, a quick-steps row, and a non-empty story, running and finished, at every required size. (The Flow-specific parts of this paragraph -- its own layout, panes and the `flow_matrix_*`/`fallback_at_80x24_drops_the_state_pane`/`two_column_layout_at_100x30_shows_the_state_pane`/`folding_breadcrumbs_and_quick_steps` tests -- describe the tree screen issue #691 replaces, not the rail; §6.3e above and its own goldens are current. The non-Flow golden names this paragraph lists -- `pending_plan`, `running_chain`, the tree-loop and queued-under-concurrency cases, `failed`/`failed_and_handled`/cancelled/aborted, `osi_watch` -- are unchanged and still exercise the rail today, just not at the two/three-pane sizes this paragraph names for them.)
- **Key handling (shipped, issue #434; the new navigation, issue #527).** Unit tests in `oscilloscope/src/keys.rs`: navigation, `PgUp`/`PgDn`/`Home`/`End`/`g`/`G` (against a ten-row synthetic plan, with an explicit `tree_viewport` standing in for `tui.rs`'s own last-rendered height), collapse/expand (with a trailing `.` so a same-prefix sibling is never hidden too), follow (on by default, and turned off by every manual move), errors-only, filter, `v`'s own prompt-first-then-cycle behaviour, both confirm dialogs (`c`, and `q` while a run is going), Ctrl-C's own action (checked before, and never merely dismissing, a dialog), the three-way `tab` cycle (including skipping straight past the state pane when it isn't drawn at all, `state_viewport == 0`), the state pane's own selection staying independent of the orchestration pane's, and the details pane's own scroll resetting whenever the orchestration selection changes — pure, with no real terminal. `tui.rs`'s own `scroll_tests` module unit-tests `clamp_scroll`/`scroll_indicator` and the state pane's line-aware `clamp_state_scroll` directly, beyond what the rendered goldens already exercise indirectly.
- **The render model (`oscilloscope-core/src/render.rs`).** Unit tests for the loop-pass multiplier/lower-bound on "effects planned", and the stderr tail's own reading order; `oscilloscope-core/src/model.rs` and `plan.rs` cover an unnamed `if`/`else`'s own two branches (neither pollutes the other's earlier-siblings, and an untaken branch resolves once the taken one is observed, rather than blocking every chain sibling after it forever). The state pane's own order, abbreviation (including a multi-line error showing only its first line), container token sums over a loop, a tree dynamic and a `use` child, are covered on synthetic snapshots; the token rule is pinned against each engine's own real recorded state for a retried `prompt_type: json` prompt whose first reply fails to decode (`model::tests::subtree_tokens_matches_run_totals_for_a_retried_prompt_on_cof`/`_on_electricity`); the token bar's own running-prompt filter (`waiting_for` excludes a queued prompt) and estimate are unit-tested; and the 2,000-node redraw measurement above lives here too. A multi-step tree- and chain-flow loop pass's own status, derived from its leaf descendants rather than defaulting to pending, is covered for the running/failed/done cases, alongside a regression test that a single-step pass still derives the same way (`a_single_step_pass_still_derives_done_from_its_one_leaf`). The token bar's own count is tested against a row confirmed running by events alone, with no state node at all (a tree-loop pass's prompt; a lower bound, since there is no `prompt_sent` to estimate from) and against one that cannot be classified as a prompt at all from path and events alone (a `use: inline:` child's, pinning that known gap rather than silently assuming it). "Effects planned"/"done" are tested against a real compiled `use: inline:` and `use: path:` document each, at every discovery stage (nothing yet, one child, every child, and a declared-outputs style `use` whose children never appear even once it finishes), always with done never exceeding planned.
- **The Store (`oscilloscope-core/src/store.rs`, issue #695).** Unit tests pin the layout's own rectangles to both frames' geometry (`layout_matches_the_120x30_frame_geometry`/`_80x24`), the largest-remainder apportionment itself (`apportion_uses_largest_remainder_with_earliest_index_tie_break`, including the zero-weight/zero-cells cases), a container too small for its own children collapsing to its own title and `<n>/<m> with tokens`, too-small top-level stores folding into one `… N more` tile, the store tree's own container/fold rules (a loop pass with exactly one token-bearing step folding into a leaf, more than one staying a container, zero-token stores and containers excluded, a running prompt's own live estimate getting a faint tile, a running step with nothing yet getting none, a running container's own marker leaving the moment the run ends even with no `completed_at`), the table's own order/ties/share rounding/breadcrumb, Readings with and without a loop, and `build_store`'s own performance over 2,000 flat leaves (the same bound `render::build`'s own test polices). `oscilloscope-core/tests/store_fixtures.rs` (renamed from `cost_fixtures.rs`) keeps the acceptance item that the store tree's own token sum matches the run's totals on every recorded fixture with a `prompt` effect, on both engines, including a retried prompt. `oscilloscope/src/keys.rs` covers the Store's own keys: `↑↓` and its aliases moving the table's own cursor and clamping at either end, `enter` opening the selected store's own step full-screen only with a row selected, `esc` closing it, switching screens resetting the shared toggle, and `i`/`o`/`$`/`Tab`/`z`/`v`/`e`/`/`/`←→` all doing nothing on the Store. `tui.rs`'s own golden tests cover the Store screen at 120×30 and 80×24 against the design's own frames (`tui::tape_frame_goldens::store_normal_120x30_matches_the_frame`/`_80x24`); further unit-level `tui.rs` tests cover the Help overlay listing exactly the Store's own keys, the full title dropping `Tokens by Store` below 120 columns, Readings dropping below 30 rows or 80 columns, and a leaf's own name and tokens wrapping onto two lines once they don't fit one.
- **Terminal restore (shipped, issue #434).** Unit tests in `oscilloscope/src/terminal.rs` prove `TerminalGuard`'s own entered/restored bookkeeping, and that the installed panic hook itself (not a direct `restore()` call) clears it through a real `catch_unwind`; `oscilloscope/tests/e2e_tui_pty.rs`'s `a_forwarded_sigint_still_restores_the_terminal` proves the real escape sequences on a real pseudo-terminal, after a forwarded SIGINT.
- **End-to-end with `cof`** (`tests/e2e_cof.rs`, plain mode; `tests/e2e_tui_pty.rs`, the TUI on a real pseudo-terminal via `posix_openpt`/`grantpt`/`unlockpt`/`ptsname` — no pty crate dependency; `tests/e2e_tui_pty_select.rs`, the Select screen's own PTY tests, kept in their own file and serialised among themselves as a defence against one test's own leftover process being visible to a later one in the same file), gated on `OSI_E2E_COF=1` (`cof` on `PATH` required once set):
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
- **The Signal screen (issue #688, replacing the Time screen of issue #650).** `oscilloscope-core/src/screens.rs` unit-tests `build_signal` end to end against synthetic rows and nodes: the axis rule with history (1.5x the usual largest step, the smallest doubling-series value at or above it) and without (600/80, doubled once past the run's own largest single column, never past the run's own running total, and never a second time); two prompts ending in the same 100 ms column summing into it rather than each getting their own; a decision's own `◆` mark winning over a finished prompt's `✓` sharing its column; a running prompt's own live estimate landing in the Now column, never the column its own start time would otherwise place it at; and Readings' own ratio and peak. `oscilloscope/src/keys.rs` unit-tests `←→` scrubbing one column at a time and clamping to Now and zero, `+`/`-` stepping the five zoom levels and clamping at both ends (and resetting a scrubbed cursor back to Now, since a column's own width just changed under it), `n` returning to Now, `⏎` toggling the full-screen step view only while a step is actually under the cursor, and switching screens resetting that shared toggle. `oscilloscope/src/tui.rs` unit-tests the chart directly: a live column's own bar draws faint, never in the Now sweep's bold style; a run many times longer than the pane is wide still keeps the sweep on screen (the trace's own window scrolls to follow the cursor, or Now while following it); a scrubbed cursor is tinted distinctly from the sweep; an axis top of 1,200 or more is never cut, with its own thousands separator; and the Help overlay lists exactly Signal's own keys, not Flow's or Cost's older generic list. `tui.rs`'s own golden tests cover the Signal screen at 120×30 and 80×24 against the design's own frames, replayed from the `run2` fixture frozen 3.62s after its own start with three pooled history runs. `oscilloscope/tests/e2e_tui_pty.rs` presses `1` during a live `cof` run and checks the Now sweep and the running step's own row, scrubs, zooms and returns to Now, opens and closes the full-screen step view, returns to the Tape with `0`, and separately checks that 80 columns drops the Readings pane and that below 80 columns Signal bounces back to the Tape on its own, picked up on the next redraw with no key needed.
- **The one summary function (issue #739).** `oscilloscope-core/src/summary.rs` unit-tests `json_value_summary`/`first_words`/`first_sentence_end`/`is_json_shaped` directly: a top-level string, a JSON-looking string that never parses (kept to the first-words rule), a string that genuinely does parse as an object or an array (summarised exactly as if it had arrived typed), null, a number and a boolean, an object with and without a summary-like field (tried in order, a blank one skipped), a nested empty object and array reading `0 keys`/`0 items`, a single item or key reading the singular (`1 item`/`1 key`), mixed value kinds in one object, an array of strings and of non-strings, an empty array, and cutting a long result to width with `⋯`. `oscilloscope-core/src/screens.rs` covers a finished prompt's own JSON reply on the Tape row and the Signal's own step row, an agent step whose `meta.raw` has nothing recorded falling through to the same summary on its own value, and the Flow rail's own selected pass pane summarising a prompt's JSON reply rather than dropping it; `oscilloscope-core/src/render.rs` covers a non-prompt step's own JSON value the same way, and that a plain-text, non-JSON reply or value keeps its own full preview rather than the summary's first-words cut at its first sentence. Issue #745's two fixes: the detail pane's own title cut (`tui::tape_detail_title_cut`) is unit-tested directly for a two-segment breadcrumb (dropping the first segment entirely rather than hard-clipping the step's own name), plus a `tui.rs` golden-style test over a real mid-run snapshot of `tests/fixtures/nested_tree_loops` at 120x30 for the deeper breadcrumb case; the same fixture's golden test covers `■ Run Started`/`■ Run End` reading at the exact column (in cells, not bytes) every other row's own widened name field starts at. `oscilloscope/tests/e2e_tui_pty.rs`'s `json_replies_read_as_short_summaries_not_raw_json` scripts an object with a summary field, an object of lists, an array of strings, an array of objects, a string holding JSON and plain text as a live `cof` run's own replies, and checks the replayed screen's finished rows: each reads its own short summary, and none shows `{"`/`["` anywhere.

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

