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

## The terminal UI

On a TTY, `osi <doc>` and `osi watch <dir>` show the interactive terminal UI
(DESIGN.md §6.3) instead of the plain-text stream. `--log`, or any non-TTY
stdout (a pipe, a file, `CI` set), keeps the plain stream unchanged.

On a terminal big enough (120×40 shows every pane usefully): a header (a
small animated oscilloscope icon to its left, on a wide enough terminal); a
left column of the details pane (the selected row's own meta, and, for a
prompt, a `sent`/`received` block with a snippet and token count each)
over the orchestration pane (the plan tree, a status glyph, duration, loop
`n/total` and the provider/model in a dim colour), the column split and
the left column's own row split both at the golden section — the larger
share always going to whichever side grows with the run (the left column
over the right, the orchestration pane over the details pane, the state
pane over the treemap and the log pane); a right column
of a nested `tokens by key` treemap (cell area proportional to tokens,
coloured by status, squarified and laid out as pure integer geometry in
`oscilloscope-core`), the state pane as a tree (every effect node nested
by depth, latest-or-current first, with its status, a dim model/provider/
duration/retries/loop-progress summary and its own or its subtree's
tokens — no value or error text, read those in the details pane), and the
log pane (the same lines `--log` prints, plus the engine's stderr); and a
token bar across the full bottom width (the `↑`/`↓` totals as bars, and
what's in flight — an estimated input for every prompt currently running,
from `ceil(chars(prompt_sent) / 4)`, and how many). On a smaller terminal,
panes drop in this order until the rest fits — the header and the token
bar never disappear:

1. below 100×30: the treemap;
2. below 90×24: the state pane too (the right column becomes the log pane alone);
3. below 80×20: the two-column layout itself, down to one column (details, orchestration, log stacked); the scope icon stops being drawn at this width too.

A `report` effect's own events (`electricity/docs/spec/runtime-semantics.md` §8.7) show up in three places.
Once the run's first report arrives, the header shows its message in
place of the run id (a warning styled in the theme's own warning colour);
document path and message both truncate before status, elapsed, effects
or tokens ever do, collapsing to a bare `▸` once there's barely room for
it at all. The run id itself moves to the root row's own details, and to
a `■ run started` line in `--log`. Every report, named or not, gets its
own log line (`▸ <message>`, `▸ warning: <message>`), with a compact
`data` preview after it when it has one. A *named* report still writes a
shadow-state node (so it has a row in the state pane, with its own
`level`/message/clock time and `data` as pretty JSON in the details pane
once selected there) but, named or not, a report never becomes a row in
the orchestration pane, and never changes how a loop pass folds.

Follow is on when the TUI starts: the orchestration pane's own selection
tracks the running effect, and the details pane shows it with no key press.
Any manual move (`↑↓`/`j`/`k`, `PgUp`/`PgDn`, `Home`/`End`, `g`/`G`) turns
follow off; `f` turns it back on. The orchestration pane scrolls, keeping
the selected row in view, with a `12/48`-style position in its own border
whenever a row is hidden above or below; the details and state panes scroll
the same way once they have focus (`tab` cycles orchestration → details →
state), and the state pane's own selection (and its own collapsed set) is
independent of the orchestration pane's.

Whatever just changed gets a soft background tint that fades out over about
a second, so the eye goes to it: a row's status, a state row, a treemap
cell, a new log line or the token totals. The first observation never
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
| `↑↓` / `j`/`k` | move (the focused pane's own selection, or the details pane's scroll) |
| `PgUp`/`PgDn` | move a page |
| `Home`/`End`, `g`/`G` | jump to the first / last row |
| `←` / `→` | collapse / expand (orchestration or state pane) |
| `⏎` | expand a quick-steps row in place |
| `z` | fold on/off (breadcrumbs, quick steps); on by default |
| `tab` | orchestration → details → state |
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
token bar, the state pane, details and the treemap): a node's own
`tokens_sent_total`/`tokens_received_total` when present, falling back to
`tokens_sent`/`tokens_received` otherwise — the same pair both `cof`'s and
electricity's own `runtime.last_run.totals` are built from, so a retried
prompt can never make one surface disagree with another. There is no live
*output* count: both engines report a prompt's tokens only once it
finishes. Every token count is formatted the same one way too (`369 t`,
`18.0 kt`, `142 kt`, `2.1 Mt`), and so is every duration, everywhere one
shows up (`0.4s`, `6m12s`, `2h05m`, `3d04h`) — `oscilloscope_core::format`'s
one formatter each. Clock times (`HH:MM:SS` in the log, the header's own
`since 11:04:58` while a run is going and `11:04:58–11:31:10` once it has
ended, and the innermost loop's ETA as `ETA ~11:42`) go through the
binary's own real local zone, resolved per timestamp.

The orchestration pane folds a single-child container into one breadcrumb
row (`review › fix › agent`) and two or more consecutive, finished,
sub-second tool steps into one `N quick steps` row — `z` turns folding off
to see the full tree, and `⏎` expands a quick-steps row in place. A loop
pass is labelled `pass N of M` there (the state pane keeps the real
`iter_N` key). A running `agent` tool step counts in the token bar's
in-flight line next to running prompts (`2 prompts · 1 agent in flight`),
with no token estimate of its own.

The engine exiting does not, on its own, end the TUI: the header, every
row's final status and the details pane stay live on whatever the run
ended with until the user leaves with a confirmed `q`, a Ctrl-C cancel, or
an external `INT`/`TERM`/`HUP` to osi itself (a `SIGHUP` never waits for a
key — the terminal is gone by definition). Short of one of those three, a
run that finishes on its own stays up for `q`/Ctrl-C to leave.

A `TestBackend` capture (`insta`, `crates/oscilloscope/src/tui.rs`'s own
golden tests) at 120×40, every pane populated:

```
╭────────────┬──╮┌─────────────────────────────────────────────────────────────────────────────────────────────────────┐
│⠔⠒⠒⠤⠤⠔⠒⠒⠤⠤⠔⠒│◉ ││do-thing.yml  cof  running  5.0s  effects 2/6  ↑170 t ↓410 t                                         │
╰────────────┴──╯└─────────────────────────────────────────────────────────────────────────────────────────────────────┘
┌ details ───────────────────────────────────────────────────────────────┐┌ tokens by key ─────────────────────────────┐
│path   prime.a                                                          ││┌prime 580 t───────────────────────────────┐│
│type   tool                                                             │││a 460 t                          ┌fan 12…┐││
│plan   tool "a"                                                         │││                                 │iter_0…│││
│adapter  scripted                                                       │││                                 │       │││
│                                                                        │││                                 │       │││
│value (v for full):                                                     │││                                 │       │││
│the summary text                                                        │││                                 │       │││
│                                                                        │││                                 │       │││
│                                                                        │││                                 │       │││
│                                                                        │││                                 └───────┘││
│                                                                        ││└──────────────────────────────────────────┘│
└────────────────────────────────────────────────────────────────────────┘└────────────────────────────────────────────┘
┌ orchestration do-thing ────────────────────────────────────────────────┐┌ state ─────────────────────────────────────┐
│◐ prime                                                                 ││◐ fan  1/4                                  │
│✓ a                                                                     ││  ↑50 t ↓70 t                               │
│✓ b  shell                                                              ││  ◐ iter_0                                  │
│◐ fan  1/4                                                              ││    ↑50 t ↓70 t                             │
│  ◐ pass 1 of 4                                                         ││✓ a                                         │
│                                                                        ││  ↑120 t ↓340 t                             │
│                                                                        ││✓ b  shell                                  │
│                                                                        ││                                            │
│                                                                        ││                                            │
│                                                                        ││                                            │
│                                                                        │└────────────────────────────────────────────┘
│                                                                        │┌ log ───────────────────────────────────────┐
│                                                                        ││                                            │
│                                                                        ││                                            │
│                                                                        ││                                            │
│                                                                        ││                                            │
│                                                                        ││                                            │
│                                                                        ││                                            │
└────────────────────────────────────────────────────────────────────────┘└────────────────────────────────────────────┘
┌ tokens ──────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│in  █████████████████ 170 t   out  ████████████████████████████████████████ 410 t                                     │
│now in ≈6 t   (1 prompt in flight)                                                                                    │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
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
    oscilloscope-core/       # no terminal code, so a future GUI can reuse it (render.rs: the TUI's own render model, treemap.rs: the `tokens by key` layout)
    oscilloscope/            # the `osi` binary: clap CLI, the plain stream, and the ratatui/crossterm TUI (tui.rs, keys.rs, terminal.rs)
```

## Read more

- [`DESIGN.md`](DESIGN.md) — the full design, moved from issue #418:
  measurements of what `cof`'s live state and a new `--events` stream show,
  the inference rules, engine launching, architecture and milestones.
