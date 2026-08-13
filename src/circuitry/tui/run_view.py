"""The Run view: pick an orchestration, fill its inputs, launch it.

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

The execution view proper lands in a later story; this screen shows the
minimal running / done / failed signal in its place.
"""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING, Any, ClassVar

from textual.app import ComposeResult
from textual.binding import Binding, BindingType
from textual.containers import Horizontal, Vertical
from textual.message import Message
from textual.widgets import Button, Input, Label, Select, Static

from ..cli.config import CircuitryConfig, resolve_config
from ..cli.runtime_shim import RunRequest, RunResult
from .launch import (
    NO_OVERRIDE,
    InputField,
    OrchestrationChoice,
    OrchestrationForm,
    Runner,
    RunSession,
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
    from circuitry.adapters import Adapter

__all__ = ["RunScreen"]

#: What the status line says in each state, so tests and copy agree.
IDLE = "Idle — pick an orchestration to begin."
RUNNING = "Running…"
CANCELLING = "Cancelling…"
DONE = "Done"
FAILED = "Failed"
CANCELLED = "Cancelled"


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

    RunScreen.-compact .form-label {
        margin-top: 0;
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
        name: str | None = None,
        id: str | None = None,
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

    # -- composition ---------------------------------------------------------

    def compose_body(self) -> ComposeResult:
        yield Static(self.spec.name, classes="view-title")
        yield Static(self.spec.blurb, classes="view-blurb")

        yield Label("Orchestration", classes="form-label")
        yield Select(
            [(choice.option, choice.key) for choice in self._choices],
            prompt="Pick an orchestration",
            id="run-orchestration",
        )

        yield Vertical(id="run-form")

        yield Label("Adapter", classes="form-label")
        yield Select(
            _override_options(adapter_options(self._config)),
            value=NO_OVERRIDE,
            allow_blank=False,
            id="run-adapter",
        )
        yield Label("Model", classes="form-label")
        yield Select(
            _override_options(model_options(self._config)),
            value=NO_OVERRIDE,
            allow_blank=False,
            id="run-model",
        )

        yield Horizontal(
            Button("Launch", variant="primary", id="run-launch"),
            Button("Cancel run", id="run-cancel", disabled=True),
            id="run-actions",
        )
        yield Static(IDLE, id="run-status")

    # -- selection -----------------------------------------------------------

    async def on_select_changed(self, event: Select.Changed) -> None:
        if event.select.id != "run-orchestration":
            return
        event.stop()
        choice = self._choice_for(event.value)
        if choice is None:
            self._form = None
            await self._render_fields(())
            self._set_status(IDLE)
            return
        try:
            form = load_form(choice)
        except Exception as exc:
            self._form = None
            await self._render_fields(())
            self._set_status(f"{FAILED} to load {choice.label}: {exc}", "-failed")
            return
        self._form = form
        await self._render_fields(form.fields)
        self._refresh_model_options(form)
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
        session = RunSession(
            request,
            on_state=self._observe_state,
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
        self.post_message(self.RunStateUpdate(state))

    def _observe_finish(self, result: RunResult) -> None:
        cancelled = self._session is not None and self._session.cancelled
        self.post_message(self.RunFinished(result, cancelled=cancelled))

    def on_run_screen_run_state_update(self, message: RunStateUpdate) -> None:
        message.stop()
        self._updates += 1
        if self._session is not None and self._session.running:
            self._set_status(f"{RUNNING} ({self._updates} state updates)")

    def on_run_screen_run_finished(self, message: RunFinished) -> None:
        message.stop()
        self.last_result = message.result
        self._set_running(False)
        if message.cancelled:
            self._set_status(CANCELLED, "-failed")
        elif message.result.ok:
            self._set_status(f"{DONE} ({self._updates} state updates)", "-done")
        else:
            self._set_status(f"{FAILED}: {message.result.error}", "-failed")

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
    except Exception:
        return CircuitryConfig()


def _safe_choices(root: Path | None) -> list[OrchestrationChoice]:
    try:
        return discover_orchestrations(root)
    except Exception:
        return []
