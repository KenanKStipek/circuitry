# Terminal UI — app chrome

The TUI is the Textual app behind `cof tui` (and a bare `cof` on a TTY). It
ships in the optional `tui` extra:

```sh
pip install "circuitry-cof[tui]"
```

This page documents the shell — navigation, keys, layout breakpoints, logging
and the test harness. Individual views (Library, Run, Inspect, Runs, Doctor,
Settings) plug into it and are documented as they land.

## Keymap

| Key | Action |
| --- | --- |
| `1`–`6` | Jump straight to a view, in registry order |
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

## Run view

`2` opens the launcher. Three steps, top to bottom:

1. **Pick** — the dropdown lists orchestration files found next to you (the
   working directory and `./orchestrations/`, one level deep, filtered to files
   that actually declare `effects`) followed by the bundled curation library.
2. **Fill** — selecting an orchestration reads its `interface.inputs` and
   generates one box per declared input. Required inputs are marked with `*`,
   the declared `type` is shown, `description` becomes the placeholder, and a
   declared `default` is prefilled. Values are parsed to their declared type
   before launch — `number` and `boolean` accept the obvious spellings,
   `array` and `object` take JSON. Failures are reported under the offending
   field and nothing is launched. A blank optional field falls back to its
   default, or is left out of `initial_state` entirely.
3. **Launch** — `Ctrl-R` or the button. The adapter and model dropdowns
   override resolution for this run only (they map to
   `RunRequest.adapter_override` / `model_override`, which rank above the
   orchestration's own `adapter`/`model`); left on `—` they change nothing.
   Adapter options come from the adapters you have configured, minus
   `host_claude`, which can only be injected at runtime.

`Tab` belongs to view navigation, so `Enter` is what walks the form: it moves
to the next field and, from the last one, lands on Launch.

The run itself executes `runtime_shim.run` on a worker thread, so the UI never
blocks. Its `state_observer` is pure — it deep-copies each snapshot before
handing it to the UI and never writes back, so a run driven from the TUI ends
in exactly the state a plain `cof run` produces (there is a fixture that
asserts precisely that). Snapshots and the final result come back as Textual
messages posted from the worker thread.

`Ctrl-X` (or the Cancel button) requests cancellation: the observer raises on
the next state write, the runtime unwinds through its ordinary error path, and
the result is a normal failed `RunResult`. Nothing is left half-torn-down.

The status line carries the whole signal for now — `Running…`, `Done`,
`Failed: …`, `Cancelled`. The execution view that renders effects as they
stream lands in the next story; `RunScreen.last_result` is where it picks up.

Logic that is not a widget lives in `circuitry/tui/launch.py` (discovery, the
typed form, override options, `RunSession`) and is tested without booting an
app.

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
