# Terminal UI — app chrome

The TUI is the Textual app behind `cof tui` (and a bare `cof` on a TTY). It
ships in the optional `tui` extra:

```sh
pip install "circuitry-cof[tui]"
```

This page documents the shell — navigation, keys, layout breakpoints, logging
and the test harness — and the views that have landed. Views still to come
(Library, Run, Inspect, Runs) render a placeholder until they do.

| Key | View |
| --- | --- |
| `1`–`4` | Library, Run, Inspect, Runs — placeholders |
| `5` / `6` | [Doctor / Settings](#doctor-5-and-settings-6) |
| `7` | [Validate](#validate-7) |
| `8` | [Chat](#chat-8--build-an-orchestration-by-talking-to-it) |

## Keymap

| Key | Action |
| --- | --- |
| `1`–`8` | Jump straight to a view, in registry order |
| `Tab` / `Shift+Tab` | Cycle forward/backward through home and every view |
| `Enter` | Open the highlighted view from the home list |
| `?` | Toggle the help overlay |
| `q` / `Esc` | Back to home; from home, quit |
| `Ctrl-C` | Quit, from anywhere, always |

`q` and `Esc` are deliberately "back then quit": quitting is never more than
one level away, and never a surprise from inside a view. `Ctrl-C` is bound with
Textual `priority` so it wins over the screen-level copy binding.

Views replace one another rather than stacking, so the screen stack is never
deeper than home + view (+ the help overlay).

## Help overlay

`?` opens a modal built from `app.active_bindings` — the same table Textual
dispatches keypresses through — so the overlay cannot drift from the real
keymap. Rows are grouped:

- **Global** — the app's own bindings (view keys, navigation, quit)
- **This screen** — bindings declared by the screen you were looking at
- **Focused widget** — bindings of whatever currently has focus

Bindings Textual marks `system=True`, disabled bindings and bindings without a
description are left out. A new binding on a screen shows up in the overlay
with no extra work; there is no hand-maintained help table anywhere.

## Resize safety

`circuitry.tui.layout` holds the primitives:

- `size_classes(width, height)` → the CSS classes for a viewport
- `ResponsiveLayout` — mixin that stamps those classes on mount and on resize
- `fit(text, width)` — truncate with an ellipsis, never raise
- `key_column_width(available)` — column sizing for key/description lists

Two breakpoints drive the stylesheet:

| Class | When | Effect |
| --- | --- | --- |
| `-compact` | width < 48 | padding and borders drop, secondary copy hides |
| `-tiny` | width < 24 or height < 8 | header and footer hide; every row goes to content |

Every screen body is a scroll container, which is what keeps a full-size view
renderable in a 10x4 terminal. The suite renders each screen from 80x24 down
to 1x1 and asserts the frame stays rectangular.

## Logging in TUI mode

A log record written to stdout while Textual owns the screen shreds the frame.
`circuitry.tui.log.tui_logging()` wraps the app run and, for the duration:

- detaches *console* handlers (stdout/stderr `StreamHandler`s, and
  `rich`-style handlers whose `console.file` is stdout/stderr) from the root
  and `circuitry` loggers;
- leaves every other handler alone — file handlers keep receiving records, so
  a configured log file is complete;
- parks a `NullHandler` on any logger left with none, so Python's
  `logging.lastResort` cannot write warnings to stderr;
- restores the original handlers on exit, including when the app crashes.

## Doctor (`5`) and Settings (`6`)

Doctor answers "can this machine run anything" in two panels.

The top panel is the preflight walk — the same `check()` call `cof doctor`
makes on every enabled (or, with no allowlist, every compiled-in) adapter, tool
plugin and configured runtime plugin. Enumerating what will be checked is
instant, so every row is on screen immediately in a `checking…` state; results
replace them as they arrive, up to `CHECK_CONCURRENCY` probes at a time. A slow
network check therefore costs you one row, never the keyboard. `Ctrl-R` re-runs
the whole walk.

Rows classify the same way the CLI does:

| State | Meaning |
| --- | --- |
| `ok` | `check()` returned ready |
| `deferred` | can only be built with a runtime-injected handler (`host_claude`) |
| `missing deps` | `check()` reported missing items |
| `error` | unknown extension, failed load, or a `check()` that raised |

Missing items are translated out of the `CheckResult` grammar into next steps:

| Item | Rendered as |
| --- | --- |
| `env:OPENAI_API_KEY` | Set the OPENAI_API_KEY environment variable, then re-run the check. |
| `binary:ffmpeg` | Install ffmpeg and make sure it is on your PATH. |
| `library:pymongo` | Install the Python package: pip install pymongo |
| `host:http://localhost:11434` | Start the service at http://localhost:11434, or point the config at a host that is up. |

The bottom panel — and the Settings view on its own — is
`resolve_effective_settings` flattened to one row per value, each tagged with
the layer it came from (`cli`, `orchestration`, `config`, `default`). `runtime`
is flattened to dotted keys so a nested credential gets its own row, and every
value goes through `circuitry.cli.redaction.redact` first, so a token renders as
`***REDACTED***` and never reaches the compositor.

## Validate (`7`)

Type a path, press Enter. `cof check` returns at the first gate that trips,
because all it owes the shell is an exit code; this view runs each gate
independently and groups everything it finds by class:

| Class | Gate |
| --- | --- |
| Load | file unreadable, empty, or unparseable — nothing else can run |
| Schema | `orchestration.schema.json`, with the JSON pointer to the offending field |
| Allowlist | `enabled_adapters` / `enabled_tools` / `enabled_plugins` |
| Compile | `compile_orchestration` |
| Cycle | `detect_cycles` over the static `use:` graph |
| Preflight | `check()` on every extension the file references, with next steps |

Gates that could not run (no config resolved, `jsonschema` absent) are named
under "Not checked:" rather than being silently counted as passing. `Ctrl-R`
re-validates the current path after you have edited the file.

Validation reads files and may probe the network, so it runs in a worker; the
view sits in a `Validating…` state until the report lands.

## Chat (`8`) — build an orchestration by talking to it

The front end for [the wizard](./wizard.md). A light seed form — name,
category, one-line goal — opens a conversation; from there every message you
send re-runs `curation/agents/wizard.yml` over the transcript so far, on a
worker thread, and paints what comes back:

| Turn output | Where it lands |
| --- | --- |
| `say` | the next bubble in the conversation |
| `yaml` | the pane on the right, under the validator's verdict |
| `done` | the save hint, once the draft is also valid |

The wizard handles one turn; the host owns the loop. That host —
`circuitry.tui.wizard_host` — is deliberately free of Textual: the seed, the
transcript, the draft, the verdict and the two save paths are the same objects
`scripts/wizard-chat` could drive from a terminal, which is what makes the flow
testable without a screen.

**The pane's verdict is the file's verdict.** Every draft is put back through
`runtime_shim.validate` — the gate `cof check` runs, minus preflight — rather
than trusting the `valid` flag the wizard reports about its own work. A green
`✔ Valid` means the file that would be written passes; a red `✘ N problems`
lists the validator's own messages. Saving is gated on that verdict in
`wizard_host`, not in the screen, so there is no path from an invalid draft to
a file on disk.

| Key | Command | Action |
| --- | --- | --- |
| `Ctrl-S` | `/save [path]` | Write the draft to the path in the save box |
| `Ctrl-G` | `/library` | Save into the local library and index it |
| `Ctrl-R` | `/run` | Hand the saved file to the Run view |

A library save writes `<library>/<category>/<slug>.yml` plus an entry in that
folder's `manifest.json` — the shape `FolderSource` reads and the curation
manifest schema validates, so a saved orchestration is immediately reachable as
`cof run <category>/<slug>`. The library is the first `folder` source in
`runtime.library.sources` (see [library sources](./library-sources.md)), or
`~/.circuitry/library` when none is configured. Re-saving a name replaces its
entry rather than appending a duplicate.

"Run it now" sets `app.pending_run` and opens the Run view; the Run view reads
it on mount. Until that view lands, the hand-off is the seam and the Run screen
is still a placeholder.

The view never constructs an adapter. It calls a `TurnRunner` — by default
`api.run_orchestration` over whatever the config resolves — which is what makes
it adapter-agnostic, and what lets `tests/tui/test_chat_view.py` drive the real
wizard (its `validate_yaml` tool, its revision loop, its `done` gate) over a
scripted adapter, all the way to a saved file that `cof check` accepts.

## Adding a view

Views are declared once, in `circuitry/tui/screens.py`:

```python
VIEWS = (
    ViewSpec("library", "Library", "Browse bundled and shared orchestrations", "1"),
    ...
)
```

Registration drives the home list, the number key, the Tab cycle and the help
overlay together. To replace a placeholder, subclass `ViewScreen` (implementing
`compose_body`) and set the spec's `factory`; nothing else in the shell changes.
The factory is a small function that imports the screen locally, which is what
keeps `screens.py` the single registry without it depending on the screens
registered in it.

Two rules the built views follow, both learned the hard way:

- Anything a view renders that came from a message — a `check()` message, a
  validator error — is rendered with `markup=False`. A schema error quoting
  `[A-Za-z_]` is not a style tag, and Textual will happily eat it.
- Slow work belongs in a `run_worker(..., thread=True)` that hands results back
  through `app.call_from_thread`, with the screen painting a per-item pending
  state up front. Set a closing flag in `on_unmount` so a result cannot be
  posted into a screen that has gone away.

## Testing

`tests/tui/conftest.py` exposes the shared harness:

- `run_app(scenario, *, app=None, size=(80, 24))` — drive a Textual pilot from
  a plain synchronous test
- `render(*, app=None, size=..., keys=(), resizes=())` — boot, press keys,
  resize, and return the frame as text
- `capture_frame(app)` — render the current frame inside a scenario
- `snapshot.assert_match(text, name)` — compare against
  `tests/tui/__snapshots__/<name>.txt`

```python
def test_number_key_opens_a_view(run_app):
    async def scenario(pilot):
        await pilot.press("2")
        await pilot.pause()
        return pilot.app.current_view().slug

    assert run_app(scenario) == "run"
```

Re-record snapshots after an intentional layout change:

```sh
CIRCUITRY_SNAPSHOT_UPDATE=1 pytest tests/tui
```

A missing snapshot fails rather than silently recording, so CI cannot pass on
an empty baseline.

Two conventions keep the view suites honest:

- Screens that read the machine (Doctor, Settings) take a
  `DiagnosticsSource`, and screens that read a file (Validate) take a
  validator callable. Tests pass a fixture instead, so an assertion is about
  the view and not about whether the CI box has ffmpeg installed.
- Snapshots are rendered from canned data for the same reason — pinning the
  layout, not the adapter registry, which would otherwise re-record every
  snapshot each time an adapter is added.

The pure data layer behind those views (`circuitry.tui.diagnostics`) imports no
Textual, so its tests live in `tests/test_tui_diagnostics.py` and run in the CI
lanes that install without the `tui` extra and skip `tests/tui` entirely.
