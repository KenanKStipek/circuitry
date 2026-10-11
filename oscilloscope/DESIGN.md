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
preview: it runs `tool`/`dynamic`/a CEL `if`/`finally:` documents only,
with `json` as its one tool provider, and refuses everything else
(`prompt`/`loop`/`use`/`reflector`/`yield`, a model-mode `if`,
persistence/runtime plugins, `--profile`) up front, before any state is
written, with a message naming the preview marker
(`is a preview and cannot run orchestrations yet`). osp shows that
refusal the same way it shows an invalid document: a pre-execution
failure, `■ run failed: ...`, exit 1 -- though only the refusal writes
nothing at all under the run directory; an invalid document still gets
its own `--out` (electricity's `fail!` macro saves state before
returning the error).

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

### 6.3 TUI (shipped, issue #434)

`ratatui` (`=0.30.0`, default features off, `crossterm` + `underline-color`
only) and `crossterm` `0.29` (the version ratatui's own crossterm backend
uses), both building on the workspace MSRV (1.86). `--log`, or any non-TTY
stdout (a pipe, a file, `CI` set), keeps §6.2's plain stream unchanged; a
TTY gets this instead, for both `osp <doc>` and `osp watch`.

The render model is `oscilloscope-core`'s own (`render.rs`): a `Header`, the
plan tree flattened to depth-first `Row`s, and a selected row's `Details` —
no terminal code, so a GUI front end could lay the same `RenderState` out
its own way. Only `oscilloscope`'s `tui.rs` imports `ratatui` widgets.

- **Header:** document, engine, run state (running / ok / failed /
  cancelled / aborted), elapsed time, effects done out of planned, tokens
  ↑↓, and the ETA of the innermost running loop. There is no run-level
  ETA: plans have data-dependent loops. The run ID shows once `--events`
  has carried a `run_start`; state alone never has one. "Planned"
  multiplies a loop body's own leaf by that named loop's `meta.progress.
  total` once it's known (so a four-pass loop's one-leaf body counts as
  4, not 1), and "done" counts every finished pass separately; while any
  crossed loop's own total isn't known yet, "planned" is a lower bound,
  shown with a trailing `+` (`3/3+`).
- **Left: the plan tree.** Each row shows:
  - a status glyph: `·` pending, `◐` running (also shown for a "likely
    running" guess), `◌` running/queued, `✓`, `✗`, `!` failed-handled,
    `↷` skipped, `⊘` cancelled, `?` aborted (no glyph of its own in the
    original table; `?` is the one status left with no better fit);
  - the duration, `n/total` for a loop's own progress, and the provider
    or model in a dim colour.
- **Right: details for the selected row.** Type, path, plan summary, a
  meta summary (provider/model, exit code, command, `waiting_for`,
  tokens, retries, a stderr tail), and the error. `prompt_sent` and the
  value are shown truncated (200 characters), with `v` showing either in
  full in an overlay.
- **Bottom: the log pane** — the same lines `--log` prints (§2.4), plus
  the engine's own stderr, as a scrolling tail (no scrollback in this
  milestone: always the most recent lines that fit).
- **Keys:**

| Key | Action |
|---|---|
| `↑↓`/`jk` | move |
| `←→` | collapse / expand |
| `f` | follow the running row |
| `e` | errors only (keeps a failed leaf's own ancestors so there's still a tree to show it under) |
| `/` | filter (substring, on path or label; same ancestor-keeping rule) |
| `tab` | switch pane |
| `v` | full value, in an overlay; `v` again cycles between `prompt_sent` and the value when the row has both (opening on `prompt_sent` first when there is one); any other key closes it |
| `c` | cancel, with a confirm — the same as a *confirmed* `c`/`q` always has (`do_run`'s own signal-forwarding/kill-escalation path, not a separate one) |
| `q` | quit; asks first if a run is going (confirming sends the same cancelling signal as `c`, and leaves the instant the engine exits — see "the finished screen" below); with nothing running, quits at once, no confirm; in `osp watch`, always just detaches, with no confirm — watch owns no engine to cancel |
| Ctrl-C | checked before any dialog, and never merely dismisses one: forwards SIGINT at once with no confirm while something's running (a second Ctrl-C starts the same 10s kill deadline a real repeated SIGINT would), quits at once with nothing running, and in `osp watch` always just detaches |
| `?` | help |

**The finished screen.** The engine exiting (or, for `osp watch`, the
watched run ending) does not, on its own, end this loop: the header, every
row's final status, and the details pane (`v` included) stay live and
navigable on whatever the run ended with — ok, failed, cancelled or
aborted — until the user explicitly leaves. Only three things make this
loop leave the instant the engine has exited, rather than staying on that
final state: a confirmed `q`, a Ctrl-C cancel, or any external
INT/TERM/HUP osp itself received (a SIGHUP in particular never waits for
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
after that at all. A redraw, and the render state (plan-tree rows,
header counts) it draws, are rebuilt at most once every `REDRAW_INTERVAL`
and only when an observation, a key or a resize actually changed
something — forced at least once a second regardless, so the header's own
elapsed time still visibly ticks while the run is otherwise quiet.

### 6.4 Tests

- **Golden tests, with `insta`.** Each fixture is a recorded run: plan, snapshot sequence with times, events, stdout/stderr and exit code. The fixtures are the §1 probes, with paths scrubbed. Two outputs are checked against goldens:
  - the status table after each observation;
  - the `--log` output.

  Cases cover every row of §2, including events-only, state-only and abort (no final snapshot).
- **TUI render tests (shipped, issue #434).** `ratatui::backend::TestBackend` buffers (`oscilloscope/src/tui.rs`'s own `golden_tests` module, `insta`), one per state the acceptance list named: pending plan, running chain, a running tree loop with `max_concurrency`, a loop with two of its passes already done, failed, failed and handled, cancelled, aborted (no final state) and `osp watch`.
- **Key handling (shipped, issue #434).** Unit tests in `oscilloscope/src/keys.rs`: navigation, collapse/expand (with a trailing `.` so a same-prefix sibling is never hidden too), follow, errors-only, filter, `v`'s own prompt-first-then-cycle behaviour, both confirm dialogs (`c`, and `q` while a run is going), and Ctrl-C's own action (checked before, and never merely dismissing, a dialog) — pure, with no real terminal.
- **The render model (`oscilloscope-core/src/render.rs`).** Unit tests for the loop-pass multiplier/lower-bound on "effects planned", and the stderr tail's own reading order; `oscilloscope-core/src/model.rs` and `plan.rs` cover an unnamed `if`/`else`'s own two branches (neither pollutes the other's earlier-siblings, and an untaken branch resolves once the taken one is observed, rather than blocking every chain sibling after it forever).
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

