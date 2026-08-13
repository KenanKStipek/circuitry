"""The Run view: pick an orchestration, fill its inputs, watch it run.

The picker lists local orchestration files and the bundled curation
library. Selecting one reads its ``interface.inputs`` and generates a typed
form — one box per declared input, required ones marked, descriptions used
as hints. Adapter and model dropdowns override resolution for this run
only.

Launching hands a :class:`~circuitry.cli.runtime_shim.RunRequest` to a
:class:`~circuitry.tui.launch.RunSession`, which executes it on a worker
thread and posts messages back here. The UI thread never blocks, and
cancelling unwinds the run through its ordinary error path. Everything
that is not a widget lives in :mod:`circuitry.tui.launch`.

Below the form sits the execution view: a tree mirroring the
orchestration's structure with a status glyph, elapsed and token counts
per effect, and a footer aggregating the run. It is drawn from the plan
the moment an orchestration is picked — so the shape is visible before
launch — and then repainted from the run's events. Repaints are
coalesced onto a timer rather than done per event, so a chatty run
cannot starve the input queue; the model doing the overlaying lives in
:mod:`circuitry.tui.execution`.
"""

from __future__ import annotations

import time
from collections.abc import Callable
from pathlib import Path
from typing import TYPE_CHECKING, Any, ClassVar

from rich.text import Text
from textual.app import ComposeResult
from textual.binding import Binding, BindingType
from textual.containers import Horizontal, Vertical
from textual.message import Message
from textual.widgets import Button, Input, Label, Select, Static

from ..cli.config import CircuitryConfig, resolve_config
from ..cli.runtime_shim import RunRequest, RunResult
from . import execution
from .execution import (
    ExecNode,
    PlanNode,
    RenderLine,
    Totals,
    build_tree,
    format_totals,
    plan_from_orchestration,
    render_lines,
    render_text,
    totals_for,
)
from .inspector import StateStore
from .launch import (
    NO_OVERRIDE,
    InputField,
    OrchestrationChoice,
    OrchestrationForm,
    RunSession,
    Runner,
    adapter_options,
    build_initial_state,
    default_text,
    discover_orchestrations,
    load_form,
    model_options,
    placeholder_for,
)
from .screens import ViewScreen, ViewSpec

if TYPE_CHECKING:  # pragma: no cover - typing only
    from textual.events import Mount

    from circuitry.adapters import Adapter

__all__ = ["RunScreen"]

#: What the status line says in each state, so tests and copy agree.
IDLE = "Idle — pick an orchestration to begin."
RUNNING = "Running…"
CANCELLING = "Cancelling…"
DONE = "Done"
FAILED = "Failed"
CANCELLED = "Cancelled"

#: Placeholder for the tree pane before an orchestration is picked.
NO_TREE = "No orchestration picked — the effect tree appears here."

#: How often the execution view repaints while a run is in flight. Events
#: arrive far faster than a person reads, so they are coalesced onto this
#: tick; anything the user types is handled in the gaps.
REFRESH_SECONDS = 0.1

#: Row colour per effect status. Keyed off :mod:`circuitry.tui.execution`'s
#: names rather than this module's same-spelled run states.
_STATUS_STYLES: dict[str, str] = {
    execution.PENDING: "dim",
    execution.RUNNING: "bold yellow",
    execution.DONE: "green",
    execution.FAILED: "bold red",
    execution.SKIPPED: "dim italic",
}


class RunScreen(ViewScreen):
    """Orchestration picker, generated input form, and launch controls."""

    CSS = """
    RunScreen .form-label {
        color: $text-muted;
        margin-top: 1;
    }

    RunScreen .field-error {
        color: $error;
    }

    RunScreen #run-status {
        margin-top: 1;
        text-style: bold;
    }

    RunScreen #run-status.-failed {
        color: $error;
    }

    RunScreen #run-status.-done {
        color: $success;
    }

    RunScreen #run-actions {
        height: auto;
        margin-top: 1;
    }

    RunScreen #run-panes {
        height: auto;
    }

    RunScreen #run-setup {
        width: 3fr;
        height: auto;
        padding-right: 1;
    }

    RunScreen #run-execution {
        width: 2fr;
        height: auto;
    }

    RunScreen #run-tree {
        height: auto;
        margin-top: 1;
    }

    RunScreen #run-footer {
        height: auto;
        margin-top: 1;
        text-style: bold;
        color: $text-muted;
    }

    RunScreen.-compact .form-label {
        margin-top: 0;
    }

    /* Side by side needs width; below the breakpoint the panes stack. */
    RunScreen.-compact #run-panes {
        layout: vertical;
    }

    RunScreen.-compact #run-setup,
    RunScreen.-compact #run-execution {
        width: 1fr;
        padding-right: 0;
    }
    """

    BINDINGS: ClassVar[list[BindingType]] = [
        Binding("ctrl+r", "launch", "Launch"),
        Binding("ctrl+x", "cancel_run", "Cancel run"),
    ]

    class RunStateUpdate(Message):
        """A state snapshot observed mid-run (already a private deep copy)."""

        def __init__(self, state: dict[str, Any]) -> None:
            super().__init__()
            self.state = state

    class RunEffectComplete(Message):
        """One effect finished writing its node (already a private copy)."""

        def __init__(self, path: str, node: dict[str, Any]) -> None:
            super().__init__()
            self.path = path
            self.node = node

    class RunFinished(Message):
        """The worker thread is done, successfully or not."""

        def __init__(self, result: RunResult, *, cancelled: bool) -> None:
            super().__init__()
            self.result = result
            self.cancelled = cancelled

    def __init__(
        self,
        spec: ViewSpec,
        *,
        config: CircuitryConfig | None = None,
        choices: list[OrchestrationChoice] | None = None,
        adapter: Adapter | None = None,
        runner: Runner | None = None,
        root: Path | None = None,
        clock: Callable[[], float] | None = None,
        name: str | None = None,
        id: str | None = None,  # noqa: A002 - Textual's parameter name
        classes: str | None = None,
    ) -> None:
        super().__init__(spec, name=name, id=id, classes=classes)
        self._config = config if config is not None else _safe_config()
        self._choices: list[OrchestrationChoice] = (
            choices if choices is not None else _safe_choices(root)
        )
        # Injection points for tests and embedders: a prebuilt adapter to
        # run against, and a stand-in for runtime_shim.run.
        self._adapter = adapter
        self._runner = runner
        self._form: OrchestrationForm | None = None
        self._fields: list[tuple[InputField, Input]] = []
        self._session: RunSession | None = None
        self._updates = 0
        self._status_text = IDLE
        #: Last finished result, for tests and for the execution view to pick up.
        self.last_result: RunResult | None = None

        # -- execution view state ------------------------------------------
        # ``_clock`` is injectable so a test can pin the wall-clock reading
        # the footer shows.
        self._clock: Callable[[], float] = clock if clock is not None else time.monotonic
        self._plan: tuple[PlanNode, ...] = ()
        self._run_state: dict[str, Any] = {}
        self._completed: set[str] = set()
        self._exec_nodes: tuple[ExecNode, ...] = ()
        self._totals = Totals()
        self._started_at: float | None = None
        self._final_elapsed: float | None = None
        self._in_flight = False
        self._repaint: Any = None
        #: The app's state store, captured on mount so a run can keep
        #: publishing into it after this screen is replaced.
        self._store: StateStore | None = None

    def _on_mount(self, event: Mount) -> None:
        super()._on_mount(event)
        store = getattr(self.app, "run_states", None)
        self._store = store if isinstance(store, StateStore) else None

    # -- composition ---------------------------------------------------------

    def compose_body(self) -> ComposeResult:
        yield Static(self.spec.name, classes="view-title")
        yield Static(self.spec.blurb, classes="view-blurb")
        # Setup on the left, the run itself on the right — so launching
        # does not scroll the controls away, and watching does not hide
        # them. Below the breakpoint the two panes stack.
        yield Horizontal(
            Vertical(*self._setup_pane(), id="run-setup"),
            Vertical(*self._execution_pane(), id="run-execution"),
            id="run-panes",
        )

    def _setup_pane(self) -> list[Any]:
        return [
            Label("Orchestration", classes="form-label"),
            Select(
                [(choice.option, choice.key) for choice in self._choices],
                prompt="Pick an orchestration",
                id="run-orchestration",
            ),
            Vertical(id="run-form"),
            Label("Adapter", classes="form-label"),
            Select(
                _override_options(adapter_options(self._config)),
                value=NO_OVERRIDE,
                allow_blank=False,
                id="run-adapter",
            ),
            Label("Model", classes="form-label"),
            Select(
                _override_options(model_options(self._config)),
                value=NO_OVERRIDE,
                allow_blank=False,
                id="run-model",
            ),
            Horizontal(
                Button("Launch", variant="primary", id="run-launch"),
                Button("Cancel run", id="run-cancel", disabled=True),
                id="run-actions",
            ),
            Static(IDLE, id="run-status"),
        ]

    def _execution_pane(self) -> list[Any]:
        return [
            Static(NO_TREE, id="run-tree"),
            Static(format_totals(Totals()), id="run-footer"),
        ]

    # -- selection -----------------------------------------------------------

    async def on_select_changed(self, event: Select.Changed) -> None:
        if event.select.id != "run-orchestration":
            return
        event.stop()
        choice = self._choice_for(event.value)
        if choice is None:
            self._form = None
            await self._render_fields(())
            self._reset_execution(())
            self._set_status(IDLE)
            return
        try:
            form = load_form(choice)
        except Exception as exc:  # noqa: BLE001 - any load failure is user-facing
            self._form = None
            await self._render_fields(())
            self._reset_execution(())
            self._set_status(f"{FAILED} to load {choice.label}: {exc}", "-failed")
            return
        self._form = form
        await self._render_fields(form.fields)
        self._refresh_model_options(form)
        # Draw the plan straight away: the shape of the run is worth seeing
        # before committing to it.
        self._reset_execution(plan_from_orchestration(form.orchestration))
        self._set_status(_ready_message(form))

    def _choice_for(self, value: Any) -> OrchestrationChoice | None:
        for choice in self._choices:
            if choice.key == value:
                return choice
        return None

    async def _render_fields(self, fields: tuple[InputField, ...]) -> None:
        """Replace the form body with one labelled box per declared input."""
        container = self.query_one("#run-form", Vertical)
        await container.remove_children()
        self._fields = []
        if not fields:
            note = "No declared inputs — launch runs it as-is."
            if self._form is not None:
                await container.mount(Static(note, classes="view-note"))
            return
        for index, spec in enumerate(fields):
            box = Input(
                value=default_text(spec),
                placeholder=placeholder_for(spec),
                id=f"run-field-{index}",
            )
            await container.mount(
                Label(spec.label, classes="form-label"),
                box,
                Static("", id=f"run-error-{index}", classes="field-error"),
            )
            self._fields.append((spec, box))

    def _refresh_model_options(self, form: OrchestrationForm) -> None:
        """Fold the orchestration's own model into the dropdown."""
        select: Select[str] = self.query_one("#run-model", Select)
        current = select.value
        options = _override_options(model_options(self._config, form.orchestration))
        select.set_options(options)
        values = {value for _, value in options}
        select.value = current if current in values else NO_OVERRIDE

    # -- launching -----------------------------------------------------------

    def on_button_pressed(self, event: Button.Pressed) -> None:
        if event.button.id == "run-launch":
            event.stop()
            self.action_launch()
        elif event.button.id == "run-cancel":
            event.stop()
            self.action_cancel_run()

    def on_input_submitted(self, event: Input.Submitted) -> None:
        """Enter walks the form — Tab belongs to view navigation."""
        event.stop()
        boxes = [box for _, box in self._fields]
        if event.input in boxes:
            index = boxes.index(event.input)
            if index + 1 < len(boxes):
                boxes[index + 1].focus()
                return
        self.query_one("#run-launch", Button).focus()

    def action_launch(self) -> None:
        """Validate the form and start the run on a worker thread."""
        if self._session is not None and self._session.running:
            return
        if self._form is None:
            self._set_status("Pick an orchestration first.", "-failed")
            return

        raw = {spec.name: box.value for spec, box in self._fields}
        initial_state, errors = build_initial_state(
            (spec for spec, _ in self._fields), raw
        )
        self._show_errors(errors)
        if errors:
            self._set_status(
                f"{len(errors)} input{'s' if len(errors) > 1 else ''} need attention.",
                "-failed",
            )
            return

        request = RunRequest(
            orchestration_path=self._form.choice.path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state=initial_state,
            config=self._config,
            adapter=self._adapter,
            adapter_override=_override_value(self.query_one("#run-adapter", Select)),
            model_override=_override_value(self.query_one("#run-model", Select)),
            skip_preflight=False,
        )

        self._updates = 0
        self.last_result = None
        self._begin_execution()
        session = RunSession(
            request,
            on_state=self._observe_state,
            on_effect=self._observe_effect,
            on_finish=self._observe_finish,
            runner=self._runner,
        )
        self._session = session
        self._set_running(True)
        self._set_status(RUNNING)
        session.start()

    def action_cancel_run(self) -> None:
        """Ask the in-flight run to stop; a no-op when nothing is running."""
        session = self._session
        if session is None or not session.running:
            return
        session.cancel()
        self._set_status(CANCELLING)

    # -- worker-thread callbacks --------------------------------------------
    # Both run off the UI thread. ``post_message`` is thread-safe; touching
    # widgets from here would not be.

    def _observe_state(self, state: dict[str, Any]) -> None:
        # Published to the app's store *here* rather than from the message
        # handler: the store is what the Runs view inspects, and a run
        # outlives the screen that launched it — once the user navigates
        # away this screen stops receiving messages, but the run goes on.
        if self._store is not None:
            self._store.publish(state)
        self.post_message(self.RunStateUpdate(state))

    def _observe_effect(self, path: str, node: dict[str, Any]) -> None:
        self.post_message(self.RunEffectComplete(path, node))

    def _observe_finish(self, result: RunResult) -> None:
        cancelled = self._session is not None and self._session.cancelled
        if self._store is not None:
            self._store.finish(result.state if result.ok else None)
        self.post_message(self.RunFinished(result, cancelled=cancelled))

    def on_run_screen_run_state_update(self, message: RunStateUpdate) -> None:
        message.stop()
        self._updates += 1
        # Recorded, not drawn: the repaint timer picks this up on its next
        # tick, so a burst of writes costs one render, not one each.
        self._run_state = message.state
        if self._session is not None and self._session.running:
            self._set_status(f"{RUNNING} ({self._updates} state updates)")

    def on_run_screen_run_effect_complete(self, message: RunEffectComplete) -> None:
        message.stop()
        # Parallel siblings merge their state back only once the last one
        # lands; this is what lets the tree mark them off as they finish.
        self._completed.add(message.path)

    def on_run_screen_run_finished(self, message: RunFinished) -> None:
        message.stop()
        self.last_result = message.result
        self._set_running(False)
        self._end_execution(message.result)
        if message.cancelled:
            self._set_status(CANCELLED, "-failed")
        elif message.result.ok:
            self._set_status(f"{DONE} ({self._updates} state updates)", "-done")
        else:
            self._set_status(f"{FAILED}: {message.result.error}", "-failed")

    # -- the execution view --------------------------------------------------

    def _reset_execution(self, plan: tuple[PlanNode, ...]) -> None:
        """Adopt a new plan and draw it as a wholly pending run."""
        self._plan = plan
        self._run_state = {}
        self._completed = set()
        self._started_at = None
        self._final_elapsed = None
        self._in_flight = False
        self._paint()

    def _begin_execution(self) -> None:
        """Start the clock and the repaint tick for a launch."""
        self._run_state = {}
        self._completed = set()
        self._started_at = self._clock()
        self._final_elapsed = None
        self._in_flight = True
        if self._store is not None:
            self._store.begin(label=self._form.choice.label if self._form else "")
        self._paint()
        self.reveal_execution()
        if self._repaint is None:
            self._repaint = self.set_interval(REFRESH_SECONDS, self._paint)

    def _end_execution(self, result: RunResult) -> None:
        """Freeze the wall clock and draw the run's final state."""
        self._in_flight = False
        if self._repaint is not None:
            self._repaint.stop()
            self._repaint = None
        if self._started_at is not None:
            self._final_elapsed = max(self._clock() - self._started_at, 0.0)
        if isinstance(result.state, dict) and result.state:
            self._run_state = result.state
        self._paint()

    def _paint(self) -> None:
        """Rebuild the tree and footer from whatever is known right now."""
        self._exec_nodes = build_tree(
            self._plan,
            self._run_state,
            completed=self._completed,
            # The session is the authority on whether work is in flight:
            # the first snapshot lands only once the first effect does.
            running=True if self._in_flight else None,
        )
        self._totals = totals_for(
            self._exec_nodes, self._run_state, elapsed=self._elapsed()
        )
        if not self.is_mounted:
            # A run outlives the screen when the user navigates away; the
            # model still tracks it, there is just nothing to draw on.
            return
        lines = render_lines(self._exec_nodes)
        tree = self.query_one("#run-tree", Static)
        tree.update(_tree_text(lines) if lines else Text(NO_TREE, style="dim"))
        self.query_one("#run-footer", Static).update(format_totals(self._totals))

    def _elapsed(self) -> float | None:
        if self._final_elapsed is not None:
            return self._final_elapsed
        if self._started_at is None:
            return None
        return max(self._clock() - self._started_at, 0.0)

    @property
    def execution_nodes(self) -> tuple[ExecNode, ...]:
        """The tree as last drawn (used by tests)."""
        return self._exec_nodes

    @property
    def totals(self) -> Totals:
        """The footer's aggregate numbers as last drawn (used by tests)."""
        return self._totals

    @property
    def tree_text(self) -> str:
        """The tree pane's rows as plain text, outliving the widget."""
        return render_text(self._exec_nodes) if self._exec_nodes else NO_TREE

    @property
    def footer_text(self) -> str:
        """The footer bar as plain text, outliving the widget."""
        return format_totals(self._totals)

    def reveal_execution(self) -> None:
        """Scroll the tree into view — a launch is a request to watch it.

        Done once per launch rather than on every repaint, so a user who
        scrolls back up to the form is left alone.
        """
        try:
            self.query_one("#run-tree", Static).scroll_visible(top=True, animate=False)
        except Exception:  # noqa: BLE001 - a scroll is never worth an error
            return

    def show_state(self, state: dict[str, Any], *, elapsed: float | None = None) -> None:
        """Apply a state snapshot directly, without a run behind it.

        The scripted entry point: snapshot tests and embedders drive the
        execution view with a state they wrote themselves, pinning the
        wall clock rather than reading it.
        """
        self._run_state = state
        if elapsed is not None:
            self._final_elapsed = elapsed
        self._paint()

    # -- small helpers -------------------------------------------------------

    def _show_errors(self, errors: dict[str, str]) -> None:
        for index, (spec, _) in enumerate(self._fields):
            self.query_one(f"#run-error-{index}", Static).update(
                errors.get(spec.name, "")
            )

    def _set_running(self, running: bool) -> None:
        self.query_one("#run-launch", Button).disabled = running
        self.query_one("#run-cancel", Button).disabled = not running

    def _set_status(self, text: str, state_class: str = "") -> None:
        status = self.query_one("#run-status", Static)
        status.remove_class("-failed", "-done")
        if state_class:
            status.add_class(state_class)
        status.update(text)
        self._status_text = text

    @property
    def status_text(self) -> str:
        """The status line as plain text (used by tests)."""
        return self._status_text


def _tree_text(lines: list[RenderLine]) -> Text:
    """Colour each tree row by the status of the effect it describes."""
    text = Text()
    for index, line in enumerate(lines):
        if index:
            text.append("\n")
        text.append(line.text, style=_STATUS_STYLES.get(line.status, ""))
    return text


def _override_options(values: list[str]) -> list[tuple[str, str]]:
    """Dropdown options with the "leave it alone" sentinel first."""
    return [(f"{NO_OVERRIDE} default", NO_OVERRIDE), *((value, value) for value in values)]


def _override_value(select: Select[str]) -> str | None:
    """``None`` when the dropdown is on the sentinel, else the chosen value."""
    value = select.value
    if value in (NO_OVERRIDE, Select.BLANK) or not isinstance(value, str):
        return None
    return value


def _ready_message(form: OrchestrationForm) -> str:
    required = sum(1 for spec in form.fields if spec.required)
    if not form.fields:
        return f"{form.choice.label} — no inputs declared."
    return (
        f"{form.choice.label} — {len(form.fields)} input"
        f"{'s' if len(form.fields) != 1 else ''}"
        f" ({required} required)."
    )


def _safe_config() -> CircuitryConfig:
    """Resolved config, falling back to defaults rather than failing to open."""
    try:
        return resolve_config()
    except Exception:  # noqa: BLE001 - a bad config file must not kill the view
        return CircuitryConfig()


def _safe_choices(root: Path | None) -> list[OrchestrationChoice]:
    try:
        return discover_orchestrations(root)
    except Exception:  # noqa: BLE001 - discovery touches the filesystem
        return []
