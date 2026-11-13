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
osi do-thing.yml --label "ticket 42"
osi watch ./run
osi watch ./run --plan do-thing.yml --config config.json -e key=value
```

`--label TEXT` (issue #752) passes literal text through to whichever engine
runs the document, overriding its own `label:` template; `osi watch` has no
`--label` of its own, since it never launches anything. Wherever osi shows a
run's own generated name (issue #717's `amber-falcon`) -- the header, Select,
Recent Runs, All's tile bands, the Combined Signal's legend and Per Run
table, `--log` and the exit summary -- a labelled run shows its label
instead, cleaned the same way both engines already clean it before writing
it (whitespace runs collapsed to one space, control characters and escape
sequences dropped, trimmed and cut to 64 characters), since a file osi reads
back can be written by anything. Two runs on the same screen sharing a
label each add their own generated name (`ticket 42 · amber-falcon`), the
same way two unlabelled runs already add their own run id's first four hex
digits; in a column (Select's NAME, the Combined Signal's Per Run, a tile's
own band) a label is cut to 32 display cells, never a byte or character
count, so CJK or emoji text is cut exactly where it stops fitting -- the
header alone shows a label in full, cut only once the header itself is too
narrow for it.

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
  subdirectories -- and prints one plain line per run (document, name,
  config, engine, pid, run directory), then exits 0. On a TTY with no `--log`, it
  opens the Select screen instead (below).
- **`OSI_PROCESS_SCAN=0`:** list only the registry, skipping the `ps`
  scan and the current-directory source entirely -- for tests and a
  shared machine where the scan would otherwise find orchestrations this
  `osi` never launched or watched.

## Select

Bare `osi`, on a TTY with no `--log` and no document, finds the
orchestrations already running the same way the plain-text list above
does, and shows one of three screens:

- **Exactly one running:** attaches to it straight away, read-only, the
  same as `osi watch <run_dir>` (`c` is inert; `esc` does nothing, since
  there is no Select screen behind this one to go back to).
- **More than one running:** a list, `All` first, then one row per run
  with the document and config basenames and the engine. The cursor
  starts on the first run. `enter` on a run row attaches to it, the same
  way; `esc` on that attached run returns here. `enter` on `All` opens
  the tiled grid (below).
- **None running:** where discovery looked, and a Recent Runs table built
  from run history across every document -- DOC, CONFIG (`—` for an
  older record with none), STARTED, TOOK, TOKENS and OUTCOME (`ok`, `ok ·
  N handled failure(s)`, `failed · <step>`, or `gone` once the record's
  own run directory existed but no longer does). An older record with no
  run directory recorded at all still shows its real outcome, just with
  nothing to open (`enter` is inert there, not `gone` -- that word is
  reserved for a directory that genuinely vanished); `enter` on any other
  row opens `osi watch <run_dir>` read-only. Either list's own window
  scrolls to keep the cursor in view once there are more rows than fit.

Keys: `↑↓`/`j k`/`Home End`/`g G` move the cursor (kept across every
attach and `esc` back, not reset to the first run each time), `enter`
attaches (or opens, on the empty screen), `T` cycles the colour theme
(issue #714: the same one value every other screen shares, surviving
an attach and `esc` back), `?` shows Select's own Help (which lists `c`
as inert here), `q` quits at once -- leaving Select, or an attached run,
only ever stops watching, never cancels anything, so there is nothing to
confirm. `0`-`3` and a plain `c` do nothing on Select itself (no run is
attached from here to switch a screen on or cancel); a
raw Ctrl-C quits with exit 130, same as a forwarded SIGINT would, and a
forwarded SIGTERM/SIGHUP quits with 143/129. Select needs at least 80
columns (every screen's own shared 60-column floor is higher here, since
80×24 is already its own narrowest frame); below that it shows the usual
too-small notice.

## All

`enter` on Select's own `All` row tiles every running orchestration, one
full osi per run:

- **Screens:** `0`-`3` set every tile's own screen at once (the same four
  screens the terminal UI section below describes, one per tile). A Tape
  tile keeps the Tape's own order without the clock column or a detail
  pane, folding whatever doesn't fit into `… n earlier`/`… n more`. A
  Signal tile is a fixed fifteen-row chart on a scale every tile shares
  (the smallest round top at or above 1.5× the largest single finished
  effect's own tokens anywhere on the grid, growing only), its own sent
  trace in its run's own colour, one of six roles repeating past the
  sixth run; under the chart, its own two most recent finished-prompt
  label groups, each sitting at its own trace column; below that, a Now
  pane listing the running leaves then the rows ahead. Flow and Store
  tiles are the ordinary rail and area diagram, clipped to the tile. `u`
  opens the Combined Signal (below).
- **Grid:** as many 56-cell columns as the width fits, as many rows as
  the runs need, then fewer columns when that leaves fewer empty cells,
  never paging a run that would otherwise fit on one page (six runs at
  180 columns make 3×2; five also fit 3×2, with one empty cell; four
  make 2×2; three make one row). More tiles than a page holds page, the
  last slot reading `… n more`; `PageUp`/`PageDown` turn the page. A grid
  too narrow for even one real tile pages every run from the start,
  rather than showing one as if it were the only run going.
- **Keys:** `←→↑↓` move the focus (the focused tile gets the accent
  border); `enter` opens the focused run full-screen on whichever screen
  every tile was already showing, the same attach a Select row opens --
  `esc` from it returns to the grid, not to Select; `esc` on the grid
  itself returns to Select; `T` cycles the colour theme, the same shared
  value Select and the Combined Signal carry; `q` quits at once
  everywhere here, the same as Select.

## The Combined Signal

`u` on All folds every running run into one trace: every run's 100 ms
columns stack on it, sent tokens above a shared axis and received
below it, in launch order -- a run whose own addition completes a row
claims it; the one partial top row, when a column's total never fills a
whole row by itself, goes to whichever run contributes the larger share
of that one row. The axis is anchored to the earliest of every run's own
start and shares one round top across every run (growing only,
persisted for as long as All stays open); the window holds Now wherever
it falls until Now would sit past three quarters of the way across it,
then scrolls to keep it pinned there, so a run of any length still
shows its own latest activity. Under the chart, one
legend row per run: a swatch in the run's own colour, the document, its
first finished prompt (`+N` for the rest, the run's own tokens so far
appended only while there's room before the running label), and the
running label fixed at the Now column. Below that, cross-run Totals
(tokens, Peak, Rate, Quiet), a Per Run table (document, config, engine,
what's running now, tokens, share, done) with a cursor, and Readings.

- **Keys:** `←→` scrub a column cursor, clamped to Now (it never
  scrubs into the future); it moves Peak and Quiet the same way Signal's
  own scrub cursor does. `n` returns to Now; `↑↓` select a run in the
  Per Run table, which scrolls to keep the cursor in view once the runs
  outgrow it; `enter` opens the selected run full-screen on its Signal
  screen -- `esc` from it returns to the Combined Signal, not the grid;
  `esc` on the Combined Signal returns to the grid, with its own focus
  and page unchanged; `T` cycles the colour theme; `q` quits at once;
  `?` shows this screen's own Help, which lists `c` as inert, the same
  as the grid's.
- **Sizes:** below 120 columns, or below the height the chart and its
  own legend row need, the shared too-small notice shows naming 120
  columns.

## The terminal UI

On a TTY, `osi <doc>` and `osi watch <dir>` show the interactive terminal UI
(DESIGN.md §6.3) instead of the plain-text stream. `--log`, or any non-TTY
stdout (a pipe, a file, `CI` set), keeps the plain stream unchanged.

osi has four screens, switched with `0`-`3`: the Tape (`0`, the
default for both `osi <doc>` and `osi watch`), Signal (`1`, the
token trace, below), Flow (`2`, the rail as control flow, below) and
the Store (`3`, where the run's tokens went, below). Selection,
follow, folding, filters and collapsed rows live on `keys::App`
outside the `screen` field, so switching never touches them; each of
the four screens draws its own self-contained header band, status
line, key bar and Totals row (below).

Every screen shares one header band, status line, key bar and Totals
row: `osi <document> · <status> <elapsed>` on the left, the `osi`
word itself animating in place of a separate icon while the run is
live (a static word with `--no-animation`), the screen's own chip
(`TAPE`/`SIGNAL`/`FLOW`/`STORE`) and the clock on the right; below the
screen's own panel, a status line (the run's own latest report once
one has arrived, else the newest finished step); a key bar naming
that screen's own keys; and a Totals row (tokens sent/received and
the total, the run's own deviation from its usual tokens once
history has one, effects done of planned, the ETA and the run's own
cost). `osi watch` names the document from the run's own
`runtime.last_run.orchestration_path`, its own file name alone (issue
#714: `--plan /path/to/ticket.yml` once showed the whole path; two
runs on the same file name are already told apart by the parent
directory on Select and All).

A `report` effect's own events show up in several places: once the
run's first report arrives, the header shows its message (a warning
styled in the theme's own warning colour) in place of the run id,
and it gets its own line in `--log` and (on the Tape and the Flow)
the story. `--theme <name>` (or `OSI_THEME`) paints the whole screen
in one of ten named colour themes (`dracula`, `nord`, `solarized-
dark`/`-light`, `gruvbox-dark`/`-light`, `catppuccin-mocha`/`-latte`,
`tokyo-night`, `one-dark`) instead of the terminal's own default
colours; `T` cycles through them live, on every screen osi draws
(issue #714: a single run's own four screens, Select, All, the
Combined Signal and the full-screen step), sharing one theme across
an attach, never persisted past this one session. `--no-animation`
turns the scope icon's own trace off, on both `osi <doc>` and
`osi watch`.

### The Tape (`0`)

osi's own design document (`oscilloscope/design/tui-design.md`) is the
intent this screen implements; this section is what has actually
shipped of it.

One time axis: finished rows in the order they finished, each
prompt followed by its own faint preview line when previews are on
(`p`); the Now band; the rows currently running, bold, with how long
they've run; a slots rule while a loop is dispatching passes
concurrently; the rows still ahead, faint,
with what they'll do and (once there are three or more finished runs of
the same document) `usually N t` from history; Run End. Every row's
token column is a fixed-scale bar (`▰ = 50 t`, or finer once a run's
own usual steps are small enough); `t` swaps it for each row's own
deviation from its usual figure instead. The detail pane shows the
selected row's status, type, tokens, its usual tokens and how this run
compares, its share of the run's tokens so far, and the prompt sent and
the reply in full; `enter` opens the same content full-screen. A
Totals row (tokens, the run's own deviation from usual, effects done,
ETA, cost) is pinned above the list's own bottom border. While the
run is starting or live, the Now area sits at the list's own vertical
middle, finished rows above it and the plan below, each padded with
blank rows when its own side is short; moving the selection off Now
scrolls only as far as the selection needs, and following Now again
re-centres it. Once the run has ended or failed, the list fills from
the top as before.

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

A running agent tool's own row reads the same way: elapsed time,
the turn (`repair` during the repair turn, `attempt N` on a retry),
the current tool with its own elapsed, and context usage --
`1m12s · turn 4 · bash 12.0s · ctx 41k`; a stale leftover -- including
one left behind by an interrupted run, which ends the whole run
without ever completing the step -- reads `stopped` in place of the
tool and context. A finished agent's own row reads its turns, tool
calls and files changed instead -- `14.1s · 3 turns · 12 tools · 2
files +3 −1`.
The detail pane, the full-screen step (shared by the Signal and the
Store) and the Flow screen's own Selected pane all show the same
Agent block for one: Asked, Turn, Tool, Tools, Context, Files,
Tests, Said (the transcript's own last reply) and Cost, each left
out when the engine never reported it, followed by the transcript
tail (every tailed tool call, newest at the bottom, a running one
with its own elapsed, an ended one with `✓`/`✗` and its own
duration). The tail keeps only as many of the newest calls as the
pane's own height allows, with a leading `…` once either the read
window or that height cut the list short.

A running agent session's own tokens count everywhere too: the Tape
row gets the same token column a prompt's does (faint while running,
solid once finished), the Totals row and every share add the
session's own live totals in, and the run summary's own Largest
names it once it's the biggest step. A stale leftover from an
interrupted session still shows its last known figures, just no
longer faint.

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
this screen's own keys. `Tab`, `z`, `s` and `l` are not Tape keys at
all, nor any other screen's; `v`, `e`, `/` and `←`/`→` are not Tape
keys either, but each still works on at least one of the Signal or
the Flow (never the Store). The Signal, the Flow and the Store each
have their own keys entirely (below). Below 60×24, every screen --
not only the Tape -- shows the too-small notice instead.

The selection is one, shared across all four screens: switching
with `0`-`3` keeps the same effect selected, wherever it names a
row each screen actually has (the Store's own table cursor lands on
the nearest ancestor with tokens of its own when the effect itself
has none; the Signal's own scrub cursor sits at the selected
effect's own column, its end time once finished, its start while
still running, until an actual `←→` scrub moves it, and the
selection, to wherever the cursor then lands). Once a run has ended,
the selection starts on the run's own most notable row (a handled
failure, or else the last one), and the detail pane shows the run
summary for as long as it stays there; moving the selection shows
that row's own details instead, and `enter` opens it.

The full-screen step (`enter`, shared by every screen) shows every
section the run recorded for the selected effect, each only when
there is one: Status, Asked (the prompt sent, in full, or a tool's
own command line), Produced (the value in full), Error, Tokens
(including the usual figure and the share), Attempts, Tool (exit
code, stdout, stderr), Decision (condition, branch, the model's own
answer), Loop (one line per pass), the Agent block, and Meta (every
other key the run recorded, folded -- `m` unfolds it). `[`/`]` step
to the previous/next effect that has started, in plan order;
`↑↓`/`PageUp`/`PageDown`/`Home`/`End` scroll the step's own content
instead of moving the selection; `esc` closes it, back on the
screen it was opened from with the same effect still selected.

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

A running agent session's own tokens plot on the trace too, one
column per increase actually observed (sent above, received below),
at the time each was seen -- not only in the Now column the way a
running prompt's own estimate does -- so the sum of its own columns
always equals its final tokens once it finishes, even for a session
osi never once saw running or one that was retried along the way.
Readings' own Peak, Largest and Quiet read the trace's own columns
up to the cursor; Sent and Received instead always read the same
live-inclusive total the run's own Totals do, never cut at the
scrub cursor, since the trace's own columns alone leave out
anything with no column of its own (a model-mode decision's own
tokens, say).

With no explicit scrub, the cursor sits at the selected effect's own
column instead of always at Now -- its end time once finished, its
start while it is still running. `←`/`→` scrub it freely along the
axis, drawn distinctly from the Now sweep, turning follow off; the
selection then follows whichever step the cursor lands on. `n`
returns it to Now, and moves which column Readings' own Peak,
Largest and Quiet read up to. `+`/`-` zoom the column width from
50 ms to 1 s (100 ms the default), and return a scrubbed cursor to
Now. `⏎` opens the step under the cursor full-screen, the same
shared full-screen step every other screen opens, and only while
there's a step under the cursor to show at all. A run longer than
the pane is wide scrolls to keep the cursor (Now, while following
it) on screen.

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
headline, detail) -- a finished tool's own command and duration, a
finished prompt's own tokens and deviation from its own usual. A
running, streaming prompt's own headline is its received figure
(`↓≈212 t`), its detail the rate (`41 t/s`), `waiting` before the
first delta, or the attempt number on a retry. A running agent's
own headline is its turn and current tool (`turn 4 · bash`), its
detail context usage or files/lines changed, whichever it has; a
finished agent's own station reads exactly like a finished prompt's,
tokens and the bar. A decision forks into two
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
plus Attempts, Tool and Meta when the step has them, or, for an
agent step, the same shared Agent block as the Tape's own
detail pane and full-screen step -- Asked, Turn, Tool, Tools,
Context, Files, Tests, Said and Cost, then the transcript tail -- a
station inlined from a sequential container keeps its own ten-cell
name on the rail, but the Selected pane's own title still says which
container it's inside (`gate › reply`). The Story pane (bottom right, 100
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
scroll lands Now back in view on its own. With follow off, or once
the run has ended, `←`/`→` instead move the selection between the
selected lane's own stations, in plan order -- a plain station, a
named or inferred decision and a fork's own host (a loop, a parallel
`dynamic`) are all stations; a decision's own taken lane flattens
right into the sequence (only one of its two lanes is ever real), a
loop's or a `dynamic`'s own lanes stay their own separate zones,
reached only by `↑`/`↓` first. `↑`/`↓`/`j`/`k` move which
pass is selected, turning follow off the same way, and back on the
moment the selection lands back on the pass the Selected pane would
otherwise default to; `PageUp`/`PageDown`/`Home`/`End`/`g`/`G` are
no-ops here, there being no tree left to page or jump through. `⏎`
opens the current selection full-screen, the same toggle the Tape's
and Signal's own `⏎` already share, armed only while something is
actually selected. `f` follows the running station -- itself, when
it's running outside any loop; the pass that holds it, when it's a
loop's own step; the default pass, with nothing running and nothing
selected. `0`-`3` switch screens; `?` opens Flow's own Help, listing
exactly its own keys and the globals every key bar shares.

| Key | Action |
|---|---|
| `←` / `→` | following a live run: scroll the rail, turning follow off; otherwise: move the selection between the selected lane's own stations |
| `↑` / `↓` / `j` / `k` | select a pass |
| `+` / `-` | open / fold the selected fork's own lanes, three at a time |
| `⏎` | open the selection full-screen, and back |
| `f` | follow the running station (on by default) |
| `0`-`3` | switch screens (Tape, Signal, Flow, Store) |
| `?` | help |
| `c` | cancel, with a confirm (the same as a confirmed `q`) |
| `q` | quit — asks first if a run is going; with nothing running, quits at once; in `osi watch` on a still-going run, detaches at once with a distinct ending (`■ detached (the run is still going)`, exit 0) |
| Ctrl-C | checked before any dialog, never merely dismissing one: cancels at once with no confirm while something's running, quits at once with nothing running, and in `osi watch` always just detaches |

The token rule is the same everywhere a count shows up (the header, the
rail, the Selected pane and the Store's own tiles and table): a
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
 osi  demo.yml · ochre-desert · ● Running 3.6s                                                         FLOW    12:50:50 
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
│ Status   ● Running  pause · 0.6s · slot 1                  ││ 12:50:47 ■ run started · ochre-desert                  │
│ Draft    ✓ Done  ↑118 ↓12 · 130 t  -16% vs usual 154 t     ││ 12:50:50 ◆ gate → then                                 │
│ Produced Cats nap in sunbeams and judge you quietly.       ││ 12:50:50 ⟳ drafts · 3 passes · 2 wide                  │
│                                                            ││                                                        │
│                                                            ││                                                        │
│                                                            ││                                                        │
╰────────────────────────────────────────────────────────────╯╰────────────────────────────────────────────────────────╯
 ✓ gate › reply finished · ↑348 ↓41 · 0.6s ago
 ←→ Scroll  ↑↓ Lane  +/- Lanes  f Follow  enter Open  0-3 Screens                                        ? Help  q Quit 
```

### The Store (`3`): where the run's tokens went

An area diagram of filled tiles, one per store (a leaf step) that has
used tokens so far, drawn self-contained the same way the Tape, the
Signal and the Flow are: a decision, loop, `dynamic` or `use` step is
always a container, a darker surface holding its own children's
tiles (in the `terminal` theme, issue #714: a leaf tile fills with
bright black instead, and a container marks its own bounds with a
guide line along its left edge, since no second ANSI colour reads as
a safe fill against every terminal's own background); a loop pass
folds into its own single token-bearing step when
it has exactly one (`drafts › iter_0` reads as `iter_0 › draft`),
staying a plain container, named `iter_N`, once it has more than one.
Nothing is drawn for a store, or a whole container, that has not used
any tokens at all; a running container's own title carries `●` and
the accent colour until the run itself ends. Children sort in plan
order, not alphabetically by path. A tile's own text drops lines from
the bottom as it runs out of room: `name · N t` on one line where it
fits, else `name` then `N t` on their own two, then the deviation from
usual (absent without history), then sent and received. A container
too small for its own children collapses to its own title and
`<n>/<m> with tokens`; too-small top-level stores fold together into
one trailing `… N more · T t` tile.

Below the diagram, the Largest Stores table ranks every leaf by
tokens descending, ties in plan order: each row's own full path from
the run's root (`drafts › iter_1 › draft`, cut on the right with
`⋯`), the fixed-scale bar, tokens, share of the run's tokens so far
and deviation, with a cursor. Readings shows the table's own first
row (Largest), how many stores have used tokens so far, the ratio of
sent to received tokens, and the running loop's -- or the last one
that ran's -- own `name · n of m passes have tokens`, absent with no
loop in the document at all. A running agent session gets a tile
from its own live totals the same way a running prompt's estimate
does; every share here counts a running step's own live tokens too,
so it never reads above 100%.

`↑↓`/`j`/`k`/`PageUp`/`PageDown`/`Home`/`End`/`g`/`G` move the
table's own cursor -- the one shared selection every screen reads,
so it stays on the same effect switching to another screen and back,
resolved to the nearest ancestor with tokens of its own when the
effect itself has none; `enter` opens the selected store's own step
full-screen, the same shared toggle the other three screens already
use, and `esc` closes it. `i`/`o`/`$`, `Tab`, `z`, `v`, `e`, `/` and
`←`/`→` are not Store keys at all.

| Key | Action |
|---|---|
| `↑↓` / `j`/`k`/`PageUp`/`PageDown`/`Home`/`End`/`g`/`G` | select a row in the Largest Stores table |
| `⏎` | open the selected store's own step full-screen, and back |
| `0`-`3` | switch screens (Tape, Signal, Flow, Store) |
| `?` | help |
| `c` | cancel, with a confirm (the same as a confirmed `q`) |
| `q` | quit -- asks first if a run is going; with nothing running, quits at once; in `osi watch` on a still-going run, detaches at once with a distinct ending (`■ detached (the run is still going)`, exit 0) |
| Ctrl-C | checked before any dialog, never merely dismissing one: cancels at once with no confirm while something's running, quits at once with nothing running, and in `osi watch` always just detaches |

At 100 columns and up, and 30 rows and up, the chart sits above the
table (about 62% of the lower band's own width) beside Readings;
below 30 rows, still 100 columns and up, Readings drops and the table
keeps three rows instead of four; 80-99 columns is one pane, no
Readings, the table at the pane's own full width, still three rows;
below 80 columns the Store isn't offered, bouncing back to the Tape
on its own if the terminal shrinks below it while the Store is on
screen. The title drops `Tokens by Store` below 120 columns.

A `TestBackend` capture (`crates/oscilloscope/src/tui.rs`'s own golden
tests, `design/frames/store--normal--120x30.mock`'s cell-exact twin,
checked cell by cell rather than through `insta`) of `design/
fixtures/run2`, replayed to 3.62 s after its own start, the cursor on
`gate › reply`:

```
 osi  demo.yml · ochre-desert · ● Running 3.6s                                                        STORE    12:50:50 
╭─ Store · Tokens by Store · Area = Tokens Used So Far ────────────────────────────────────────────────────────────────╮
│  kind · 215 t                 gate · 389 t                                        ● drafts · 261 t                   │
│  +6% vs usual                  reply · 389 t                                       iter_0 › draft · 130 t            │
│  ↑212 ↓3                       +47% vs usual                                       -16% vs usual                     │
│                                ↑348 ↓41                                            ↑118 ↓12                          │
│                                                                                                                      │
│                                                                                                                      │
│                                                                                                                      │
│                                                                                                                      │
│                                                                                    iter_1 › draft · 131 t            │
│                                                                                    -14% vs usual                     │
│                                                                                    ↑118 ↓13                          │
│                                                                                                                      │
│                                                                                                                      │
│                                                                                                                      │
│                                                                                                                      │
│                                                                                                                      │
│──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────│
│ Totals ↑796 ↓69 · 865 t  +12% Usual  6/14 Effects  ETA 12:50:52  Cost —                                              │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
╭─ Largest Stores · ▰ = 50 t ────────────────────────────────────────────╮╭─ Readings ─────────────────────────────────╮
│   STORE                               TOKENS SHARE VS USUAL            ││ Largest gate › reply 389 t · 45%           │
│ ❯ gate › reply               ▰▰▰▰▰▰▰▰  389 t   45%     +47%            ││ Stores  4 with tokens so far               │
│   kind                       ▰▰▰▰▱▱▱▱  215 t   25%      +6%            ││ Ratio   11.5 : 1 sent to received          │
│   drafts › iter_1 › draft    ▰▰▰▱▱▱▱▱  131 t   15%     -14%            ││ Loop    drafts · 2 of 3 passes have tokens │
│   drafts › iter_0 › draft    ▰▰▰▱▱▱▱▱  130 t   15%     -16%            ││                                            │
╰────────────────────────────────────────────────────────────────────────╯╰────────────────────────────────────────────╯
 ✓ iter_1 › draft finished · ↑118 ↓13 · 0.6s ago                                                                        
 ↑↓ Select  enter Open  0-3 Screens                                                                      ? Help  q Quit 
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
    oscilloscope-core/       # no terminal code, so a future GUI can reuse it (render.rs: the TUI's own render model, store.rs: the Store screen's own area-diagram layout, table and Readings)
    oscilloscope/            # the `osi` binary: clap CLI, the plain stream, and the ratatui/crossterm TUI (tui.rs, keys.rs, terminal.rs)
```

## Read more

- [`DESIGN.md`](DESIGN.md) — the full design, moved from issue #418:
  measurements of what `cof`'s live state and a new `--events` stream show,
  the inference rules, engine launching, architecture and milestones.
