# oscilloscope

`osi` (oscilloscope) launches a Circuitry orchestration on either engine —
`cof` (this repository's Python reference implementation) or `electricity`
(the Rust runner in `../electricity`) — and shows it running: every effect's
status against the document's plan, plus a readable log of what is
processing. A terminal UI comes first; a GUI can reuse the same core later.

```
CYBERDINER_TOKEN=... osi do-thing.yml config.json
```

**This is a preview.** Milestone O-1 (issue #424) filled in the core: `osi
<orchestration>` launches and supervises `cof` or `electricity` and exits
with the engine's own exit code; `osi watch <dir>` attaches to a run `osi`
didn't start. Milestone O-2 (issue #434) adds the terminal UI below.
electricity's own run path, `--events` and `--live-state` give
`--engine electricity` the same live view `cof` does for the content it
runs — see [`electricity/README.md`](../electricity/README.md)'s preview
section for exactly what runs today. Anything it doesn't support yet is
refused up front, with a message naming the preview marker, before any
state is written. See [`DESIGN.md`](DESIGN.md) §4.2.

```
osi do-thing.yml config.json
osi do-thing.yml -e key=value --engine cof --out-dir ./run
osi watch ./run
osi watch ./run --plan do-thing.yml --config config.json -e key=value
```

- With no `config.json`, `cof` resolves its own config layers (global,
  project, environment) exactly as `cof run` would. The plan osi compiles
  from the document uses the same `config.json`/`-e` pair passed on the
  command line (`electricity::check_options`, the exact options a real
  `electricity <config> <doc> -e k=v...` run would check it against). With
  no `config.json` given here either, osi now mirrors `cof`'s own
  discovery for the plan too: `CIRCUITRY_CONFIG`, if set, replaces
  discovery entirely (the same precedence `cof run` itself uses);
  otherwise the global config (`~/.config/circuitry/config.json`) and a
  *trusted* project `circuitry.config.json`/`config.json` found in osi's
  own working directory are deep-merged the same way `cof run` layers
  them (`cof trust` still governs whether a discovered project config
  applies at all). A document whose plan depends on a
  `group:`/`concurrency_groups` defined only there no longer falls back
  to no plan. `CIRCUITRY_*` per-setting environment overlays
  (`CIRCUITRY_MODEL`, `CIRCUITRY_ADAPTER`, ...) still apply on top once
  there's a file for them to layer onto, the same as `cof run`. An
  explicit `config.json` given here already wins over all of the above,
  same as `cof run` itself. `osi watch --plan do-thing.yml` takes its own
  `--config`/`-e` for the same reason, independent of whatever the
  watched run itself was started with, and gets the same discovery with
  neither.
- `--engine electricity` needs a config (electricity has no config
  discovery yet). For a document outside electricity's current scope, it
  shows electricity's own preview-marker refusal and exit code, the same
  way osi shows any other pre-execution failure.
- Run files (`state.live.json`, `state.json`, `events.jsonl`, `stdout.txt`,
  `stderr.txt`) go to a fresh temporary directory by default, printed when
  the run ends; `--out-dir DIR` keeps them in a chosen directory instead —
  the same layout `osi watch` reads.
- `cof run --events` (issue #419) is detected from `cof run --help`, and
  electricity's own `--events`/`--live-state` (issue #431) the same way from
  `electricity --help`; without them `osi` falls back to `--live-state`/
  `--out` alone, state-only (DESIGN.md §2's "From state alone" rules).
- `osi watch <dir>` on a run with no `--events` stream has no engine pid to
  check and no way to tell a genuinely aborted run (a second signal,
  `SIGKILL`, a crash) from one that's simply still going quietly: neither
  ever writes a final snapshot. `osi watch` warns once on stderr when this
  is the case, and otherwise just keeps waiting — there is no staleness
  timeout, since a quiet run can stay quiet for a long time. Ctrl-C always
  stops the watch.
- A document that fails to compile (or an engine whose compiler is still a
  stub) falls back to a plan-free run: rows come only from what's observed.
- `osi watch <dir>` without `--plan` detects an `if`/loop container from
  the freshest state it has for that path (`meta.mode` for an `if`,
  `meta.flow` for a dynamic) before printing an event line for it, even
  within the very same poll tick its own state first appears in -- the
  case that matters when `watch` attaches to a run already under way or
  already finished. A container whose own `start` event is tailed in a
  *live* run before its state has been written even once yet (no entry
  for that path at all, same tick or earlier) still can't be told from a
  leaf, so it can briefly print its own `▶`/`✓` the way a live `osi <doc>`
  run of the same document never does (it already has a compiled plan
  from the start). Pass `--plan do-thing.yml` to `osi watch` for the same
  output a live run gives.
- The first Ctrl-C forwards `SIGINT` to the engine's process group and logs
  a cancelling notice; a second forwards it again; still running 10s later,
  `osi` sends `SIGKILL`. `SIGTERM`/`SIGHUP` are forwarded the same way. `osi`
  always exits with the engine's own exit code, and never leaves it running
  after `osi` itself exits cleanly — including on a panic. The one exception
  is `osi` itself being `SIGKILL`ed: nothing can run its own cleanup code
  after that, so the engine (in its own process group) keeps running.

## Run history

`osi` keeps a short record of each finished run, per document: how long
each step took, its tokens and cost, and how the run ended. From these
records it works out what is usual for each step (p50/p90) and, once
there are enough runs of the same version of a document, an ETA for the
whole run -- shown in the header, `--glance`, and the overview.

- **Where:** `$XDG_STATE_HOME/circuitry/osi/history/` --
  `~/.local/state/circuitry/osi/history/` by default, on macOS as well as
  Linux. One JSONL file per document, named by the SHA-256 of its own
  absolute path. `OSI_HISTORY_DIR=<dir>` puts it somewhere else.
- **What's in a record:** the document's path and content hash, the
  engine, when it started and ended, how it ended, its wall time, tokens
  and cost, and the same figures per step, keyed by its own path (a
  loop's passes written `iter_*`). A step a plan doesn't know by name (a
  generated `dynamic` child, a `use: inline:` child's own made-up names)
  is never recorded by name -- it's folded into one `(other)` entry.
  **Never recorded:** values, prompts, rendered params or commands, tool
  targets, error text, stdout/stderr, report messages or data, model
  replies, transcript content, the environment, or the config.
- **Limits:** at most 50 runs per document (the file is compacted once it
  reaches 100 lines) and 500 steps per run; at most 500 documents and 64
  MiB in total, oldest-written evicted first.
- **Turning it off:** `--no-history` (on `osi <doc>` and `osi watch`) or
  `OSI_HISTORY=off` turns history off completely -- nothing is read or
  written, and no directory is created.
- **Deleting it:** `rm -rf ~/.local/state/circuitry/osi/history` (or
  wherever `OSI_HISTORY_DIR` points) removes every document's history.
  There is no `osi history` command.
- `osi watch` keys history by the watched run's own absolute
  `runtime.last_run.orchestration_path`, else by `--plan`'s own document,
  else history is off for that watch (one line on stderr says so). It
  writes only once the watched run has really ended -- detaching (`q`,
  Ctrl-C) from one still running writes nothing, and
  `osi watch --glance --once` only ever reads.

## The terminal UI

On a TTY, `osi <doc>` and `osi watch <dir>` show the interactive terminal UI
(DESIGN.md §6.3) instead of the plain-text stream. `--log`, or any non-TTY
stdout (a pipe, a file, `CI` set), keeps the plain stream unchanged.

osi has four screens, switched with `0`-`3`: the overview (`0`, the
default for both `osi <doc>` and `osi watch`), time (`1`, a clock-axis
timeline, below), flow (`2`, the tree as control flow, below) and cost
(`3`, below). Selection, follow, folding, filters and collapsed rows
are shared across every screen.

Every screen shares one header line: the scope icon's own animated
trace (a static `◉` with `--no-animation`), the document, `♥` while
the engine process is alive, the run's status and elapsed time, the
current clock time, the innermost running loop's ETA, the latest report
(#610, once one has arrived), and the screen tabs. When
it's short of room: the tabs shrink to their own numbers first, then the
report truncates (down to a bare `▸`), then the document (keeping its
own tail), then the ETA goes — status, elapsed and the clock never
truncate. `osi watch` names the document from the run's own
`runtime.last_run.orchestration_path`.

### The overview (`0`)

A phase strip (the document's own top-level steps, with a single-child
wrapper folded into one breadcrumb, each with its own status glyph,
start time and duration); a now pane (the run's latest log lines next
to whichever step follow selects — its breadcrumb, kind, provider or
model, since/elapsed, and, for a prompt, its token estimate and a
prompt snippet, for a tool its rendered command, for a running agent
session its own live summary (engine, model, turn and tool-call
counts, tokens sent/received and cost — a figure the CLI doesn't
report live reads `—`, never `0` — context size against its window
when known, files/lines changed, the last test command's own outcome
worked out from the transcript, and a glyph strip plus a clock-timed
line per recent tool call, both from the same transcript, tailed only
while this step is on screen; with no transcript tailed yet, the same
activity reads from `meta.live`'s own tool names and counts instead);
the last finished step with nothing running (a finished agent reads
the same way any other finished tool step does),
or the run's own outcome, start-end and totals once it has ended); and
three cards —
time (start/end, elapsed, ETA, a bar of the top-level steps' own
durations, the slowest finished one), flow (effects done out of
planned, failures, the latest decisions or reports, loop progress, the
next pending step, quick steps) and cost (tokens in/out, cost once any
effect reports one, the store with the most tokens -- the same store
the Cost screen's own table ranks first, what's in flight). The
effects count that used to be in the header lives in the flow card
now; the Flow screen's own flow pane title keeps it too.

### `--glance`

`osi <doc> --glance` and `osi watch <dir> --glance` show only the
header line, without the tabs, rewritten in place about once a second;
the final line stays on screen once the run ends. Signals and exit
codes work exactly as `--log`'s own do, and the two flags can't be
combined. `osi watch <dir> --glance --once` prints the current line
once and exits 0, for a tmux status line that runs its own command on
a timer instead of leaving one process attached. When stdout isn't a
terminal, only that one final line is ever printed.

### Screen 1 (`time`): a clock-axis timeline

The timeline (top) is one row per folded step (the same breadcrumbs,
`pass N of M` and quick-steps folding the overview and Flow use) with a
bar on a shared clock axis, start and duration columns on the right. The
axis runs from the run's own start to the later of now and the usual
(p90) end while the run is going, plus a small margin, or start to end
once it has finished; labelled ticks (1, 2, 5, 10, 15 or 30 minutes, or a
whole number of hours) stay about 10 columns apart. A `┃` marks the
current time through every row and strip, gone once the run has ended. A
running bar ends in a `▶` head; a failed one ends in `✗`. A pending step
with enough history gets a ghost bar (`░`), its own usual start and end
shifted by how far ahead or behind the run is right now — gone without
enough runs of this document version, and after an edit, since both need
whole-run figures. The last row, `planned`, is a ghost from now to the
ETA. A loop collapses to one row by default (`⟲ 3/8`, one bar from its
first pass's start to its last pass's end); `⏎` expands it into one row
per pass — an unnamed loop's own passes (which share one path) need
`--events` to tell them apart at all, numbered `#1`, `#2`, …, or the row
stays one.

Below the timeline, three strips on the same axis: **events/min** (start
and end events per minute, as bar heights; without `--events`, the same
`created_at`/`completed_at` transitions stand in, and the strip reads
`(from state)`); **decisions** (a `◆` at the time each named `if`
decided); and **in flight** (how many steps are running at once, ending
in the run's own peak, and `N queued` when a prompt is waiting on a
concurrency slot).

The zoom pane (bottom left) is the selected step's own timeline: a
container's own children as rows; an agent's own tool calls as one band
per kind (read, edit, bash, other) from its transcript, with a `think`
band covering the time between calls, and a footer (`turn 14 · 31 tools ·
bash 4m10s (67%)`) from its live summary while running or `meta.raw`
once finished; a prompt or tool's own bar, with its usual (p50/p90)
marked when history has it.

"vs usual" (bottom right) lists each timed row's own `now`, `p50`, and
a mark — `▲` slower, `▼` faster, `≈` usual (outside p10–p90 with 5 or
more runs, more than 20% from p50 with 3 or 4) — with a footer
(`finish ~12:05 · usual 11:58–12:09 · from 7 runs`). After an edit, the
title reads `vs usual · earlier versions` instead, with no finish time;
with fewer than 3 runs, the pane reads `history: 1 of 3 runs of this
version` instead of any rows.

`+`/`-` zoom the axis in and out, around now while follow is on or
around the selection otherwise; `=` fits the whole run again. `tab`
cycles timeline → zoom → vs usual → timeline; the zoom and vs usual
panes scroll when focused. `↑↓`/`jk`/`PgUp`/`PgDn`/`Home`/`End`/`g`/`G`
select rows, shared with every other screen. At 100×30 and larger, the
timeline with its strips sits above the zoom pane and the vs usual pane,
side by side (62/38); 80×20 up to that, the timeline and strips alone,
with the start/duration columns additionally dropped below 90 columns;
below 80×20, a plain list (status, step, start, duration), no axis.

### Screen 2 (`flow`): the tree as control flow

The flow pane (left) is today's orchestration pane, plus each `if`'s own
decision: a named `if` gets a row of its own, `◆ <name> → then`/`→ else`
(`◆ <name> …` before the condition has been evaluated), with the untaken
branch as one dim line underneath (`· else: <its first step>`, `(+N more)`,
or `· else: nothing` when it's empty) — never a row per untaken step. An
unnamed `if` writes no node and fires no event, so its own row
(`◆ if <condition, cut short> → then`) is inferred instead: once any step
of either branch has appeared, that branch is read as taken; with no
`else`, it reads `◆ if … → else (nothing ran)` once the enclosing step
has finished, or once a later step in the same chain has started, with
nothing from either branch having started; until either happens, it
reads `◆ if …`. When both branches would read as the same step names, it
reads `◆ if → ?` instead of guessing. At 100 columns and wider, a
decision's own condition (the CEL expression, or a model-mode template's
first line, cut with `…`) is a dim second line under it; narrower, it's
in the selected pane only. A named loop with a known total of exactly
one pass, or an unsettled (`dynamic`) total once the loop itself has
finished with only one pass observed, merges into one row, `⟲ <name>
pass N of M`; more than one pass keeps one row each, as before. Two or more consecutive, finished, sub-second tool steps still
fold into one `N quick steps` row (`⏎` expands it in place), and the
pane's own footer totals every quick step in the run
(`quick steps folded: 9 · 2.1s`) and gives a health line
(`health: 0 failed · 0 retries · 0 warnings`). Columns (start, duration,
tokens, cost) sit to the right of each row; a narrow pane
drops them in this order — cost, start, tokens — duration always stays.

The selected pane (top right) shows the flow pane's own selection: a
prompt's text, a tool's rendered command, or an agent's own first line
of prompt text (the file's own path, for a prompt read from one) under
"asked", what it's doing right now (a running prompt's token estimate, a
running tool's elapsed time, or a running agent's own live summary — the
same one the overview's now pane shows, including its transcript's last
few tool calls and the last test command's own outcome, tailed only while
this step is selected), and, once it finishes, its
value or reply, tokens, cost and any error under "produced" (a running step
reads `(when done)`; a finished agent step's own "produced" additionally
shows `meta.raw`'s own transcript path, files changed and turns). A
decision row shows its own condition, mode, the
branch it took and, for a model-mode condition, the model's own answer. `s`
swaps this pane for today's state tree instead, so a named report
(#605/#610) stays selectable; `s` again swaps back. `v` opens the full
value or prompt, as ever.

The story pane (bottom right) is the run's own notable events, oldest
first, with clock times: run start, each decision, each report, each
failure, and run end with its totals. It keeps at most 2,000 lines, and
follows the latest line while scrolled to the bottom. `l` swaps it for the
full log (the same lines `--log` prints, plus the engine's stderr) instead;
`l` again swaps back.

Sizes:

1. 100×30 and larger: flow pane on the left; the selected pane over the
   story pane on the right, both splits at the golden section (62/38);
2. 80×20 up to that: flow pane | selected pane, with the story pane
   hidden — `l` shows the log in the selected pane's own place instead;
3. below 80×20: one column, the selected pane over the flow pane, with no
   story pane and the flow pane's own columns dropped to duration alone.

A `report` effect's own events (`electricity/docs/spec/runtime-semantics.md` §8.7) show up in several places.
Once the run's first report arrives, the header shows its message
(a warning styled in the theme's own warning colour) after the clock
time and before the screen tabs, per the truncation order above. The
run id lives in the root row's own details, and in a `■ run started`
line in `--log` and the story. Every report, named or not, gets its
own log line (`▸ <message>`, `▸ warning: <message>`), with a compact
`data` preview after it when it has one, and joins the story the same way.
A *named* report still writes a shadow-state node (so it has a row in the
state pane, with its own `level`/message/clock time and `data` as pretty
JSON once selected there) but, named or not, a report never becomes a row
in the flow pane, and never changes how a loop pass folds.

Follow is on when the TUI starts: the flow pane's own selection tracks the
running effect, and the selected pane shows it with no key press. Any
manual move (`↑↓`/`j`/`k`, `PgUp`/`PgDn`, `Home`/`End`, `g`/`G`) turns follow
off; `f` turns it back on. The flow pane scrolls, keeping the selected row
in view, with a `12/48`-style position in its own border whenever a row is
hidden above or below; the selected and story panes scroll the same way
once they have focus (`tab` cycles flow → selected → story → flow).

Whatever just changed gets a soft background tint that fades out over about
a second, so the eye goes to it: a row's status, a state row, a new log
line or the token totals. The first observation never
highlights anything (so `osi watch` attaching to a finished run opens on a
calm screen), and neither does elapsed time, selection, scrolling, the
filter or a resize. `--theme <name>` (or `OSI_THEME`) paints the whole
screen in one of ten named colour themes (`dracula`, `nord`, `solarized-
dark`/`-light`, `gruvbox-dark`/`-light`, `catppuccin-mocha`/`-latte`,
`tokyo-night`, `one-dark`) instead of the terminal's own default colours;
`t` cycles through them live. `--no-animation` turns the highlight fade and
the scope icon's own trace off, both on `osi <doc>` and `osi watch`.

| Key | Action |
|---|---|
| `0` | the overview |
| `2` | flow (this screen) |
| `3` | cost (below); `1` (time) does nothing yet |
| `↑↓` / `j`/`k` | move (the focused pane's own selection, or the selected/story pane's scroll) |
| `PgUp`/`PgDn` | move a page |
| `Home`/`End`, `g`/`G` | jump to the first / last row |
| `←` / `→` | collapse / expand (flow or state pane) |
| `⏎` | expand a quick-steps row in place |
| `z` | fold on/off (breadcrumbs, quick steps); on by default |
| `tab` | flow → selected → story → flow |
| `s` | swap the selected pane for today's state tree, and back |
| `l` | swap the story pane for the full log, and back |
| `f` | follow the running row (on by default) |
| `e` | errors only |
| `/` | filter |
| `v` | show the full value; `v` again cycles between the prompt and the value when the row has both |
| `t` | cycle the colour theme |
| `c` | cancel, with a confirm (the same as a confirmed `q`) |
| `q` | quit — asks first if a run is going; with nothing running, quits at once; in `osi watch` on a still-going run, detaches at once with a distinct ending (`■ detached (the run is still going)`, exit 0) |
| Ctrl-C | checked before any dialog, never merely dismissing one: cancels at once with no confirm while something's running, quits at once with nothing running, and in `osi watch` always just detaches |
| `?` | help |

The token rule is the same everywhere a count shows up (the header, the
state pane, the selected pane and the Cost screen's treemap and table): a
node's own `tokens_sent_total`/`tokens_received_total` when present,
falling back to `tokens_sent`/`tokens_received` otherwise — the same pair
both `cof`'s and electricity's own `runtime.last_run.totals` are built
from, so a retried prompt can never make one surface disagree with
another. There is no live *output* count: both engines report a prompt's
tokens only once it finishes. Every token count is formatted the same one
way too (`369 t`, `18.0 kt`, `142 kt`, `2.1 Mt`), and so is every duration,
everywhere one shows up (`0.4s`, `6m12s`, `2h05m`, `3d04h`) —
`oscilloscope_core::format`'s one formatter each. Clock times (`HH:MM:SS`
in the log, the story, the header's own `since 11:04:58` while a run is
going and `11:04:58–11:31:10` once it has ended, and the innermost loop's
ETA as `ETA ~11:42`) go through the binary's own real local zone, resolved
per timestamp.

The flow pane folds a single-child container into one breadcrumb row
(`review › fix › agent`) and two or more consecutive, finished, sub-second
tool steps into one `N quick steps` row — `z` turns folding off to see the
full tree, and `⏎` expands a quick-steps row in place. A named `if`'s own
row never folds into a breadcrumb, so its `◆` stays visible even with a
single child. A loop pass with more than one step is labelled
`pass N of M` there (the state pane keeps the real `iter_N` key). A
running `agent` tool step counts in the overview's cost card's own
in-flight line next to running prompts (`2 prompts · 1 agent in flight`),
with no token estimate of its own. A step with a `meta.live` whose own
`updated_at` is within 5 seconds of now counts as running there too,
with no `--events` needed at all; an interrupted run's own leftover
`meta.live`, whose `updated_at` has stopped moving, reads as stopped
instead, in the now pane and the selected pane alike, with the
session's own last-known figures still shown underneath.

The engine exiting does not, on its own, end the TUI: the header, every
row's final status and the selected pane stay live on whatever the run
ended with until the user leaves with a confirmed `q`, a Ctrl-C cancel, or
an external `INT`/`TERM`/`HUP` to osi itself (a `SIGHUP` never waits for a
key — the terminal is gone by definition). Short of one of those three, a
run that finishes on its own stays up for `q`/Ctrl-C to leave.

A `TestBackend` capture (`insta`, `crates/oscilloscope/src/tui.rs`'s own
golden tests) at 100×30, a run just finished with a named decision taken,
its untaken branch's own dim summary line, an unnamed decision inferred,
a loop's single pass merged into one row, a quick-steps row folded with
its own footer total, and the story:

```
⠤⠤⠤⠤⠤⠤⠤⠤ do-thing.yml · ok 2.0s · 00:00:02   0 overview  [2 flow]  3 cost                           
┌ flow (do-thing · 8/8) ◆ decision ⟲ loop ▶ running ─────────┐┌ selected ──────────────────────────┐
│✓ prime   2.0s                                              ││chosen                              │
│  ✓ 2 quick steps   0.4s                                    ││tool · shell                        │
│  ◆ gate  → then   0.0s                                     ││produced:                           │
│    x > 1                                                   ││ok                                  │
│    · else: other                                           ││                                    │
│    ✓ chosen   0.3s                                         ││                                    │
│  ◆ if y > 1  → then  (inferred)                            ││                                    │
│    y > 1                                                   ││                                    │
│    · else: u_else                                          ││                                    │
│  ✓ u_then   0.2s                                           │└────────────────────────────────────┘
│  ⟲ fan  pass 1 of 1   0.4s                                 │┌ story ─────────────────────────────┐
│                                                            ││00:00:00 ■ run started  abc         │
│                                                            ││00:00:00 ◆ gate → then              │
│                                                            ││00:00:00 ◆ if y > 1 → then          │
│                                                            ││                                    │
│                                                            ││                                    │
│                                                            ││                                    │
│                                                            ││                                    │
│                                                            ││                                    │
│                                                            ││                                    │
│                                                            ││                                    │
│                                                            ││                                    │
│                                                            ││                                    │
│                                                            ││                                    │
│quick steps folded: 4 · 0.9s                                ││                                    │
│health: 0 failed · 0 retries · 0 warnings                   ││                                    │
│                                                            ││                                    │
└────────────────────────────────────────────────────────────┘└────────────────────────────────────┘
```

### Screen 3 (`cost`): where the tokens and money went

A treemap of the current run's state stores (one tile per store, area
proportional to its tokens), a stacked "tokens over time" chart (one band
per top-level step, on a clock axis from the run's start to now) and a
table of the largest stores, laid out at three sizes:

1. 100×30 and up: the treemap (62% of the height) above the chart and the
   table, side by side at 62/38;
2. 80×20 to just under that: the treemap above the table alone (no chart);
3. below 80×20: the table alone, where `⏎` still zooms it.

The treemap's default measure is total tokens (in + out); `i`/`o`/`$`
switch it to input tokens, output tokens or cost, and the same key again
returns to total. A single-child chain folds into one tile (`plan ›
agent`), by the exact same breadcrumb-eligibility rule the orchestration
pane's own breadcrumbs use (a loop, the document root and a quick-steps
summary never fold away, and neither side folds while its own status is
still pending). `↑`/`↓` select a tile in size order, `⏎` zooms into the
selected store's own children, and `esc` zooms back out; the zoomed
store's own name at each level (never its full dotted path) is shown
alongside the measure in the treemap's title. A store with no value for
the chosen measure is left out, and the title counts how many were
(`N stores without cost`); a store still running shows "tokens when
done" instead of a number, however small its placeholder weight is, and
one that never finished because the run itself ended first (aborted or
cancelled) reads "no tokens reported" instead, rather than its own
placeholder weight read back as if it were real. A `while: mode: model`
loop's own tokens (written onto the loop's own meta alongside its
passes) get a leaf of their own next to them, rather than being dropped.
A running `agent` tool step is the one exception to "tokens when done":
its own tile (and the largest-stores table's own row for it) sizes and
labels itself `live` from its own live `tokens_sent` under the
`tokens`/`input tokens` measures, growing as the session goes. A tile
with room for a second row also draws a per-turn strip (`turn_tokens`,
oldest first) and a cache bar (`tokens_cached_in`'s own share of
`tokens_sent`). `output tokens`/`cost` still read "tokens when done"
until the step finishes: pi does report `tokens_received`/`cost_usd`
live too, but sizing a tile from them is left for a later issue.
The treemap's own squarify layout accounts for the terminal's own tall
character cells (about 2:1), so a tile that is actually square in
character cells (and so visibly tall and narrow on screen) is never what
"the worst aspect ratio" means — a visually square tile is. The largest-
stores table always ranks by total tokens, regardless of the treemap's
own measure, filters to the zoomed store below the treemap (so `⏎` still
has a visible effect even where the treemap itself doesn't fit), and its
footer gives the run's own totals (`runtime.last_run.totals`) and any gap
between them and the table (a declared-outputs `use` child's effects, for
one, never reach the final state tree at all even though the run's own
totals counted them as they completed). `meta.cost_usd` shows wherever an
effect reports it; when only some finished stores do, the figure is
marked with its own coverage (`$4.20 (from 3 of 9 prompts)`) -- a store
still running is never counted either way.

| Key | Action |
|---|---|
| `↑↓` / `j`/`k` | select a store, in size order |
| `⏎` | zoom into the selected store |
| `esc` | zoom back out |
| `i` | switch the treemap's measure to input tokens (again: total) |
| `o` | switch the treemap's measure to output tokens (again: total) |
| `$` | switch the treemap's measure to cost (again: total) |

A `TestBackend` capture at 120×40 (blank rows trimmed here; the full
capture is `crates/oscilloscope/src/snapshots/osi__tui__golden_tests__
cost_running_at_120x40.snap`):

```
⠔⠒⠒⠤⠤⠔⠒⠒ do-thing.yml · ♥ running 5m00s · 00:05:00   0 overview  2 flow  [3 cost]                                       
┌ tokens by store · tokens ────────────────────────────────────────────────────────────────────────────────────────────┐
│a 1.3 kt                                          ┌fan tokens when done─────────────────────────────┐b 450 t          │
│                                                  │iter_0 › nap tokens when done                    │                 │
│                                                  │                                                 │                 │
│                                                  └─────────────────────────────────────────────────┘                 │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
┌ tokens over time ──────────────────────────────────────────────────────┐┌ largest stores ────────────────────────────┐
│            ███                                                         ││a  ↑900 t ↓400 t  $1.20  74%                │
│            ███                                                         ││b  ↑300 t ↓150 t  26%                       │
│            ███                                                         ││fan › iter_0 › nap  tokens when done        │
│            ███                                                         ││                                            │
│            ███                                                         ││total ↑1.2 kt ↓550 t                        │
│            ███                                                         ││$1.20 (from 1 of 2 prompts)                 │
│            ███                                                         ││                                            │
│            ███            ▓▓▓                                          ││                                            │
│            ███            ▓▓▓                                          ││                                            │
│            ███            ▓▓▓                                          ││                                            │
│            ███            ▓▓▓                                          ││                                            │
│00:00:00                        00:02:30                        00:05:00││                                            │
│█ a ▓ b ▒ fan                                                           ││                                            │
└────────────────────────────────────────────────────────────────────────┘└────────────────────────────────────────────┘
```

Raw mode and the alternate screen are entered once and restored on every
exit path — a normal end, osi's own error, a panic (a panic hook), and a
forwarded `SIGINT`/`SIGTERM`/`SIGHUP`, forwarded to the engine as the same
signal it was — the same `SIGKILL` exception as the supervision section
below. Quitting while osi is running the engine cancels the run first (the
confirmed `q`/`c` dialog, or Ctrl-C): osi never leaves the engine running.

`oscilloscope/` is its own Cargo workspace, versioned in lockstep with the
rest of this repository (`pyproject.toml`, `electricity/Cargo.toml`). It
depends on `electricity`'s `electricity-compiler` and `electricity-bytecode`
crates by path, to compile a document into the plan tree it displays, on
either engine.

## Build and test

From the repository root:

```bash
cd oscilloscope
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

`cargo build --release -p oscilloscope` produces the `osi` binary at
`target/release/osi`.

## Layout

```
oscilloscope/
  Cargo.toml                 # workspace; path deps on ../electricity/crates/{electricity-compiler,electricity-bytecode}
  crates/
    oscilloscope-core/       # no terminal code, so a future GUI can reuse it (render.rs: the TUI's own render model, treemap.rs: the Cost screen's by-store treemap layout, cost.rs: its largest-stores table and tokens-over-time chart)
    oscilloscope/            # the `osi` binary: clap CLI, the plain stream, and the ratatui/crossterm TUI (tui.rs, keys.rs, terminal.rs)
```

## Read more

- [`DESIGN.md`](DESIGN.md) — the full design, moved from issue #418:
  measurements of what `cof`'s live state and a new `--events` stream show,
  the inference rules, engine launching, architecture and milestones.
