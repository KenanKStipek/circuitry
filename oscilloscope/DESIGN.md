# oscilloscope — design

Moved verbatim from issue #418's "Full design" section (the owner's decisions
and the design that followed them), so it lives with the code it describes.

---

# oscilloscope (`osp`): design

`osp` launches an orchestration on either engine (`cof` or electricity) and shows it running. It reads two things: the plan, which is the compiled document, and the run's live output. From these it keeps a status for every effect and writes a readable log. The first front end is a TUI. A GUI can come later and reuse the same core.

```
CYBERDINER_TOKEN=... osp do-thing.yml config.json
```

**Decided by the owner:**

- osp lives in its own top-level folder, `oscilloscope/`, in the Circuitry repository. It is its own Cargo workspace, uses electricity's crates by path, and has its own CI workflow and release jobs.
- Configs are JSON only and are passed to the engine unchanged. The CLI is `osp <orchestration> [config.json]`, and the environment is inherited.

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
| Later writes | At least 0.5 s apart. Each write follows an effect completing or a `Store.set` call. Writes are coalesced: p2 (12.6 s) produced 20 distinct snapshots, and p4 (6.4 s) produced 8. |
| Final write | Equal to `--out`. `runtime.last_run.completed_at` and `runtime.last_run.totals` are now set, and `prime.value` is `true` (ok) or `false`. |
| Second SIGINT during cleanup | **No final write and no `--out`.** The file stays at its last mid-run snapshot, still with `completed_at: null`. |

A write serialises the shared state dict at write time, not at the moment of the event that triggered it *(from code: `cli/live_state.py`)*. As a result, a write can include nodes created after its trigger. Running nodes are captured only by chance (§1.4).

### 1.2 Fields per effect type

Every node has the shape `{value, meta}`. Every `meta` has `created_at`, `completed_at` (ISO-8601 UTC, `null` while running) and `error` (`null` or a string).

| Effect | `value` | Other `meta` fields seen |
|---|---|---|
| root `prime` / `dynamic` | `null` → `true`; `false` when a failure was swallowed (`on_error: continue`) or failed the run | `adapter` (`"_noop"` when no prompt needs one), `model`, `tokens_sent`/`tokens_received` (always `null`), `flow` (`chain`/`tree`), `dry_run`, `labels`. On failure: `error: "<child path>: <message>"`, e.g. `"guarded.g_fail: /bin/ls failed (exit 1): ls: ..."`. |
| `tool` (shell) | stdout, e.g. `"x\n"` | `provider`, `stdout`, `stderr`, `exit_code`, `waiting_for`, `params_rendered`, `binary`, `raw`. **When the command fails, `exit_code`, `stdout` and `stderr` are `null`.** The exit code appears only inside `error` (`"/bin/ls failed (exit 1): <stderr>"`). |
| `tool` (json) | the parsed value | the same keys; `exit_code: null`, `raw: {mode}` |
| `tool` + failed `expect` | `null` | `error: "expect failed: value == 'yes'"`, `expect: {mode, expr, result: false}`. `stdout` and `exit_code: 0` are kept. |
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
| A running leaf in **chain** flow | **Only by accident.** Its node (`completed_at: null`) appears if a write happens while it runs. Seen in p2 (`prime.gate_taken.chosen` during its 1 s sleep). Not seen in p4: no write occurred during `flaky`'s 2 s retry window. |
| A running branch or pass in **tree** flow (dynamic or `each`) | **Never.** Branches run in isolated stores. A leaf appears only when it completes, and a nested container inside a branch (`prime.fan.inner`) appears only when the whole container completes. The loop node's `progress.done` is the only live sign. |
| A running child of a `use` | **Never in-flight.** The `use` node shows `completed_at: null`, and its children appear afterwards. |
| A queued branch (under `max_concurrency`) | No. Nothing tells a queued branch from a running one. |
| A retry in progress | Tool: yes, from state alone — `completed_at` going back to `null` (running again) with the same `created_at` as the attempt that just failed (§2.1 rule 6, §2.4). A leaf that *completes* again with the same `created_at` as an earlier failure, with no running sighting in between, gets the same `↻` retroactively alongside its own end line. Prompt: no. |
| A prompt in flight | Only as a chain-flow node with `completed_at: null`, and `prompt_sent` is already set. Model, adapter and the rendered prompt are visible. Elapsed time is not. |
| A branch cancelled by `stop_on_error`, or an effect after a failure | No node at all. Hooks do not fire for it either. |
| An effect interrupted by SIGINT/SIGTERM | **It stays `completed_at: null`, `error: null` in the final state.** Its enclosing loop also stays `completed_at: null`. `prime.meta.error` is `"Interrupted (Ctrl-C/SIGINT)"` or `"Interrupted (SIGTERM)"`, and `finally` effects run and appear. |

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

Two inputs feed these rules: the plan (§5), and a sequence of observations. An observation is a snapshot, or an event once events exist (§3). **Event time comes from `meta.created_at` and `meta.completed_at`, never from when osp noticed a change.** Log lines are sorted by those times. A short effect that starts and ends between two snapshots still gets its line, in the right place.

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
   - A tree `each` without `max_concurrency` defaults to `min(32, cpu + 4)` workers. osp cannot know `cpu`, so it must say "≤ N".
   - The completed count comes from `meta.progress.done`.
   - **With events:** the container's own `dispatch` event gives an exact bound instead of this estimate — `branches` is the true total (so progress reads "`done` of `branches`" instead of the plan's own, possibly data-dependent, count), and `concurrency`, when the stream has it, is the exact running ceiling in place of the `max_concurrency`/`cpu`-guess above.
5. A running `use`: its plan children (from compiling a `path` child) follow rule 1. An `inline` child has no static plan, so its children are discovered from the observations.
6. A retry: a running tool node whose `created_at` moved forward is **retrying (attempt ≥ 2)** — kept for whichever engine/version still moves `created_at` between attempts, though neither cof nor electricity do since issue #421. The current rule: a tool node that was non-running with an error and is running again with the *same* `created_at`, or that completes again with the same `created_at` as an earlier failure, is **retrying** too (§2.4's table). A prompt retry cannot be detected from state either way.
7. Run status (one function, shared by the TUI header, the plain `--log` summary and `osp watch`):
   - `runtime.last_run.completed_at` set means the run **ended**: **ok** if `prime.meta.error` is `null`, **cancelled** if it starts with `"Interrupted"` (a confirmed `q`, a Ctrl-C cancel, or a forwarded SIGINT/SIGTERM/SIGHUP always reach this branch, §4.1), otherwise **failed**.
   - The process exited with no ended state: **failed** only when the exit code is exactly 1 *and* there is a pre-execution failure reason (`cof`'s own stdout JSON, or a `run_end` event's error) -- the shape of a document invalid enough that nothing ever ran (§4.1). Otherwise **aborted** (a second signal, SIGKILL, a crash: no final write). `osp watch`, which owns no process of its own, reads `--events`' own `run_end.ok == false` in place of the exit code.

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

Values are never logged in full: one line, with a character cap. The details pane shows the full value on request.

---

## 3. Engine addition: an `--events <file>` JSONL stream (recommended)

**The problem.** Live state cannot answer "what is running now":

- tree branches and `use` children are invisible while they run;
- a chain leaf is visible only if a write happens to land while it runs;
- writes are coalesced to every 0.5 s or more;
- each write serialises the whole state.

The runtime already has exactly the right hooks (`on_effect_start`/`on_effect_complete`, §1.4).

**Other options rejected:**

| Option | Why not |
|---|---|
| The `jsonl-file` runtime plugin | It needs a `plugins` entry in config, and osp passes configs unchanged. It has no start event. It reads `runtime_plugins.jsonl-file`, not `runtime.runtime_plugins` *(from code)*, and appends across runs by default. |
| A start marker written into the live state | Tree branches live in isolated stores until they merge, so they would still be invisible. Adding one changes the state shape, which both engines must reproduce byte for byte. |
| Flushing the mirror on start | Still misses tree branches and `use` children, and costs a full serialisation per start. |

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
| `dispatch` | Sent once by a tree loop or tree dynamic before its branches start. This is the existing `concurrent_dispatch` callback. `branches` is the true branch count (an `each` loop's item total, or a `dynamic`'s effect count); `concurrency`, when present, is the ceiling — at most this many run at once. `concurrency` is optional: a stream from a `cof` built before it was added has `branches` alone, and osp still works, just with a less precise bound (DESIGN.md §2.1 rule 4). |
| `error` | Present only when `ok` is false. Its first 500 characters, already redacted. |

What the format leaves out, and why:

- **No values and no prompt text.** osp reads those from the live state.
- **No effect kind.** osp takes it from the plan, or from the shape of `meta` for generated children.

**Rules:**

- **Ordering.**
  - A container's `start` comes before any child's `start`.
  - Every child's `end` comes before the container's `end` (measured: true for the hooks today).
  - Tree branches may interleave.
  - `run_start` is the first line. When `run_end` is written, it is the last line, written after the final live-state write. So when osp sees `run_end`, the final snapshot is already on disk.
- **Flushing.** The file is opened and truncated before the first effect. Each event is written with **one `write()` of one whole line, under one lock, and flushed at once**. There is no buffering across events, so a reader tailing the file never sees a torn line, except at EOF while a write is in progress. Readers keep any trailing partial line until its `\n` arrives.
- **Failures.** A failure to write is logged once and then ignored. It never fails the run, the same as the live mirror.
- **Abort.** No `run_end` is written after `os._exit` (second signal). osp treats EOF with no `run_end` plus a dead process as aborted.

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

osp makes a run directory and launches the engine with these files in it:

- `state.live.json`
- `state.json`
- `events.jsonl`
- `stdout.txt` and `stderr.txt`

By default the directory is a new temporary one, printed when the run ends; `--out-dir <dir>` chooses it instead. These are the same file names a `cof run --live-state <dir>/state.live.json --out <dir>/state.json` user already has, so `osp watch <dir>` (§7) can attach to such a run.

### 4.1 `cof`

```
cof run <orchestration> [--config <config.json>] --quiet \
    --live-state <dir>/state.live.json --out <dir>/state.json --events <dir>/events.jsonl \
    [-e key=value]...
```

- **Config.** With no config, `--config` is left off, and `cof` resolves its usual layers: the global config, a *trusted* project config in the working directory, and env vars. `--config` replaces the global and project layers and is always trusted.
- **Inputs.** `-e` values are JSON-sniffed by `cof`, so osp passes them through untouched. osp accepts `-e`/`--state` after the positionals; this does not change the owner's CLI form.
- **stdin is `/dev/null`.** Nothing interactive can then hang:
  - capability consent prompts only when stdin *and* stdout are TTYs and neither `--quiet` nor `--json` is set; otherwise it refuses with a message naming `cof trust <doc>` *(from code: `cli/app.py` `_capability_prompt`)*;
  - an untrusted project config is never a prompt, only a one-line stderr warning.
- **stdout is a pipe** (measured with `--quiet --out`):
  - success: stdout is empty;
  - failure, validation errors included: it holds one JSON object, `{"ok": false, "error": "…", "warnings": [], "state_out": "…"}`. osp parses it for the final error line.
- **stderr is a pipe.** It carries `WARNING: …` log lines and `Warning: …` notices. Each line becomes an `engine` line in the log pane.
- **Process group.** osp spawns the engine with `CommandExt::process_group(0)`, so that terminal signals reach osp only and osp alone decides what to forward.
- **Side effect.** `cof run` updates the user's `--last` stash. This is accepted, and documented.

**Signals.** In raw mode, Ctrl-C is a key, so osp forwards signals itself (measured, sent to the engine's process group):

| Action | Engine result |
|---|---|
| first Ctrl-C → SIGINT | exit **130**. The interrupted effect is left open, `finally` runs, and the final snapshot plus `--out` are written (`error: "Interrupted (Ctrl-C/SIGINT)"`). |
| second Ctrl-C (0.24 s later) → SIGINT | exit **130** at once: no `finally`, no `--out`, no final snapshot, and no stdout. |
| SIGTERM to osp → SIGTERM | exit **143**, same as the first SIGINT, with `"Interrupted (SIGTERM)"`. |
| SIGHUP to osp (terminal closed) | forward SIGHUP: exit 129, handled like SIGTERM *(from code: `cli/interrupts.py`)*. |

osp behaviour:

- **First Ctrl-C:** send SIGINT and show "cancelling, finally running…".
- **Second Ctrl-C:** send SIGINT again.
- **Still running 10 s after the second:** send SIGKILL to the group.
- **`q` while a run is going:** ask "cancel run?" first.
- **Exit code:** osp exits with the engine's code.

### 4.2 electricity

**M0-H's run wiring (issue #441) landed electricity's own side of
this; issue #431's lane E2 wires osp to it.** electricity is still a
preview: `electricity/README.md`'s preview section is the single
source of truth for what it runs, and it refuses everything else up
front, before any state is written, with a message naming the preview
marker (`is a preview and can't run this orchestration`,
`electricity_bytecode::PREVIEW_MARKER`). osp shows that refusal the
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
  neither flag, and osp falls back to the no-events path with the same
  notice `cof` gets ("this electricity has no --events: running from
  state only"), worded for whichever engine is actually missing it.
- Exit codes are exactly `cof`'s own: 0, 1, 2, 130 (SIGINT), 143
  (SIGTERM), 129 (SIGHUP). A config error has nothing on stdout and
  `Error: <text>` on stderr; an invalid document or a refusal prints
  `cof`'s own `{"ok":false, "error":...,"warnings":[...],
  "state_out":...}` shape on stdout instead, which osp's existing
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
  `--out`), so osp's plan join, status inference and `--log` output
  work unchanged.

**Argument order.** The owner's form is `osp <orchestration> [config]`; osp maps it to `electricity <config> <orchestration>`. Neither file can be recognised by its extension (an orchestration may be `.json`). osp therefore keeps the order strict. It gives a hint only when the first argument has no `effects` key and the second has one.

**Missing config.** With no config, osp tells the user that electricity needs one. It does not invent a `{}` config, because that would silently drop the user's global settings, which `cof` would have applied.

**End-to-end tests** (`oscilloscope/crates/oscilloscope/tests/e2e_electricity.rs`, mirroring `e2e_cof.rs`) run behind `OSP_E2E_ELECTRICITY=1`, against an `electricity` built with `--features test-tools` (its `sleep`/`fail` providers stand in for `cof`'s `shell`, which M0-H doesn't run) on `PATH`. `oscilloscope.yml`'s own `e2e-electricity` job builds that binary and runs them on Linux and macOS.

---

## 5. The plan side: what osp needs from `Program`

osp compiles the document with `electricity_compiler::check_for_run(path, &CheckOptions{skip_preflight: true, trust_document: true})`, linked by path, so the same plan serves both engines. From the IR it needs:

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
- Transparent ops add no segment, so their children match in the parent's scope. One concrete path can then match several plan ops: `then`/`else` reusing a name, or an unnamed `if`. osp keeps them as one merged row, with a list of candidate plan ops.
- `finally` ops share the container's scope.
- **`use` with `path`:** osp compiles the child file (with a cycle guard) and grafts its root's children under the `use` node, stripping the child's `prime`.
- **`use` with `inline`, and reflector `generated.iter_N`:** these have no static plan. Their rows are created as paths are first seen. The type is guessed from the shape of `meta`, the same rule the reference's SQL plugins use.

**Asks for the M0-G lanes:**

1. **A path display helper.** `impl Display for EffectPath`, rendering a pass as `iter_*`, plus a `matches(&str) -> Option<Vec<(LoopId, u32)>>`, so osp does not reimplement the join.
2. **Source positions.** An optional `span: Option<(line, col)>` on `Op`, so the details pane can show the YAML location. This is nice to have.
3. **A visitor.** `Program::walk()` or a visitor that yields `(depth, &Op)`.
4. **Stability.** The IR is documented as having "no stability promise". Since osp links by path in the same repository, a breaking IR change must update osp in the same PR, and the shared CI enforces that.
5. **A fallback.** osp must still work when compilation fails or is unsupported: no plan, and a tree built from observations only. This matters until M0-G lands.

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
    oscilloscope/                 # binary `osp`: ratatui + crossterm TUI and --log plain mode
  tests/fixtures/                 # recorded runs (scrubbed): plan, snapshots, events, stdout/stderr, exit code
```

`oscilloscope-core` has no terminal code, so a GUI (`oscilloscope-gui`) can reuse it later. The workspace version follows the repository version, like electricity.

Polling, every 100 ms, is preferred over `notify`. The live file is replaced by rename on every write, and polling is simpler to make correct across platforms.

### 6.2 Plain-text mode

`osp --log` is chosen automatically when stdout is not a TTY or `CI` is set. It prints the §2.4 lines with `mm:ss.s` timestamps, plus the engine's stderr lines (prefixed `engine:`), and ends with a summary line and the engine's exit code. Example (from p4):

```
00:00.0 ▶ flaky                 prompt scripted/scripted-model "flaky"
00:02.0 ✓ flaky                 2.0s  retries 2  ↑3 ↓1
00:02.0 ▶ always_fails          shell ls /nonexistent-osp-probe
00:02.9 ✗ always_fails          /bin/ls failed (exit 1): ls: … (on_error: continue)
00:03.9 ✗ guarded.g_fail        /bin/ls failed (exit 1) …
00:04.6 ✓ guarded.g_cleanup     0.7s  (finally)
00:05.7 ■ run ok  5.7s · 12 effects · ↑3 ↓1
```

### 6.3 TUI (shipped, issue #434; the layout, state pane, treemap and
token bar below, issue #527)

`ratatui` (`=0.30.0`, default features off, `crossterm` + `underline-color`
only) and `crossterm` `0.29` (the version ratatui's own crossterm backend
uses), both building on the workspace MSRV (1.86). `--log`, or any non-TTY
stdout (a pipe, a file, `CI` set), keeps §6.2's plain stream unchanged; a
TTY gets this instead, for both `osp <doc>` and `osp watch`.

The render model is `oscilloscope-core`'s own (`render.rs` and `treemap.rs`):
a `Header`, the plan tree flattened to depth-first `Row`s, the state pane's
own `StateRow`s, the `tokens by key` treemap's own tree of `TreemapNode`s,
the `TokenBar`'s totals and in-flight line, and a selected row's `Details`
— no terminal code, so a GUI front end could lay the same `RenderState` out
its own way. Only `oscilloscope`'s `tui.rs` imports `ratatui` widgets; the
squarified treemap layout itself (`treemap::layout`, below) is pure
integer geometry, deterministic for the same input, computed in
`oscilloscope-core` and only ever drawn, never computed, by `tui.rs`.

**Layout.** A header across the full width; a left column of the
orchestration pane over the details pane; a right column of the state
pane, the `tokens by key` treemap and the log pane; and the token bar
across the full bottom width. On a terminal at least 120×40, every pane
shows useful content. Below that, panes drop in this order until the rest
fits — the header and the token bar never disappear:

1. below 100 columns or 30 rows (`thresholds::TREEMAP_MIN_WIDTH`/
   `_MIN_HEIGHT` in `tui.rs`): the treemap, leaving the right column as
   the state pane over the log pane;
2. below 90 columns or 24 rows (`STATE_PANE_MIN_WIDTH`/`_MIN_HEIGHT`): the
   state pane too, leaving the right column as the log pane alone;
3. below 80 columns or 20 rows (`TWO_COLUMN_MIN_WIDTH`/`_MIN_HEIGHT`): the
   two-column split itself, down to one column stacking the orchestration
   pane, the details pane and the log pane (45%/25%/the rest).

**Header (unchanged):** document, engine, run state (running / ok / failed
/ cancelled / aborted), elapsed time, effects done out of planned, tokens
↑↓, and the ETA of the innermost running loop. There is no run-level
ETA: plans have data-dependent loops. The run ID shows once `--events`
has carried a `run_start`; state alone never has one. "Planned"
multiplies a loop body's own leaf by that named loop's `meta.progress.
total` once it's known (so a four-pass loop's one-leaf body counts as
4, not 1), and "done" counts every finished pass separately; while any
crossed loop's own total isn't known yet, "planned" is a lower bound,
shown with a trailing `+` (`3/3+`).

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
    or model in a dim colour.

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

**The details pane** moves under the orchestration pane; its own content
is unchanged. It scrolls when it has focus (`↑↓`, `PgUp`/`PgDn`,
`Home`/`End`, `g`/`G`, via `ratatui::widgets::Paragraph::scroll`), and
returns to the top the instant the orchestration pane's own selection
changes (`App::select_tree_path`).

**Focus** (`tab`) cycles orchestration → details → state →
orchestration, each highlighted with the existing cyan border. The
treemap and the token bar have no pane of their own to focus.

**The state pane** (`render::build_state_rows`, `tui.rs`'s `draw_state`).
Lists `flatten_state`'s own `{value, meta}` paths (every one but `prime`
itself), most recently updated first — a node's update time is its own
`completed_at`, or `created_at` while it has none, parsed as a real
instant (`diff::parse_instant`, now `pub`) rather than compared as a raw
string, with ties broken by path. Each row shows the path relative to
`prime`, then either: the first line of its error, in red, if it has one
(a multi-line stderr tail would otherwise break the pane's own
one-line-per-entry shape); its own abbreviated value (one line, compact
JSON for anything but a string, a quoted string for a string, its own
newlines escaped) if it has one; or its status glyph, if it's running
with no value yet. A second line shows `↑sent ↓received`, only when at
least one is nonzero — a leaf's own tokens, or (for a container) the sum
over its whole subtree (`model::subtree_tokens`). The pane scrolls when
it has focus, with its own selection (`App::state_selected_path`) kept
fully independent of the orchestration pane's own — selecting a row here
never moves the orchestration selection, and vice versa.

Both the path label and the value/error are cut to the pane's own real
width with `…` (`tui.rs`'s own job, not `render.rs`'s — the pane's
width is a terminal concern): the label gets a *left*-truncated share of
it (`tui::truncate_label_left`, `state_pane_label_width`, scaling with
the pane), so two entries sharing a long common prefix (`loop.iter_2.
step`/`loop.iter_2.other`) stay distinct — a right-truncated `loop.
iter_2…` would show identical text for both; the value/error gets
whatever's left, truncated from the right (`tui::truncate_label`), since
its own start is usually the more identifying part.

**The `tokens by key` treemap** (`oscilloscope-core/src/treemap.rs`,
drawn by `tui.rs`'s `draw_treemap`). A proportional-area chart of the
current state's token totals. The root cell is `prime` itself
(`treemap::build_tree` is just `build_node("prime", ...)` — the root
gets no special case at all, since `build_node` already treats any
container and its own children identically): a single `TreemapNode`
carrying the whole tree's `node_tokens` sum and its own children,
laid out by `treemap::layout`, the squarified algorithm (Bruls, Huizing
& van Wijk, 2000): each row of cells is grown for as long as doing so
keeps that row's worst aspect ratio no worse than before, then placed
along whichever side of the current rectangle is shorter; row
boundaries are computed once in `f64` so every cell in a row shares the
exact boundary value its neighbour does, and only then rounded to the
integer grid, which is what keeps the rectangles gap- and overlap-free.
Sibling cells are always ordered largest-total-first, ties broken by
key, so the picture doesn't jump between redraws when nothing changed.
A key with no tokens anywhere in its own subtree gets no cell at all, at
any depth; when nothing has tokens yet (including a zero-total root),
the pane shows `(no tokens yet)` instead of drawing an empty root cell.
A container is drawn as a bordered cell, titled with its own key,
holding its children's cells recursively, but only once its own rect is
at least 3×3 (room for a border plus at least one interior cell) — a
smaller container is drawn as a plain, unbordered cell instead, same as
a leaf. A leaf (or a too-small container, or a merged `…` bucket) has no
border of its own, so its *background* fill is what makes it read as an
area at all against its neighbours — an `fg`-only label over the
default terminal background, the previous behaviour, left two adjacent
cells with no visible edge between them whatsoever. A cell's own label
is cut with `…` to fit, drawn in a contrasting foreground over that same
fill; a cell with `rect.w < 2` draws no label at all, just its own
background colour (`Style::bg`, not `fg` — styling with `fg` alone draws
nothing when there's no text to colour). A cell under 2×1 is "too small
to draw at all": every such cell under one parent is merged into a
single `…` cell, sized by their combined total, and the parent is
squarified again with that merged entry standing in for all of them —
repeated, folding in whatever is newly too small each time, until a pass
leaves nothing new to fold in (a second pass can itself still produce a
survivor, or the merged bucket's own cell, under 2×1 one level removed).
Cells are coloured by status (running yellow, done green, failed red),
and the cell for the row currently selected in the orchestration pane
(matched by its own full path, which every `TreemapNode`/`TreemapCell`
carries alongside its key) is drawn reversed.

**The log pane** moves here from the bottom, otherwise unchanged — the
same lines `--log` prints (§2.4), plus the engine's own stderr, as a
scrolling tail (no scrollback in this milestone: always the most recent
lines that fit).

**The token bar** (`tui.rs`'s `draw_token_bar`), a bordered strip across
the full bottom width, two content lines:
  - **Totals:** `in`/`out` as horizontal bars (`█`), scaled to the larger
    of the two, with exact counts (thousands separators) — always equal
    to the header's own `↑`/`↓`, since both read the token rule below.
    Each bar's own width is what's left of the line after its label and
    *both* counts' own measured width, halved between the two bars — a
    bar sized without first reserving room for the counts could leave
    one of them (the whole reason the line exists) cut off entirely on
    an ordinary terminal width.
  - **What's in flight:** the estimated input summed over every prompt
    currently running, `≈ceil(chars(meta.prompt_sent) / 4)`, and how many
    prompts that is. No output estimate: neither engine reports a
    prompt's tokens before it finishes. A prompt counts as running
    (`model::is_running_prompt`) exactly when it has `created_at` and no
    `completed_at`, `meta.prompt_sent` has actually been written, and
    `meta.waiting_for` is empty — a prompt merely queued behind a
    concurrency slot isn't processing yet. With nothing running, the
    line says so ("nothing in flight").

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
treemap all read tokens this same way (`model::node_tokens`/
`subtree_tokens`), so a retried prompt can never make one surface
disagree with another; `model::tests::subtree_tokens_matches_run_totals_
for_a_retried_prompt_on_cof`/`_on_electricity` pin it against that same
recorded shape from each engine.

**Keys:**

| Key | Action |
|---|---|
| `↑↓`/`jk` | move (the focused pane's own selection, or the details pane's scroll) |
| `PgUp`/`PgDn` | move a page (the focused pane's own last-rendered interior height) |
| `Home`/`End`, `g`/`G` | jump to the first / last row |
| `←→` | collapse / expand (orchestration pane) |
| `f` | follow the running row (on by default) |
| `e` | errors only (keeps a failed leaf's own ancestors so there's still a tree to show it under) |
| `/` | filter (substring, on path or label; same ancestor-keeping rule) |
| `tab` | orchestration → details → state |
| `v` | full value, in an overlay; `v` again cycles between `prompt_sent` and the value when the row has both (opening on `prompt_sent` first when there is one); any other key closes it |
| `c` | cancel, with a confirm — the same as a *confirmed* `c`/`q` always has (`do_run`'s own signal-forwarding/kill-escalation path, not a separate one) |
| `q` | quit; asks first if a run is going (confirming sends the same cancelling signal as `c`, and leaves the instant the engine exits — see "the finished screen" below); with nothing running, quits at once, no confirm; in `osp watch`, always just detaches, with no confirm — watch owns no engine to cancel |
| Ctrl-C | checked before any dialog, and never merely dismisses one: forwards SIGINT at once with no confirm while something's running (a second Ctrl-C starts the same 10s kill deadline a real repeated SIGINT would), quits at once with nothing running, and in `osp watch` always just detaches |
| `?` | help |

**The finished screen.** The engine exiting (or, for `osp watch`, the
watched run ending) does not, on its own, end this loop: the header, every
row's final status, and the details pane (`v` included) stay live and
navigable on whatever the run ended with — ok, failed, cancelled or
aborted — until the user explicitly leaves. The instant this loop first
observes the ending (the engine's exit, or `osp watch`'s own events-based
or dead-pid stop condition), it drains once more before ever rebuilding
this screen: events tailed until a pass turns up nothing new, then the
same authoritative final read `finish_run`/`finish_watch` themselves take
— a fast-exiting engine's last events and final write (§1.1: writes land,
then it exits, with nothing left to ever prompt another poll afterwards)
are never missed, and this screen can't show a real `ok`/`failed` ending
as `aborted` just because nothing is still running to eventually correct
it. Only three things make this loop leave the instant the engine has
exited, rather than staying on that final state: a confirmed `q`, a
Ctrl-C cancel, or any external INT/TERM/HUP osp itself received (a SIGHUP
in particular never waits for
a key — the terminal is gone by definition). Short of one of those three,
a run that simply finishes on its own stays up for `q`/Ctrl-C to leave.
`osp watch`'s own `q` while the watched run is *still going* prints a
distinct "■ detached (the run is still going)" ending and exits 0, rather
than the generic "no final state" abort every other stop condition
without one falls into; `q` once the run has already ended gets the
normal ok/failed/aborted summary instead. `finish_run`/`finish_watch`
still print the usual plain summary once the terminal session is torn
down, exactly as they already do for `--log`.

**Terminal safety.** A `TerminalGuard` enters raw mode and the alternate
screen once (undoing raw mode again if the alternate screen fails to
open); `Drop` restores both on every normal return from the TUI loop
(osp's own error, the engine exiting, a confirmed quit), and a panic hook
installed alongside it restores the terminal before the default panic
handler's own message would otherwise print into it. A forwarded
SIGINT/SIGTERM/SIGHUP is forwarded to the engine as the *same* signal
(never turned into a SIGINT regardless of which arrived), with the same
escalation `do_run`'s plain loop applies, and never leaves the TUI loop
early on its own — the loop keeps rendering until the engine actually
exits, so `Drop` still runs on the normal path out. If the TUI itself
can't start (an exotic CI pty, a stdout swapped out from under osp), both
`osp <doc>` and `osp watch` fall back to their own plain supervise/watch
loop rather than a bare, signal-blind wait. `osp` itself being
`SIGKILL`ed is the one exception (§4.1): nothing can run cleanup code
after that at all. A redraw, and the render state (plan-tree rows, state
rows, the treemap's own tree, header counts) it draws, are rebuilt at
most once every `REDRAW_INTERVAL` and only when an observation, a key or
a resize actually changed something — forced at least once a second
regardless, so the header's own elapsed time still visibly ticks while
the run is otherwise quiet. The state pane and the treemap tree are both
built from the one `flatten_state` call `render::build` already makes
for the plan tree, not a second pass of their own; measured against a
synthetic 2,000-node state, one redraw stays well under
`REDRAW_INTERVAL` (`render::tests::redraw_from_a_2000_node_state_stays_
well_under_the_redraw_interval`). That measurement times `render::build`
alone, which leaves out the treemap's own layout and the ratatui draw
itself; `tui::golden_tests::drawing_a_2000_node_state_stays_well_under_
the_redraw_interval` times one `TestBackend` draw of the same state too.

### 6.4 Tests

- **Golden tests, with `insta`.** Each fixture is a recorded run: plan, snapshot sequence with times, events, stdout/stderr and exit code. The fixtures are the §1 probes, with paths scrubbed. Two outputs are checked against goldens:
  - the status table after each observation;
  - the `--log` output.

  Cases cover every row of §2, including events-only, state-only and abort (no final snapshot).
- **TUI render tests (shipped, issue #434; the new layout and panes, issue #527).** `ratatui::backend::TestBackend` buffers (`oscilloscope/src/tui.rs`'s own `golden_tests` module, `insta`), one per state the acceptance list named: pending plan, running chain, a running tree loop with `max_concurrency`, a loop with two of its passes already done, queued-under-concurrency, failed, failed and handled, cancelled, aborted (no final state) and `osp watch` — at 100×24 (two-column: the right side is the state pane over the log pane, no treemap), since that is what most of these actually want to show (the orchestration review found the previous blanket 70×20 lost the details pane's own type/plan/provider lines and the state pane entirely). `pending_plan` and `aborted_no_final_state` stay at 70×20 deliberately, as this suite's own one-column-fallback coverage. `fallback_at_80x24_drops_the_treemap_and_the_state_pane` and `fallback_at_100x30_is_the_smallest_full_layout` cover the two further thresholds boundary-adjacent to those: 80×24 is two-column with both the treemap and the state pane dropped, and 100×30 is the smallest size at which nothing is dropped at all. `full_layout_with_state_treemap_and_token_bar`, at 120×40, is the most comfortable size, with every pane populated, including a nested treemap cell under the `prime` root cell. `follow_picks_the_running_row_not_a_hard_coded_one` builds its own `App` and drives the selection through `App::follow_target`, the same way `main.rs`'s own redraw loop does, rather than hard-coding `selected_path` to a row that happens to be running — every other golden here does the latter, which (correctly) also turns follow off, since setting `selected_path` directly bypasses the key handling a real manual selection goes through. `a_long_plan_scrolls_to_keep_a_followed_effect_below_the_fold_visible` is a synthetic 48-leaf chain with the 45th leaf running and followed, at 70×20: the first leaf scrolls out of view, the followed one stays visible, and the border shows a `45/49`-style position.
- **Key handling (shipped, issue #434; the new navigation, issue #527).** Unit tests in `oscilloscope/src/keys.rs`: navigation, `PgUp`/`PgDn`/`Home`/`End`/`g`/`G` (against a ten-row synthetic plan, with an explicit `tree_viewport` standing in for `tui.rs`'s own last-rendered height), collapse/expand (with a trailing `.` so a same-prefix sibling is never hidden too), follow (on by default, and turned off by every manual move), errors-only, filter, `v`'s own prompt-first-then-cycle behaviour, both confirm dialogs (`c`, and `q` while a run is going), Ctrl-C's own action (checked before, and never merely dismissing, a dialog), the three-way `tab` cycle (including skipping straight past the state pane when it isn't drawn at all, `state_viewport == 0`), the state pane's own selection staying independent of the orchestration pane's, and the details pane's own scroll resetting whenever the orchestration selection changes — pure, with no real terminal. `tui.rs`'s own `scroll_tests` module unit-tests `clamp_scroll`/`scroll_indicator` and the state pane's line-aware `clamp_state_scroll` directly, beyond what the rendered goldens already exercise indirectly.
- **The render model (`oscilloscope-core/src/render.rs`).** Unit tests for the loop-pass multiplier/lower-bound on "effects planned", and the stderr tail's own reading order; `oscilloscope-core/src/model.rs` and `plan.rs` cover an unnamed `if`/`else`'s own two branches (neither pollutes the other's earlier-siblings, and an untaken branch resolves once the taken one is observed, rather than blocking every chain sibling after it forever). The state pane's own order, abbreviation (including a multi-line error showing only its first line), container token sums over a loop, a tree dynamic and a `use` child, are covered on synthetic snapshots; the token rule is pinned against each engine's own real recorded state for a retried `prompt_type: json` prompt whose first reply fails to decode (`model::tests::subtree_tokens_matches_run_totals_for_a_retried_prompt_on_cof`/`_on_electricity`); the token bar's own running-prompt filter (`waiting_for` excludes a queued prompt) and estimate are unit-tested; and the 2,000-node redraw measurement above lives here too.
- **The treemap (`oscilloscope-core/src/treemap.rs`).** Unit tests show that cell areas are proportional to totals within rounding, children stay inside their own parent's rect, a zero-token key gets no cell (including a zero-total root, which produces an empty tree, not an empty root cell), many tiny children merge into one `…` cell sized by their combined total (repeating the merge pass until nothing new needs folding in, for a survivor the second pass itself leaves too small), a container lays its own children out inside its own border, and the same input always produces the same rectangles regardless of the order the caller happens to list it in (an input and a shuffled copy of it both tested against the same expected layout).
- **Terminal restore (shipped, issue #434).** Unit tests in `oscilloscope/src/terminal.rs` prove `TerminalGuard`'s own entered/restored bookkeeping, and that the installed panic hook itself (not a direct `restore()` call) clears it through a real `catch_unwind`; `oscilloscope/tests/e2e_tui_pty.rs`'s `a_forwarded_sigint_still_restores_the_terminal` proves the real escape sequences on a real pseudo-terminal, after a forwarded SIGINT.
- **End-to-end with `cof`** (`tests/e2e_cof.rs`, plain mode; `tests/e2e_tui_pty.rs`, the TUI on a real pseudo-terminal via `posix_openpt`/`grantpt`/`unlockpt`/`ptsname` — no pty crate dependency), gated on `OSP_E2E_COF=1` (`cof` on `PATH` required once set):
  - the scripted adapter;
  - a temporary `HOME`;
  - credential variables removed (the four named in this repository's CLAUDE.md);
  - a hard timeout per test;
  - one test per probe shape, plus SIGINT once, SIGINT twice and SIGTERM, asserting exit codes and final statuses;
  - the pty tests additionally assert the alternate-screen entry/exit escape sequences, and that nothing osp or cof started survives, with no `osp-*` directory left in the real system temp directory (every pty test uses its own `--out-dir`); since the TUI now stays open on its own final state, every pty test that lets a run finish on its own sends `q` to leave, and three further pty tests assert SIGTERM (143), SIGHUP (129) and a raw Ctrl-C byte, `0x03` (130, since raw mode disables the kernel's own `ISIG`) in the TUI specifically.

  Live tests are never run.
- **End-to-end with electricity** (`tests/e2e_electricity.rs`, issue #431's lane E2): the same shape as `e2e_cof.rs`, against an `electricity` built with `--features test-tools`, behind `OSP_E2E_ELECTRICITY=1`.
- **Engine parity** (`oscilloscope-core/tests/engine_parity.rs`): a handful of documents built only from electricity's M0-H scope (`tool`/`json`, `dynamic`, a CEL `if`, `finally:`), recorded against both engines under the same config (`scripts/record_fixtures.py --parity-only`), assert the same `--log` lines apart from each effect's own duration.

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
| Q5 | Attach mode, `osp watch <dir \| live.json> [--plan doc.yml]`, for runs osp did not start? | **Yes in v1.** It costs little, and it covers the existing `--live-state <dir>/state.live.json` habit. |
| Q6 | Where do run files go by default? | **A temp dir**, printed at the end. `--out-dir` keeps it. |
| Q7 | How much prompt and reply text to show? | **None in the log** beyond a 60-character preview. Full text only in the details pane, on request. |
| Q8 | Should `cof` make interrupted effects explicit in state (e.g. `meta.error: "cancelled"`) instead of leaving `completed_at: null`? | **Yes, upstream, both engines.** Today a reader cannot tell "cancelled" from "still running" without the run-level error. |
| Q9 | Make tool retries record `retries_used` on failure, and settle on one `created_at` rule (first attempt or last) for tools and prompts? | **Yes, upstream.** Pick the first attempt for both, so durations include backoff. **Done for `cof` and for electricity's own tool retry loop (issue #421).** |
| Q10 | Windows? | **Not in v1:** process groups and signal forwarding are POSIX. |

The probes, the runner and the raw measurements are not part of this issue. They were run once from a scratch directory and can be recreated from §1's description.

