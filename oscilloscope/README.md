# oscilloscope

`osp` (oscilloscope) launches a Circuitry orchestration on either engine —
`cof` (this repository's Python reference implementation) or `electricity`
(the Rust runner in `../electricity`) — and shows it running: every effect's
status against the document's plan, plus a readable log of what is
processing. A terminal UI comes first; a GUI can reuse the same core later.

```
CYBERDINER_TOKEN=... osp do-thing.yml config.json
```

**This is a preview.** Milestone O-1 (issue #424) filled in the core: `osp
<orchestration>` launches and supervises `cof` or `electricity` and exits
with the engine's own exit code; `osp watch <dir>` attaches to a run `osp`
didn't start. Milestone O-2 (issue #434) adds the terminal UI below.
electricity's own M0-H milestone (issue #431) gave it a real run path,
`--events` and `--live-state`, so `--engine electricity` now shows the
same live view `cof` does, for the documents it runs so far: `tool`
effects (the `json` provider), `dynamic`, a CEL `if`, and `finally:`.
Everything else (`prompt`/`loop`/`use`/`reflector`/`yield`, a model-mode
`if`, other tool providers) is refused up front, with a message naming the
preview marker, before any state is written. See [`DESIGN.md`](DESIGN.md)
§4.2.

```
osp do-thing.yml config.json
osp do-thing.yml -e key=value --engine cof --out-dir ./run
osp watch ./run
osp watch ./run --plan do-thing.yml --config config.json -e key=value
```

- With no `config.json`, `cof` resolves its own config layers (global,
  project, environment) exactly as `cof run` would. The plan osp compiles
  from the document uses the same `config.json`/`-e` pair passed on the
  command line (`electricity::check_options`, the exact options a real
  `electricity <config> <doc> -e k=v...` run would check it against) —
  **osp does not reproduce `cof`'s own config discovery**, so a document
  that relies on a `group:`/`concurrency_groups` defined only in the
  global or project `circuitry.config.json` layer, not in an explicit
  `config.json` given here, still falls back to no plan, on either engine.
  `osp watch --plan do-thing.yml` takes its own `--config`/`-e` for the
  same reason, independent of whatever the watched run itself was started
  with.
- `--engine electricity` needs a config (electricity has no config
  discovery yet). For a document outside electricity's current scope, it
  shows electricity's own preview-marker refusal and exit code, the same
  way osp shows any other pre-execution failure.
- Run files (`state.live.json`, `state.json`, `events.jsonl`, `stdout.txt`,
  `stderr.txt`) go to a fresh temporary directory by default, printed when
  the run ends; `--out-dir DIR` keeps them in a chosen directory instead —
  the same layout `osp watch` reads.
- `cof run --events` (issue #419) is detected from `cof run --help`, and
  electricity's own `--events`/`--live-state` (issue #431) the same way from
  `electricity --help`; without them `osp` falls back to `--live-state`/
  `--out` alone, state-only (DESIGN.md §2's "From state alone" rules).
- `osp watch <dir>` on a run with no `--events` stream has no engine pid to
  check and no way to tell a genuinely aborted run (a second signal,
  `SIGKILL`, a crash) from one that's simply still going quietly: neither
  ever writes a final snapshot. `osp watch` warns once on stderr when this
  is the case, and otherwise just keeps waiting — there is no staleness
  timeout, since a quiet run can stay quiet for a long time. Ctrl-C always
  stops the watch.
- A document that fails to compile (or an engine whose compiler is still a
  stub) falls back to a plan-free run: rows come only from what's observed.
- `osp watch <dir>` without `--plan` cannot yet tell an `if`/loop container
  from a leaf by its own `meta` shape the instant its *first* observation
  arrives, so a container can briefly print its own `▶`/`✓` the way a live
  `osp <doc>` run of the same document never does (it already has a
  compiled plan from the start). Pass `--plan do-thing.yml` to `osp watch`
  for the same output a live run gives.
- The first Ctrl-C forwards `SIGINT` to the engine's process group and logs
  a cancelling notice; a second forwards it again; still running 10s later,
  `osp` sends `SIGKILL`. `SIGTERM`/`SIGHUP` are forwarded the same way. `osp`
  always exits with the engine's own exit code, and never leaves it running
  after `osp` itself exits cleanly — including on a panic. The one exception
  is `osp` itself being `SIGKILL`ed: nothing can run its own cleanup code
  after that, so the engine (in its own process group) keeps running.

## The terminal UI

On a TTY, `osp <doc>` and `osp watch <dir>` show the interactive terminal UI
(DESIGN.md §6.3) instead of the plain-text stream: a header (document,
engine, run state, elapsed time, effects done out of planned, tokens, the
innermost running loop's own ETA), the plan tree on the left (a status
glyph, duration, loop `n/total`, and the provider or model in a dim colour),
the selected row's own details on the right, and a log pane at the bottom —
the same lines `--log` prints, plus the engine's stderr. `--log`, or any
non-TTY stdout (a pipe, a file, `CI` set), keeps the plain stream unchanged.

| Key | Action |
|---|---|
| `↑↓` / `j`/`k` | move |
| `←` / `→` | collapse / expand |
| `tab` | switch pane |
| `f` | follow the running row |
| `e` | errors only |
| `/` | filter |
| `v` | show the full value; `v` again cycles between the prompt and the value when the row has both |
| `c` | cancel, with a confirm (the same as a confirmed `q`) |
| `q` | quit — asks first if a run is going; with nothing running, quits at once; in `osp watch` on a still-going run, detaches at once with a distinct ending (`■ detached (the run is still going)`, exit 0) |
| Ctrl-C | checked before any dialog, never merely dismissing one: cancels at once with no confirm while something's running, quits at once with nothing running, and in `osp watch` always just detaches |
| `?` | help |

The engine exiting does not, on its own, end the TUI: the header, every
row's final status and the details pane stay live on whatever the run
ended with until the user leaves with a confirmed `q`, a Ctrl-C cancel, or
an external `INT`/`TERM`/`HUP` to osp itself (a `SIGHUP` never waits for a
key — the terminal is gone by definition). Short of one of those three, a
run that finishes on its own stays up for `q`/Ctrl-C to leave.

A `TestBackend` capture (`insta`, `crates/oscilloscope/src/tui.rs`'s own
golden tests) of a running tree loop, at 70×20:

```
┌────────────────────────────────────────────────────────────────────┐
│do-thing.yml  cof  running  5.0s  effects 3/6  ↑0 ↓0  ETA 3.0s      │
└────────────────────────────────────────────────────────────────────┘
┌ plan ──────────────────────────────────┐┌ details ─────────────────┐
│◐ prime                                 ││path   prime.fan          │
│✓ a  shell                              ││type   loop               │
│✓ b  shell                              ││plan   loop "fan"         │
│◐ fan  1/4                              ││                          │
│  ✓ iter_0  shell                       ││                          │
│                                        ││                          │
│                                        ││                          │
│                                        ││                          │
│                                        ││                          │
│                                        ││                          │
└────────────────────────────────────────┘└──────────────────────────┘
┌ log ───────────────────────────────────────────────────────────────┐
│                                                                    │
│                                                                    │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

Raw mode and the alternate screen are entered once and restored on every
exit path — a normal end, osp's own error, a panic (a panic hook), and a
forwarded `SIGINT`/`SIGTERM`/`SIGHUP`, forwarded to the engine as the same
signal it was — the same `SIGKILL` exception as the supervision section
below. Quitting while osp is running the engine cancels the run first (the
confirmed `q`/`c` dialog, or Ctrl-C): osp never leaves the engine running.

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

`cargo build --release -p oscilloscope` produces the `osp` binary at
`target/release/osp`.

## Layout

```
oscilloscope/
  Cargo.toml                 # workspace; path deps on ../electricity/crates/{electricity-compiler,electricity-bytecode}
  crates/
    oscilloscope-core/       # no terminal code, so a future GUI can reuse it (render.rs: the TUI's own render model)
    oscilloscope/            # the `osp` binary: clap CLI, the plain stream, and the ratatui/crossterm TUI (tui.rs, keys.rs, terminal.rs)
```

## Read more

- [`DESIGN.md`](DESIGN.md) — the full design, moved from issue #418:
  measurements of what `cof`'s live state and a new `--events` stream show,
  the inference rules, engine launching, architecture and milestones.
