"""The Profile view — build and switch named profiles without YAML surgery.

Pick an orchestration and it is compiled through the validate-only path; the
tree that comes back is rendered one row per effect, each with a
provider/model picker and an enabled toggle. Around it sit the three other
things a profile can say: run-level adapter/model defaults, initial-state
inputs, and the persistence target.

Editing is deliberately dumb: every widget writes straight into a
:class:`~circuitry.tui.profile_edit.ProfileDraft`, and the draft is the only
thing that knows how to become a file. So what the editor saves is a plain
profile document — the engine's loader reads it back with no notion that a
TUI wrote it.

Two refusals are worth calling out, because both are the *engine's* rules
rather than the view's:

* A conditional's ``if`` and a loop's ``while`` cannot carry an override.
  Their rows render (the tree should match the file you are looking at) but
  the toggle springs back and the status line quotes
  :func:`~circuitry.cli.profiles.condition_target_message` verbatim.
* Overrides naming effects the orchestration no longer defines are listed as
  orphans with a one-key cleanup, rather than failing the load.

Everything that is not a widget lives in :mod:`circuitry.tui.profile_edit`.
"""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING, Any, ClassVar, Optional

from textual.app import ComposeResult
from textual.binding import Binding, BindingType
from textual.containers import Horizontal, Vertical
from textual.screen import ModalScreen
from textual.widgets import Button, Input, Label, Select, Static, Switch

from ..cli.config import CircuitryConfig, resolve_config
from ..cli.orchestration_loader import load_orchestration_file
from ..core.compiler import compile_orchestration
from .launch import (
    InputField,
    OrchestrationChoice,
    adapter_options,
    build_initial_state,
    discover_orchestrations,
    input_fields,
    model_options,
    placeholder_for,
)
from .layout import ResponsiveLayout
from .profile_edit import (
    BACKENDS,
    CUSTOM,
    DEFAULT_PROFILE_NAME,
    NO_OVERRIDE,
    BackendField,
    EffectNode,
    EffectOverride,
    PersistenceDraft,
    ProfileDraft,
    backend_by_name,
    build_effect_tree,
    condition_refusal,
    discover_profiles,
    load_draft,
    profile_dir_for,
)
from .screens import ViewScreen, ViewSpec

if TYPE_CHECKING:  # pragma: no cover - typing only
    from textual.events import Mount

__all__ = ["ConfirmDiscard", "ProfileScreen"]

#: Picker option that starts a profile from scratch.
NEW_PROFILE = "+ new profile"

#: Persistence picker option meaning "this profile says nothing about it".
NO_PERSISTENCE = "— leave it to config"

EMPTY_STATE = "Pick an orchestration to edit a profile for it."
NO_EFFECTS = "This orchestration declares no named effects to override."
DIRTY_MARK = "● unsaved"
CLEAN_MARK = "saved"


class ConfirmDiscard(ResponsiveLayout, ModalScreen[bool]):
    """Gate in front of walking away from unsaved profile edits."""

    BINDINGS: ClassVar[list[BindingType]] = [
        Binding("y", "confirm", "Discard"),
        Binding("enter", "confirm", "Discard", show=False),
        Binding("n", "cancel", "Keep editing"),
        Binding("escape", "cancel", "Keep editing", show=False),
        Binding("q", "cancel", "Keep editing", show=False),
    ]

    DEFAULT_CSS = """
    ConfirmDiscard { align: center middle; }

    ConfirmDiscard #discard-dialog {
        width: 80%;
        max-width: 56;
        height: auto;
        padding: 1 2;
        border: round $warning;
        background: $surface;
    }

    ConfirmDiscard.-compact #discard-dialog {
        width: 100%;
        max-width: 100%;
        padding: 0 1;
    }

    ConfirmDiscard.-tiny #discard-dialog { padding: 0; border: none; }
    ConfirmDiscard #discard-title { text-style: bold; color: $warning; }
    ConfirmDiscard #discard-hint { color: $text-muted; }
    ConfirmDiscard.-tiny #discard-title { display: none; }
    """

    def __init__(self, profile_name: str) -> None:
        super().__init__(id="confirm-discard")
        self.profile_name = profile_name

    def compose(self) -> ComposeResult:
        with Vertical(id="discard-dialog"):
            yield Static("Unsaved profile changes", id="discard-title")
            yield Static(
                f"{self.profile_name} has edits that are not on disk.",
                id="discard-body",
                markup=False,
            )
            yield Static("y discard · n keep editing", id="discard-hint")

    def action_confirm(self) -> None:
        self.dismiss(True)

    def action_cancel(self) -> None:
        self.dismiss(False)


class ProfileScreen(ViewScreen):
    """Effect-tree pickers, panels, and named-profile save/switch."""

    CSS = """
    ProfileScreen .form-label { color: $text-muted; margin-top: 1; }
    ProfileScreen .panel-heading { text-style: bold; color: $accent; margin-top: 1; }
    ProfileScreen .panel-note { color: $text-muted; }

    ProfileScreen .effect-row { height: auto; }
    ProfileScreen .effect-row Static { width: 1fr; }
    ProfileScreen .effect-row Select { width: 22; }
    ProfileScreen .effect-row Input { width: 22; }
    ProfileScreen .row-condition Static { color: $text-muted; }
    ProfileScreen .row-reflector Static { color: $warning; }
    ProfileScreen .row-group { color: $text-muted; }

    ProfileScreen #profile-status { margin-top: 1; text-style: bold; }
    ProfileScreen #profile-status.-failed { color: $error; }
    ProfileScreen #profile-status.-done { color: $success; }
    ProfileScreen #profile-dirty { color: $warning; }
    ProfileScreen #profile-actions { height: auto; margin-top: 1; }
    ProfileScreen #profile-orphans { height: auto; }
    ProfileScreen .orphan-row { color: $error; }

    ProfileScreen.-compact .form-label { margin-top: 0; }
    ProfileScreen.-compact .panel-heading { margin-top: 0; }
    """

    BINDINGS: ClassVar[list[BindingType]] = [
        Binding("ctrl+s", "save", "Save profile"),
        Binding("ctrl+d", "save_as", "Save as / duplicate"),
        Binding("ctrl+o", "drop_orphans", "Drop orphans"),
        Binding("ctrl+r", "run_with_profile", "Run with profile"),
        Binding("q", "leave", "Back / Quit", show=False),
        Binding("escape", "leave", "Back / Quit", show=False),
    ]

    def __init__(
        self,
        spec: ViewSpec,
        *,
        config: CircuitryConfig | None = None,
        choices: list[OrchestrationChoice] | None = None,
        root: Path | None = None,
        cwd: Path | None = None,
        name: str | None = None,
        id: str | None = None,  # noqa: A002 - Textual's parameter name
        classes: str | None = None,
    ) -> None:
        super().__init__(spec, name=name, id=id, classes=classes)
        self._config = config if config is not None else _safe_config()
        self._choices: list[OrchestrationChoice] = (
            choices if choices is not None else _safe_choices(root)
        )
        self._cwd = cwd
        self._choice: OrchestrationChoice | None = None
        self._orch: dict[str, Any] = {}
        self._tree: list[EffectNode] = []
        #: Rows in render order; index is the suffix of every row widget id.
        self._rows: list[EffectNode] = []
        self._input_fields: list[InputField] = []
        self._backend_fields: list[BackendField] = []
        self.draft = ProfileDraft()
        self._status_text = EMPTY_STATE
        #: Set while widgets are being repopulated, so the change handlers
        #: do not mistake our own writes for the user's.
        self._loading = False

    # -- composition ---------------------------------------------------------

    def compose_body(self) -> ComposeResult:
        yield Static(self.spec.name, classes="view-title")
        yield Static(self.spec.blurb, classes="view-blurb")

        yield Label("Orchestration", classes="form-label")
        yield Select(
            [(choice.option, choice.key) for choice in self._choices],
            prompt="Pick an orchestration",
            id="profile-orchestration",
        )

        yield Label("Profile", classes="form-label")
        yield Select(
            [(NEW_PROFILE, NEW_PROFILE)],
            value=NEW_PROFILE,
            allow_blank=False,
            id="profile-picker",
        )
        yield Label("Name", classes="form-label")
        yield Input(value=self.draft.name, id="profile-name")

        # The pickers that always exist are composed once and updated in
        # place. Only the panels whose *shape* follows the selected
        # orchestration (inputs, effect rows, backend fields) are remounted.
        yield Static("Run defaults", classes="panel-heading")
        yield Label("Adapter", classes="form-label")
        yield _picker("profile-adapter", [], None)
        yield _custom_box("profile-adapter-custom", "adapter name")
        yield Label("Model", classes="form-label")
        yield _picker("profile-model", [], None)
        yield _custom_box("profile-model-custom", "model name")

        yield Static("Inputs", classes="panel-heading")
        yield Vertical(id="profile-inputs")

        yield Static("Effects", classes="panel-heading")
        yield Vertical(id="profile-effects")

        yield Static("Persistence", classes="panel-heading")
        yield Select(
            [
                (NO_PERSISTENCE, NO_PERSISTENCE),
                *((f"{spec.name} — {spec.blurb}", spec.name) for spec in BACKENDS),
            ],
            value=NO_PERSISTENCE,
            allow_blank=False,
            id="persistence-backend",
        )
        yield Vertical(id="profile-persistence")

        yield Vertical(id="profile-orphans")

        yield Horizontal(
            Button("Save", variant="primary", id="profile-save"),
            Button("Save as…", id="profile-save-as"),
            Button("Run with this profile", id="profile-run"),
            id="profile-actions",
        )
        yield Static(EMPTY_STATE, id="profile-status", markup=False)
        yield Static(CLEAN_MARK, id="profile-dirty")

    def _on_mount(self, event: Mount) -> None:
        super()._on_mount(event)
        self._sync_defaults()
        self._refresh_dirty()

    # -- orchestration selection ---------------------------------------------

    async def on_select_changed(self, event: Select.Changed) -> None:
        select_id = event.select.id or ""
        if select_id == "profile-orchestration":
            event.stop()
            await self._select_orchestration(event.value)
        elif select_id == "profile-picker":
            event.stop()
            await self._select_profile(event.value)
        elif select_id == "profile-adapter":
            event.stop()
            self._on_default_changed("adapter", event.value)
        elif select_id == "profile-model":
            event.stop()
            self._on_default_changed("model", event.value)
        elif select_id == "persistence-backend":
            event.stop()
            await self._select_backend(event.value)
        elif select_id.startswith("fx-provider-"):
            event.stop()
            self._on_row_select(event.select, "provider", event.value)
        elif select_id.startswith("fx-model-"):
            event.stop()
            self._on_row_select(event.select, "model", event.value)

    async def _select_orchestration(self, value: Any) -> None:
        choice = next((c for c in self._choices if c.key == value), None)
        if choice is None:
            self._choice = None
            self._orch = {}
            await self._render_tree([])
            await self._render_inputs([])
            self._set_status(EMPTY_STATE)
            return
        try:
            orch = load_orchestration_file(choice.path)
            # The validate-only path: if it does not compile there is no
            # effect tree to pick models for, and saying so beats rendering
            # a tree built from a file the engine would reject.
            compile_orchestration(orch=orch)
        except Exception as exc:  # noqa: BLE001 - any load/compile failure is user-facing
            self._choice = None
            self._orch = {}
            await self._render_tree([])
            await self._render_inputs([])
            self._set_status(f"{choice.label} does not compile: {exc}", "-failed")
            return

        self._choice = choice
        self._orch = orch
        self._tree = build_effect_tree(orch)
        self._refresh_profile_picker()
        self._sync_defaults()
        await self._render_inputs(input_fields(orch))
        await self._render_tree(self._tree)
        await self._render_orphans()
        self._set_status(_ready_message(choice, self._tree))

    def _refresh_profile_picker(self) -> None:
        """List the profiles this orchestration can already see."""
        picker: Select[str] = self.query_one("#profile-picker", Select)
        names = (
            [name for name, _ in discover_profiles(self._choice.path, cwd=self._cwd)]
            if self._choice is not None
            else []
        )
        options = [(NEW_PROFILE, NEW_PROFILE), *((name, name) for name in names)]
        self._loading = True
        try:
            picker.set_options(options)
            picker.value = self.draft.name if self.draft.name in names else NEW_PROFILE
        finally:
            self._loading = False

    # -- profile switching ---------------------------------------------------

    async def _select_profile(self, value: Any) -> None:
        if self._loading or self._choice is None:
            return
        if value == NEW_PROFILE:
            await self._adopt(ProfileDraft())
            self._set_status("New profile — nothing overridden yet.")
            return
        name = str(value)
        try:
            draft = load_draft(name, orchestration_path=self._choice.path, cwd=self._cwd)
        except Exception as exc:  # noqa: BLE001 - a bad file must not kill the view
            self._set_status(f"Could not open profile {name!r}: {exc}", "-failed")
            return
        await self._adopt(draft)
        orphans = self.draft.orphans(self._tree)
        if orphans:
            self._set_status(
                f"{name} loaded — {len(orphans)} override"
                f"{'s' if len(orphans) != 1 else ''} no longer exist "
                "(Ctrl-O drops them).",
                "-failed",
            )
        else:
            self._set_status(f"{name} loaded.")

    async def _adopt(self, draft: ProfileDraft) -> None:
        """Make ``draft`` the working copy and repaint every panel from it."""
        self.draft = draft
        self._loading = True
        try:
            self.query_one("#profile-name", Input).value = draft.name
        finally:
            self._loading = False
        self._sync_defaults()
        await self._render_inputs(self._input_fields)
        await self._render_tree(self._tree)
        await self._render_persistence()
        await self._render_orphans()
        self._refresh_dirty()

    # -- run defaults panel ---------------------------------------------------

    def _sync_defaults(self) -> None:
        """Repoint the adapter/model pickers at the draft and this orchestration."""
        self._loading = True
        try:
            _set_picker(
                self.query_one("#profile-adapter", Select),
                adapter_options(self._config),
                self.draft.adapter,
            )
            _set_picker(
                self.query_one("#profile-model", Select),
                model_options(self._config, self._orch or None),
                self.draft.model,
            )
            backend = self.query_one("#persistence-backend", Select)
            backend.value = (
                self.draft.persistence.backend
                if self.draft.persistence is not None
                else NO_PERSISTENCE
            )
        finally:
            self._loading = False

    def _on_default_changed(self, kind: str, value: Any) -> None:
        resolved = self._resolve_picker(f"profile-{kind}-custom", value)
        setattr(self.draft, kind, resolved)
        self._refresh_dirty()

    def _resolve_picker(self, custom_id: str, value: Any) -> str | None:
        """Turn a picker value into what the profile should say.

        ``custom…`` reveals the free-text box next to it and hands the field
        over to whatever is typed there; anything else hides the box again.
        """
        box = self.query_one(f"#{custom_id}", Input)
        if value == CUSTOM:
            box.display = True
            box.focus()
            return box.value.strip() or None
        box.display = False
        if box.value:
            self._loading = True
            try:
                box.value = ""
            finally:
                self._loading = False
        return None if value in (NO_OVERRIDE, Select.BLANK) else str(value)

    # -- inputs panel ---------------------------------------------------------

    async def _render_inputs(self, fields: list[InputField]) -> None:
        panel = self.query_one("#profile-inputs", Vertical)
        await panel.remove_children()
        self._input_fields = list(fields)
        if not fields:
            await panel.mount(
                Static(
                    "This orchestration declares no inputs.", classes="panel-note"
                )
            )
            return
        self._loading = True
        try:
            for index, spec in enumerate(fields):
                await panel.mount(
                    Label(spec.label, classes="form-label"),
                    Input(
                        value=_input_text(self.draft.inputs.get(spec.name)),
                        placeholder=placeholder_for(spec),
                        id=f"pin-{index}",
                    ),
                )
        finally:
            self._loading = False

    def _collect_inputs(self) -> dict[str, str]:
        """Whatever the input boxes currently say, keyed by input name."""
        raw: dict[str, str] = {}
        for index, spec in enumerate(self._input_fields):
            try:
                box = self.query_one(f"#pin-{index}", Input)
            except Exception:  # noqa: BLE001 - panel is mid-rebuild
                continue
            raw[spec.name] = box.value
        return raw

    def _sync_inputs(self) -> list[str]:
        """Re-derive ``draft.inputs`` from the boxes; returns type errors."""
        state, errors = build_initial_state(self._input_fields, self._collect_inputs())
        # Only what was actually typed belongs in the profile: an untouched
        # optional field must not bake its declared default into the file.
        typed = {name for name, text in self._collect_inputs().items() if text.strip()}
        self.draft.inputs = {k: v for k, v in state.items() if k in typed}
        return [message for name, message in errors.items() if name in typed]

    # -- effect tree ----------------------------------------------------------

    async def _render_tree(self, tree: list[EffectNode]) -> None:
        panel = self.query_one("#profile-effects", Vertical)
        await panel.remove_children()
        self._rows = []
        if not tree:
            await panel.mount(Static(NO_EFFECTS, classes="panel-note"))
            return

        providers = adapter_options(self._config)
        models = model_options(self._config, self._orch or None)
        self._loading = True
        try:
            for node in tree:
                index = len(self._rows)
                self._rows.append(node)
                if node.kind == "group":
                    await panel.mount(
                        Static(node.row_label(), classes="row-group", markup=False)
                    )
                    continue
                override = self.draft.override(node.path)
                row_class = "row-condition" if node.kind == "condition" else ""
                if node.is_reflector:
                    row_class = "row-reflector"
                await panel.mount(
                    Horizontal(
                        Static(node.row_label(), markup=False),
                        *_row_controls(index, node, override, providers, models),
                        classes=f"effect-row {row_class}".strip(),
                        id=f"fx-row-{index}",
                    )
                )
        finally:
            self._loading = False

    def _on_row_select(self, select: Select[Any], kind: str, value: Any) -> None:
        node = self._row_of(select)
        if node is None:
            return
        index = _index_of(select.id or "")
        if kind == "provider":
            resolved = None if value in (NO_OVERRIDE, Select.BLANK) else str(value)
            self.draft.set_provider(node.path, resolved)
        else:
            self.draft.set_model(
                node.path, self._resolve_picker(f"fx-custom-{index}", value)
            )
        self._refresh_dirty()

    def on_switch_changed(self, event: Switch.Changed) -> None:
        widget_id = event.switch.id or ""
        if not widget_id.startswith("fx-enabled-"):
            return
        event.stop()
        node = self._row_of(event.switch)
        if node is None:
            return
        if node.kind == "condition":
            # The engine's rule, in the engine's words — springing the switch
            # back is the only part of this the view decides.
            self._reset_switch(event.switch, True)
            self._set_status(
                condition_refusal(node.path, profile_name=self.draft.name), "-failed"
            )
            return
        self.draft.set_enabled(node.path, event.value)
        self._refresh_dirty()
        if not event.value and node.is_container:
            self._set_status(
                f"{node.path} disabled — its whole subtree is skipped for this run."
            )

    def _reset_switch(self, switch: Switch, value: bool) -> None:
        self._loading = True
        try:
            switch.value = value
        finally:
            self._loading = False

    def _row_of(self, widget: Any) -> EffectNode | None:
        """The tree row a row widget belongs to, or ``None`` if it is stale.

        Rebuilding the tree remounts every row, and Textual can still deliver
        a ``Changed`` a departing widget posted on its way out. Checking that
        the widget is still mounted keeps such an echo from writing an
        override onto whatever row now holds its index.
        """
        if not widget.is_mounted:
            return None
        index = _index_of(widget.id or "")
        if 0 <= index < len(self._rows):
            return self._rows[index]
        return None

    # -- persistence panel -----------------------------------------------------

    async def _render_persistence(self) -> None:
        """One labelled box per key the selected backend reads."""
        panel = self.query_one("#profile-persistence", Vertical)
        await panel.remove_children()
        current = self.draft.persistence
        self._backend_fields = _backend_fields(current)
        if current is None:
            await panel.mount(
                Static(
                    "This profile leaves persistence to the orchestration or "
                    "config.",
                    classes="panel-note",
                )
            )
            return
        self._loading = True
        try:
            for spec_field in self._backend_fields:
                await panel.mount(
                    Label(
                        f"{spec_field.label}{' *' if spec_field.required else ''}",
                        classes="form-label",
                    ),
                    Input(
                        value=current.values.get(spec_field.key, ""),
                        placeholder=spec_field.placeholder,
                        id=f"pers-{spec_field.key}",
                    ),
                )
        finally:
            self._loading = False

    async def _select_backend(self, value: Any) -> None:
        current = self.draft.persistence
        selected = current.backend if current is not None else NO_PERSISTENCE
        if value == selected:
            # Our own write echoing back (or a no-op re-pick): keep the values.
            await self._render_persistence()
            return
        if value == NO_PERSISTENCE:
            self.draft.persistence = None
        else:
            name = str(value)
            spec = backend_by_name(name)
            declared = {f.key for f in spec.fields} if spec is not None else set()
            keep = current.values if current is not None else {}
            # Backends take disjoint keys; carrying a Mongo URI over into a
            # sqlite block would write a chimera the loader passes straight
            # through to the driver.
            self.draft.persistence = PersistenceDraft(
                backend=name,
                values={k: v for k, v in keep.items() if k in declared},
            )
        await self._render_persistence()
        self._refresh_dirty()

    def _sync_persistence(self) -> None:
        current = self.draft.persistence
        if current is None:
            return
        values = dict(current.values)
        for spec_field in self._backend_fields:
            try:
                box = self.query_one(f"#pers-{spec_field.key}", Input)
            except Exception:  # noqa: BLE001 - panel is mid-rebuild
                continue
            text = box.value.strip()
            if text:
                values[spec_field.key] = text
            else:
                values.pop(spec_field.key, None)
        self.draft.persistence = PersistenceDraft(
            backend=current.backend, values=values, enabled=current.enabled
        )

    # -- orphans ---------------------------------------------------------------

    async def _render_orphans(self) -> None:
        panel = self.query_one("#profile-orphans", Vertical)
        await panel.remove_children()
        orphans = self.draft.orphans(self._tree)
        if not orphans:
            return
        await panel.mount(Static("Orphan overrides", classes="panel-heading"))
        await panel.mount(
            Static(
                "\n".join(f"  {path} — no such effect any more" for path in orphans),
                classes="orphan-row",
                markup=False,
            )
        )
        await panel.mount(
            Static("Ctrl-O removes all of them.", classes="panel-note")
        )

    def action_drop_orphans(self) -> None:
        dropped = self.draft.drop_orphans(self._tree)
        if not dropped:
            self._set_status("No orphan overrides to drop.")
            return
        self.call_later(self._render_orphans)
        self._refresh_dirty()
        self._set_status(
            f"Dropped {len(dropped)} orphan override"
            f"{'s' if len(dropped) != 1 else ''}: {', '.join(dropped)}."
        )

    # -- text boxes -------------------------------------------------------------

    def on_input_changed(self, event: Input.Changed) -> None:
        if self._loading:
            return
        widget_id = event.input.id or ""
        if widget_id == "profile-name":
            event.stop()
            self.draft.name = event.value.strip()
            self._refresh_dirty()
        elif widget_id in ("profile-adapter-custom", "profile-model-custom"):
            event.stop()
            kind = "adapter" if "adapter" in widget_id else "model"
            setattr(self.draft, kind, event.value.strip() or None)
            self._refresh_dirty()
        elif widget_id.startswith("fx-custom-"):
            event.stop()
            node = self._row_of(event.input)
            if node is not None:
                self.draft.set_model(node.path, event.value.strip() or None)
                self._refresh_dirty()
        elif widget_id.startswith("pin-"):
            event.stop()
            self._sync_inputs()
            self._refresh_dirty()
        elif widget_id.startswith("pers-"):
            event.stop()
            self._sync_persistence()
            self._refresh_dirty()

    def on_input_submitted(self, event: Input.Submitted) -> None:
        """Enter commits rather than walking off the end of a long form."""
        event.stop()
        self.action_save()

    # -- actions ----------------------------------------------------------------

    def on_button_pressed(self, event: Button.Pressed) -> None:
        button_id = event.button.id or ""
        if button_id == "profile-save":
            event.stop()
            self.action_save()
        elif button_id == "profile-save-as":
            event.stop()
            self.action_save_as()
        elif button_id == "profile-run":
            event.stop()
            self.action_run_with_profile()

    def action_save(self) -> Path | None:
        """Validate the draft and write it next to the orchestration."""
        if self._choice is None:
            self._set_status("Pick an orchestration first.", "-failed")
            return None
        input_errors = self._sync_inputs()
        self._sync_persistence()
        problems = [*input_errors, *self.draft.problems(self._orch)]
        orphans = self.draft.orphans(self._tree)
        if orphans:
            problems.append(
                f"orphan override(s) {', '.join(orphans)} — Ctrl-O drops them."
            )
        if problems:
            self._set_status(problems[0], "-failed")
            return None
        try:
            path = self.draft.save(profile_dir_for(self._choice.path))
        except OSError as exc:
            self._set_status(f"Could not write the profile: {exc}", "-failed")
            return None
        self._refresh_profile_picker()
        self._refresh_dirty()
        self._set_status(
            f"Saved {path} — cof run {self._choice.label} "
            f"--profile {self.draft.name}",
            "-done",
        )
        return path

    def action_save_as(self) -> None:
        """Duplicate the draft under a new name, ready to be edited and saved."""
        base = self.draft.name or DEFAULT_PROFILE_NAME
        self.draft = self.draft.duplicate(_next_name(base, self._existing_names()))
        self._loading = True
        try:
            self.query_one("#profile-name", Input).value = self.draft.name
        finally:
            self._loading = False
        self._refresh_dirty()
        self._set_status(
            f"Duplicated as {self.draft.name} — rename it and press Ctrl-S to save."
        )
        self.query_one("#profile-name", Input).focus()

    def action_run_with_profile(self) -> None:
        """Save if needed, then hand orchestration + profile to the Run view."""
        if self._choice is None:
            self._set_status("Pick an orchestration first.", "-failed")
            return
        if self.draft.dirty and self.action_save() is None:
            return
        launch = getattr(self.app, "launch_run", None)
        if not callable(launch):  # pragma: no cover - only a stub app lacks it
            self._set_status("This app has no Run view to hand off to.", "-failed")
            return
        launch(self._choice.path, profile=self.draft.name)

    def action_leave(self) -> None:
        """``q``/``Esc``, but never silently over the top of unsaved edits."""
        self.confirm_leave(self._leave)

    def _leave(self) -> None:
        back = getattr(self.app, "action_back_or_quit", None)
        if callable(back):
            back()

    def confirm_leave(self, proceed: Any) -> None:
        """Screen guard hook — see :meth:`CircuitryScreen.confirm_leave`."""
        if not self.draft.dirty:
            proceed()
            return

        def _answer(discard: bool | None) -> None:
            if discard:
                self.draft.mark_clean()
                proceed()

        self.app.push_screen(ConfirmDiscard(self.draft.name or "This profile"), _answer)

    # -- status ------------------------------------------------------------------

    def _existing_names(self) -> set[str]:
        if self._choice is None:
            return set()
        return {name for name, _ in discover_profiles(self._choice.path, cwd=self._cwd)}

    def _refresh_dirty(self) -> None:
        mark = DIRTY_MARK if self.draft.dirty else CLEAN_MARK
        self.query_one("#profile-dirty", Static).update(f"{self.draft.name}  {mark}")

    def _set_status(self, text: str, state_class: str = "") -> None:
        status = self.query_one("#profile-status", Static)
        status.remove_class("-failed", "-done")
        if state_class:
            status.add_class(state_class)
        status.update(text)
        self._status_text = text

    @property
    def status_text(self) -> str:
        """The status line as plain text (used by tests)."""
        return self._status_text


# -- widget helpers ------------------------------------------------------------


def _picker_options(
    options: list[str], current: str | None, *, custom: bool = True
) -> list[tuple[str, str]]:
    """Known values, bracketed by the "leave it alone" and "type it" sentinels.

    A value the config does not enumerate — a model only this profile names —
    is folded in rather than silently reset to the default. That is the
    free-text fallback surviving a reload. ``custom=False`` for pickers with
    no free-text box beside them, so the sentinel can never be saved as a
    literal value.
    """
    values = [
        (f"{NO_OVERRIDE} default", NO_OVERRIDE),
        *((value, value) for value in options),
    ]
    if current and current not in options:
        values.append((current, current))
    if custom:
        values.append((CUSTOM, CUSTOM))
    return values


def _picker(
    widget_id: str, options: list[str], current: str | None, *, custom: bool = True
) -> Select[str]:
    return Select(
        _picker_options(options, current, custom=custom),
        value=current if current else NO_OVERRIDE,
        allow_blank=False,
        id=widget_id,
    )


def _set_picker(select: Select[str], options: list[str], current: str | None) -> None:
    """Repoint an already-mounted picker without remounting it."""
    select.set_options(_picker_options(options, current))
    select.value = current if current else NO_OVERRIDE


def _custom_box(widget_id: str, placeholder: str) -> Input:
    box = Input(value="", placeholder=placeholder, id=widget_id)
    # Only revealed by choosing "custom…"; an enumerated value keeps it out
    # of the way, because the picker is already showing it.
    box.display = False
    return box


def _row_controls(
    index: int,
    node: EffectNode,
    override: EffectOverride,
    providers: list[str],
    models: list[str],
) -> list[Any]:
    """The provider picker, model picker, free-text box and toggle for a row."""
    disabled = node.kind == "condition"
    # The provider picker has no free-text box beside it: adapter names come
    # from a closed registry, and an unenumerated one is folded in by
    # ``_picker_options`` anyway.
    provider = _picker(
        f"fx-provider-{index}", providers, override.provider, custom=False
    )
    model = _picker(f"fx-model-{index}", models, override.model)
    provider.disabled = disabled
    model.disabled = disabled
    custom = _custom_box(f"fx-custom-{index}", "model name")
    switch = Switch(value=override.enabled is not False, id=f"fx-enabled-{index}")
    return [provider, model, custom, switch]


def _backend_fields(current: PersistenceDraft | None) -> list[BackendField]:
    """The selected backend's fields, plus any extra keys the file carries."""
    if current is None:
        return []
    spec = backend_by_name(current.backend)
    fields = list(spec.fields) if spec is not None else []
    declared = {f.key for f in fields}
    for key in sorted(current.values):
        if key not in declared:
            # e.g. sqlite's `db_path` alias, or a key a newer engine accepts.
            fields.append(BackendField(key, key, placeholder="passed through"))
    return fields


def _index_of(widget_id: str) -> int:
    try:
        return int(widget_id.rsplit("-", 1)[-1])
    except ValueError:  # pragma: no cover - ids are generated, not typed
        return -1


def _input_text(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float, str)):
        return str(value)
    import json

    return json.dumps(value, ensure_ascii=False)


def _next_name(base: str, taken: set[str]) -> str:
    """``fast`` → ``fast-copy`` → ``fast-copy-2`` …"""
    candidate = f"{base}-copy"
    if candidate not in taken:
        return candidate
    counter = 2
    while f"{candidate}-{counter}" in taken:
        counter += 1
    return f"{candidate}-{counter}"


def _ready_message(choice: OrchestrationChoice, tree: list[EffectNode]) -> str:
    overridable = sum(1 for node in tree if node.overridable)
    return (
        f"{choice.label} — {overridable} overridable effect"
        f"{'s' if overridable != 1 else ''}."
    )


def _safe_config() -> CircuitryConfig:
    try:
        return resolve_config()
    except Exception:  # noqa: BLE001 - a bad config file must not kill the view
        return CircuitryConfig()


def _safe_choices(root: Optional[Path]) -> list[OrchestrationChoice]:
    try:
        return discover_orchestrations(root)
    except Exception:  # noqa: BLE001 - discovery touches the filesystem
        return []
