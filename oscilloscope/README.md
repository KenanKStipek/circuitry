# oscilloscope

`osp` (oscilloscope) launches a Circuitry orchestration on either engine —
`cof` (this repository's Python reference implementation) or `electricity`
(the Rust runner in `../electricity`) — and shows it running: every effect's
status against the document's plan, plus a readable log of what is
processing. A terminal UI comes first; a GUI can reuse the same core later.

```
CYBERDINER_TOKEN=... osp do-thing.yml config.json
```

**This is a preview.** Milestone O-1 (issue #424) fills in the core: `osp
<orchestration>` launches and supervises `cof` (or, once it runs documents,
electricity), prints a plain-text log of what's running, and exits with the
engine's own exit code; `osp watch <dir>` attaches to a run `osp` didn't
start. There is no terminal UI yet — that's milestone O-2 — so every run
prints the same plain-text stream today, `--log` notwithstanding. See
[`DESIGN.md`](DESIGN.md).

```
osp do-thing.yml config.json
osp do-thing.yml -e key=value --engine cof --out-dir ./run
osp watch ./run
```

- With no `config.json`, `cof` resolves its own config layers (global,
  project, environment) exactly as `cof run` would.
- `--engine electricity` needs a config (electricity has no config
  discovery yet) and, until electricity runs documents, just shows
  electricity's own "preview" message and exit code.
- Run files (`state.live.json`, `state.json`, `events.jsonl`, `stdout.txt`,
  `stderr.txt`) go to a fresh temporary directory by default, printed when
  the run ends; `--out-dir DIR` keeps them in a chosen directory instead —
  the same layout `osp watch` reads.
- `cof run --events` (issue #419) is detected from `cof run --help`; without
  it `osp` falls back to `--live-state`/`--out` alone, state-only (DESIGN.md
  §2's "From state alone" rules).
- `osp watch <dir>` on a run with no `--events` stream has no engine pid to
  check and no way to tell a genuinely aborted run (a second signal,
  `SIGKILL`, a crash) from one that's simply still going quietly: neither
  ever writes a final snapshot. `osp watch` warns once on stderr when this
  is the case, and otherwise just keeps waiting — there is no staleness
  timeout, since a quiet run can stay quiet for a long time. Ctrl-C always
  stops the watch.
- A document that fails to compile (or an engine whose compiler is still a
  stub) falls back to a plan-free run: rows come only from what's observed.
- The first Ctrl-C forwards `SIGINT` to the engine's process group and logs
  a cancelling notice; a second forwards it again; still running 10s later,
  `osp` sends `SIGKILL`. `SIGTERM`/`SIGHUP` are forwarded the same way. `osp`
  always exits with the engine's own exit code, and never leaves it running
  after `osp` itself exits cleanly — including on a panic. The one exception
  is `osp` itself being `SIGKILL`ed: nothing can run its own cleanup code
  after that, so the engine (in its own process group) keeps running.

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
    oscilloscope-core/       # no terminal code, so a future GUI can reuse it
    oscilloscope/            # the `osp` binary: clap CLI, ratatui TUI (O-2)
```

## Read more

- [`DESIGN.md`](DESIGN.md) — the full design, moved from issue #418:
  measurements of what `cof`'s live state and a new `--events` stream show,
  the inference rules, engine launching, architecture and milestones.
