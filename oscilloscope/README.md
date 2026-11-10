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
whole run -- shown in the header, `--glance`, and the Tape.

- **Where:** `$XDG_STATE_HOME/circuitry/osi/history/` --
  `~/.local/state/circuitry/osi/history/` by default, on macOS as well as
  Linux. One JSONL file per document, named by the SHA-256 of its own
  absolute path. `OSI_HISTORY_DIR=<dir>` puts it somewhere else.
- **What's in a record:** the document's path and content hash, the
  config path as given on the command line (if any) and the run's own
  run directory, the engine, when it started and ended, how it ended, its
  wall time, tokens and cost, and the same figures per step, keyed by its
  own path (a loop's passes written `iter_*`). A step a plan doesn't know
  by name (a generated `dynamic` child, a `use: inline:` child's own
  made-up names) is never recorded by name -- it's folded into one
  `(other)` entry. The config path and run directory are both optional
  (a record written before they existed reads neither). **Never
  recorded:** values, prompts, rendered params or commands, tool
  targets, error text, stdout/stderr, report messages or data, model
  replies, transcript content, the environment, or a config's own
  contents.
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

## The run registry

While a run (or an `osi watch`) is going, `osi` keeps one small file for it
so another `osi` process -- or a script -- can find it.

- **Where:** `$XDG_RUNTIME_DIR/circuitry/osi/runs/`, else
  `$XDG_STATE_HOME/circuitry/osi/runs/`, else
  `~/.local/state/circuitry/osi/runs/` (macOS has no `XDG_RUNTIME_DIR`, so
  this is the common case there). `OSI_RUNS_DIR=<dir>` puts it somewhere
  else. One `0600` JSON file per run, in a `0700` directory, named by the
  run's own `run_id`.
- **What's in an entry:** the document and config paths (both absolute),
  the engine, the engine's own pid, the launcher (`"osi <version>"` or
  `"osi <version> watch"`), the run directory, the events and live-state
  paths, and when the run started.
- Written once the run has actually started (its own `run_start`), not at
  the moment `osi` itself launches it; removed again when the run ends,
  when `osi` detaches from it, or when `osi` itself exits, including on a
  signal. An entry whose pid is no longer a live `cof`/`electricity`
  process is pruned (deleted) the next time anything reads the registry.
- **Bare `osi`, no document:** without a TTY, or with `--log`, `osi`
  finds the orchestrations already running on this machine -- the
  registry, a `ps` scan for `cof run`/`electricity` processes, and any
  `state.live.json` in the current directory or one level of
  subdirectories -- and prints one plain line per run (document, config,
  engine, pid, run directory), then exits 0. On a TTY with no `--log`,
  `osi` with no document is still a usage error, for now.

## The terminal UI

On a TTY, `osi <doc>` and `osi watch <dir>` show the interactive terminal UI
(DESIGN.md §6.3) instead of the plain-text stream. `--log`, or any non-TTY
stdout (a pipe, a file, `CI` set), keeps the plain stream unchanged.

osi has four screens, switched with `0`-`3`: the Tape (`0`, the
default for both `osi <doc>` and `osi watch`), Signal (`1`, the
token trace, below), Flow (`2`, the rail as control flow, below) and
cost (`3`, below). Selection, follow, folding, filters and collapsed
rows are shared across every screen but the Signal's and the Flow's
own, which draw their own self-contained header, status line, key
bar and Totals row instead (below).

Every screen shares one header band, status line, key bar and Totals
row: `osi <document> · <status> <elapsed>` on the left, the `osi`
word itself animating in place of a separate icon while the run is
live (a static word with `--no-animation`), the screen's own chip
(`TAPE`/`SIGNAL`/`FLOW`/`COST`) and the clock on the right; below the
screen's own panel, a status line (the run's own latest report once
one has arrived, else the newest finished step); a key bar naming
that screen's own keys; and a Totals row (tokens sent/received and
the total, the run's own deviation from its usual tokens once
history has one, effects done of planned, the ETA and the run's own
cost). `osi watch` names the document from the run's own
`runtime.last_run.orchestration_path`.

A `report` effect's own events show up in several places: once the
run's first report arrives, the header shows its message (a warning
styled in the theme's own warning colour) in place of the run id,
and it gets its own line in `--log` and (on the Tape and the Flow)
the story. `--theme <name>` (or `OSI_THEME`) paints the whole screen
in one of ten named colour themes (`dracula`, `nord`, `solarized-
dark`/`-light`, `gruvbox-dark`/`-light`, `catppuccin-mocha`/`-latte`,
`tokyo-night`, `one-dark`) instead of the terminal's own default
colours; `T` cycles through them live. `--no-animation` turns the
scope icon's own trace off, on both `osi <doc>` and `osi watch`.

### The Tape (`0`)

osi's own design document (`oscilloscope/design/tui-design.md`) is the
intent this screen implements; this section is what has actually
shipped of it.

One time axis: finished rows in the order they finished, each
prompt followed by its own faint preview line when previews are on
(`p`); the Now band, holding its place as the list grows; the rows
currently running, bold, with how long they've run; a slots rule while
a loop is dispatching passes concurrently; the rows still ahead, faint,
with what they'll do and (once there are three or more finished runs of
the same document) `usually N t` from history; Run End. Every row's
token column is a fixed-scale bar (`▰ = 50 t`, or finer once a run's
own usual steps are small enough); `t` swaps it for each row's own
deviation from its usual figure instead. The detail pane shows the
selected row's status, type, tokens, its usual tokens and how this run
compares, its share of the run's tokens so far, and the prompt sent and
the reply in full; `enter` opens the same content full-screen. A
Totals row (tokens, the run's own deviation from usual, effects done,
ETA, cost) is pinned above the list's own bottom border.

A running prompt's own row reads its own live figures while a
streaming adapter answers, elapsed time and (on a retry) attempt
first: `0.6s · ↓≈41 t · 68 t/s` (the received count marked `≈` until
the provider reports an exact one, the rate once there's been enough
of a second delta to measure one), `1.2s · waiting for the first
token` before the first delta, `1.0s · attempt 2 · waiting for the
first token` once a retry is under way; a non-streaming adapter keeps
only the elapsed time and attempt. The token bar and the Totals row draw this running estimate in
the faint style and add it to the exact total, settling to the normal
style the moment the prompt finishes. The detail pane's own Live
block shows First Token, Received, Sent and Attempt for a running
prompt (`no live figures from this adapter` for a non-streaming one)
in place of the Tokens line, which has nothing exact to show yet.
`osi watch` on `--events` alone drives the same figures from the
latest `progress` line, with no live state to read at all.

At 100 columns and up the detail pane shows beside the list, and
preview rows are on by default (`p` toggles; off at 24–29 rows, to
leave more height for the Now band, on again at 30 and up). At
80–99 columns the detail pane drops, previews are always off, and
`enter` opens the step's details over the list instead. At 60–79 columns the list drops its
clock column too, keeping only the glyph, the name and a fixed-width
token column, and the title, the status line and the Totals row all
shorten to their own bare figures. Below 60×24, a plain too-small
screen names the minimum each dimension needs against what the
terminal has, and points at `osi --log`.

`↑↓`/`j`/`k`, `PageUp`/`PageDown`, `Home`/`End` and `g`/`G` move over
the Tape's own rows in the order it draws them (finish order, then
what's running, then what's ahead), not the plan's own tree order,
and turn follow off unless the move lands the selection right back
on the row follow would have picked itself -- `f` toggles follow by
hand. `T` cycles the colour theme; `?` opens Help, listing exactly
this screen's own keys. `Tab`, `z`, `v`, `←`/`→`, `s` and `l` are not
Tape keys at all -- they still work on the Cost screen below, which
keeps its own panes until its own part of this redesign; the Signal
and the Flow have their own keys entirely (above, below). Below
60×24, every screen -- not only the Tape -- shows the too-small
notice instead.

### `--glance`

`osi <doc> --glance` and `osi watch <dir> --glance` show only the
header line, without the tabs, rewritten in place about once a second;
the final line stays on screen once the run ends. Signals and exit
codes work exactly as `--log`'s own do, and the two flags can't be
combined. `osi watch <dir> --glance --once` prints the current line
once and exits 0, for a tmux status line that runs its own command on
a timer instead of leaving one process attached. When stdout isn't a
terminal, only that one final line is ever printed.

### The Signal (`1`)

The trace is one column per 100 ms since the run's own start: tokens
sent to the model rise above a fixed axis, tokens received fall
below it, both in the run's own fixed unit (`600 t`/`80 t` by
default, or derived from history's own usual largest step the same
way the Tape's own bar unit is — 1.5x it, rounded up to the nearest
of a doubling series, never sliding once the run has started). A
`✓` on the axis marks a finished prompt, a `◆` a decision; the Now
sweep (`┃`) is bold, the axis ahead of it dashed. Seconds marks run
along the bottom; the Now sweep's own label replaces the marks it
covers, followed by `Ahead · plan order · usually ends <clock>` once
there's a whole-run usual to name. Step rows under the axis show
each top-level entry that has started, at its own column: a
finished prompt's tokens and deviation and the first words of its
reply, a tool as a bar across its own duration, a decision with the
steps its taken branch released, a loop as a bar to Now with how
many passes are running and queued — the plan still ahead trails the
last such row, faint.

The Now pane lists the currently running leaves with their own slot,
a pass waiting for one, the active loop's own progress and per-pass
tokens, the last burst (how long ago, its tokens, how many prompts)
and the next prompt in plan order with its usual tokens. Readings
shows the run's own sent, received, their ratio, the largest column
(tokens and clock), the single largest finished prompt, and how long
since the last burst.

`←`/`→` scrub a cursor along the axis, drawn distinctly from the Now
sweep, turning follow off; `n` returns it to Now, and moves which
column Readings' own Peak, Largest and Quiet read up to. `+`/`-`
zoom the column width from 50 ms to 1 s (100 ms the default), and
return a scrubbed cursor to Now. `⏎` opens the step under the
cursor full-screen, in a plainer view than the Tape's own `⏎` --
Status and, for a prompt, its own Reply in full, nothing else -- and
only while there's a step under the cursor to show at all. A run
longer than the pane is wide scrolls to keep the cursor (Now, while
following it) on screen.

At 120 columns and 30 rows and up, the trace sits above the Now pane
and Readings side by side; 80–119 columns, or rows 24–29 even at full
width, Readings drops and the Now pane keeps only the running rows,
with a shortened title; below 80 columns the Signal isn't offered,
bouncing back to the Tape on its own if the terminal shrinks below
it while Signal is on screen.

### Screen 2 (`flow`): the rail, as control flow

The rail (issue #691, design v1 part 3) runs left to right instead of
top to bottom: a station per step, plan order, forking into lanes at a
decision or a loop and rejoining. Each station is three lines (name,
headline, detail) -- a tool's own command and duration, a prompt's own
tokens and deviation from its own usual. A decision forks into two
lanes, the taken branch and the untaken one's own ghost (`· <else-
name>`/`not taken`, one line each, no row per untaken step) -- a named
`if`'s own branch comes straight from `meta.branch`; an unnamed one
writes no state node of its own to read it from, so its own decision
is inferred instead, the same rule as before (once either branch's own
step has appeared, that branch is taken; with no `else`, the other
reads as never going to run once the enclosing step has finished or a
later step in the same chain has started; `if → ?` rather than
guessing when both branches would read as the very same step names) --
selecting it (there being no row of its own to select) shows the same
condition and branch a named decision's own Selected pane does, plus
which side's own steps actually started, or that neither one's did. A
loop forks into one lane per *pass*, never per concurrency slot: the
passes actually running or done first, then one ahead lane per further
pass the loop's own count promises, each reading its own position's
history for a label standing in for tokens it doesn't have (`cats`), a
usual token figure and its own queued tool's command, both still
marked queued. A parallel `dynamic` forks into one lane per named
child the same way; a sequential one (`use`, a chain-flow `dynamic`, a
`try` body with its own `finally`) inlines its own children as plain
stations in plan order instead, no fork of their own. Below 100
columns, or a loop four or more lanes wide, those lanes fold into one
double-rail band (`══ 4 lanes + ══`, reading `● 2 run · 1 wait`); `+`/`-`
open or fold the *selected* fork's own lanes three at a time, never
every loop on the rail at once; an open fork never shows more than
three lanes at a time either way, the window sliding to keep whichever
one is selected in view.

The Selected pane (bottom left, or bottom right at 100 columns and up)
shows whichever pass, station or decision is selected: a pass as
Status (its own currently active step, elapsed time and slot), one row
per named step it ran with tokens and deviation, and Produced (the
last such step's own output); a plain station the same asked/doing/
produced shape the Tape's own full-screen step view already has,
including a running agent's own live transcript tail -- a station
inlined from a sequential container keeps its own ten-cell name on the
rail, but the Selected pane's own title still says which container
it's inside (`gate › reply`). The Story pane (bottom right, 100
columns and 30 rows and up only) is the run's own notable events in
order -- run start, each decision (inferred ones included), each loop
starting (`⟳ drafts · 3 passes · 2 wide`, an unbounded loop's own width
reading its pass count instead; synthesized here since `--log` writes
no event for a loop merely starting), each report, each failure and
run end.

Sizes:

1. 100×30 and up: the rail above the Selected and Story panes, side by
   side, roughly at the golden section;
2. 80×24 up to that: every loop's own lanes folded, and the Story pane
   dropped -- the Selected pane takes the whole lower band;
3. below 80×24: the Flow isn't offered, same floor as the Signal's.

Follow is on when the TUI starts: the rail scrolls to keep Now's own
arrow on screen as the run moves, and `←`/`→` turn it off, scrolling
manually -- only the rail itself, once redrawn, knows its own real
width well enough to turn follow back on again, the moment a manual
scroll lands Now back in view on its own. `↑`/`↓`/`j`/`k` move which
pass is selected, turning follow off the same way, and back on the
moment the selection lands back on the pass the Selected pane would
otherwise default to; `PageUp`/`PageDown`/`Home`/`End`/`g`/`G` are
no-ops here, there being no tree left to page or jump through. `⏎`
opens the current selection full-screen, the same toggle the Tape's
and Signal's own `⏎` already share, armed only while something is
actually selected. `f` follows the running station; `0`-`3` switch
screens; `?` opens Flow's own Help, listing exactly its own keys and
the globals every key bar shares.

| Key | Action |
|---|---|
| `←` / `→` | scroll the rail, turning follow off |
| `↑` / `↓` / `j` / `k` | select a pass |
| `+` / `-` | open / fold the selected fork's own lanes, three at a time |
| `⏎` | open the selection full-screen, and back |
| `f` | follow the running station (on by default) |
| `0`-`3` | switch screens (Tape, Signal, Flow, Cost) |
| `?` | help |
| `c` | cancel, with a confirm (the same as a confirmed `q`) |
| `q` | quit — asks first if a run is going; with nothing running, quits at once; in `osi watch` on a still-going run, detaches at once with a distinct ending (`■ detached (the run is still going)`, exit 0) |
| Ctrl-C | checked before any dialog, never merely dismissing one: cancels at once with no confirm while something's running, quits at once with nothing running, and in `osi watch` always just detaches |

The token rule is the same everywhere a count shows up (the header, the
rail, the Selected pane and the Cost screen's treemap and table): a
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

The engine exiting does not, on its own, end the TUI: the header, every
station's final status and the Selected pane stay live on whatever the run
ended with until the user leaves with a confirmed `q`, a Ctrl-C cancel, or
an external `INT`/`TERM`/`HUP` to osi itself (a `SIGHUP` never waits for a
key — the terminal is gone by definition). Short of one of those three, a
run that finishes on its own stays up for `q`/Ctrl-C to leave.

A `TestBackend` capture (`insta`, `crates/oscilloscope/src/tui.rs`'s own
golden tests, `design/frames/flow--normal--120x30.mock`'s cell-exact twin)
of `design/fixtures/run2`, replayed to 3.62 s after its own start: a
named decision taken (`gate`) with its untaken branch's own ghost, a
running loop folded into three observed lanes plus one ahead lane
reading its own slot's history, the pass pane and the story:

```
 osi  demo.yml · ● Running 3.6s                                                                        FLOW    12:50:50 
╭─ Flow · 6/14 · ▰ = 50 t · Following ─────────────────────────────────────────────────────────────────────────────────╮
│                                                                                               ▾ Now 12:50:50 · 3.6s  │
│■ Start   ── ✓ kind    ── ✓ tests   ── ◆ gate    ─┬─ ✓ reply   ─┴── ⟳ drafts  ─┬─ ✓ draft   ── ● pause   ─┴── · flaky │
│12:50:47     215 t +6%    3.0s         → then     │  389 t +47% │   0/3 passes │  cats 130 t   0.6s       │   tool · l│
│cof 0.1.0    ▰▰▰▰▱▱▱▱     sleep 3      kind!=""   │  ▰▰▰▰▰▰▰▰   │   261 t      │  ▰▰▰▱▱▱▱▱     sleep 1    │   usual — │
│                                                  └─ · reply   ─┘              ├─ ✓ draft   ── ● pause   ─┤           │
│                                                     not taken                 │  dogs 131 t   0.6s       │           │
│                                                                               │  ▰▰▰▱▱▱▱▱     sleep 1    │           │
│                                                                               └─ · draft   ── · pause   ─┘           │
│                                                                                  birds        sleep 1                │
│                                                                                  usual 155    queued                 │
│                                                                                                                      │
│                                                                                                                      │
│                                                                                                                      │
│ ◂ Start   ▸ flaky · summary · End                                                                                    │
│ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━────────────────────── │
│──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────│
│ Totals ↑796 ↓69 · 865 t  +12% Usual  6/14 Effects  ETA 12:50:52  Cost —                                              │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
╭─ Pass 1 · cats · Selected ─────────────────────────────────╮╭─ Story · Decisions and Failures ───────────────────────╮
│ Status   ● Running  pause · 0.6s · slot 1                  ││ 12:50:47 ■ run started · d2c8267e                      │
│ Draft    ✓ Done  ↑118 ↓12 · 130 t  -16% vs usual 154 t     ││ 12:50:50 ◆ gate → then                                 │
│ Produced Cats nap in sunbeams and judge you quietly.       ││ 12:50:50 ⟳ drafts · 3 passes · 2 wide                  │
│                                                            ││                                                        │
│                                                            ││                                                        │
│                                                            ││                                                        │
╰────────────────────────────────────────────────────────────╯╰────────────────────────────────────────────────────────╯
 ✓ gate › reply finished · ↑348 ↓41 · 0.6s ago
 ←→ Scroll  ↑↓ Lane  +/- Lanes  f Follow  enter Open  0-3 Screens                                        ? Help  q Quit 
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
⠔⠒⠒⠤⠤⠔⠒⠒ do-thing.yml · ♥ running 5m00s · 00:05:00   0 tape  2 flow  [3 cost]                                       
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
