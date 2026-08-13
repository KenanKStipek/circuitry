"""Chat view — build an orchestration by talking to the wizard.

A light seed form (name, category, one-line goal) opens a conversation. Every
message you send re-runs `curation/agents/wizard.yml` over the accumulated
transcript on a worker thread; its `say` lands as the assistant's reply and its
`yaml`, when it produces one, lands in the pane on the right with the
validator's verdict above it. Once that verdict is green the draft can be saved
— to a file, or into the local library with a generated manifest entry — and
handed to the Run view.

Everything that is not Textual lives in :mod:`circuitry.tui.wizard_host`: the
transcript, the draft, the verdict, the two save paths. This module is the
screen around it, so the view stays a rendering of host state rather than the
place the rules live.

The screen never talks to an adapter. It calls a
:data:`~circuitry.tui.wizard_host.TurnRunner`, which defaults to
``api.run_orchestration`` over whatever the config resolves — so the view works
over any configured adapter, and a test supplies a scripted one.
"""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING, ClassVar, Optional

from textual.app import ComposeResult
from textual.binding import Binding, BindingType
from textual.containers import Horizontal, Vertical
from textual.widgets import Input, Static

from .screens import ViewScreen, ViewSpec
from .wizard_host import (
    CATEGORIES,
    DEFAULT_CATEGORY,
    Conversation,
    DraftStatus,
    InvalidDraft,
    Seed,
    Turn,
    TurnRunner,
    default_library_dir,
    default_runner,
    save_to_file,
    save_to_library,
)

if TYPE_CHECKING:  # pragma: no cover - typing only
    from textual.events import Mount

__all__ = ["ChatScreen", "DraftPane", "MessageBubble", "draft_preview"]

SEED_HINT = (
    "Name it, say in one line what it should do, then press Enter.\n"
    f"Category is one of: {', '.join(CATEGORIES)}."
)
EMPTY_DRAFT = "No draft yet — keep talking."
THINKING = "Thinking…"
DONE_NOTE = "The wizard is done. Save it with Ctrl-S, or into the library with Ctrl-G."

#: Lines of draft shown in the pane. A 200-line orchestration in a scroll
#: container is a wall; the file on disk is the artefact, not this preview.
PREVIEW_LINES = 40

#: Slash commands accepted in the message box, for people who would rather type
#: than reach for a chord. Same actions as the bindings.
COMMANDS = ("/save", "/library", "/run")


def draft_preview(draft: str, *, limit: int = PREVIEW_LINES) -> str:
    """The first ``limit`` lines of a draft, with a count of what is elided."""
    if not draft.strip():
        return EMPTY_DRAFT
    lines = draft.splitlines()
    if len(lines) <= limit:
        return "\n".join(lines)
    hidden = len(lines) - limit
    return "\n".join([*lines[:limit], f"… {hidden} more line{'' if hidden == 1 else 's'}"])


class MessageBubble(Static):
    """One line of the transcript, styled by who said it."""

    DEFAULT_CSS = """
    MessageBubble { height: auto; margin-bottom: 1; }
    MessageBubble.-user { color: $accent; }
    MessageBubble.-wizard { color: $text; }
    MessageBubble.-note { color: $text-muted; }
    """

    def __init__(self, role: str, content: str) -> None:
        # markup=False: a wizard reply quotes schema fragments and file paths,
        # and "[A-Za-z_]" is not a style tag.
        super().__init__(classes=f"-{role}", markup=False)
        self.role = role
        self.content = content

    def on_mount(self) -> None:
        speaker = {"user": "you", "wizard": "wizard", "note": ""}.get(self.role, self.role)
        prefix = f"{speaker}  " if speaker else ""
        self.update(f"{prefix}{self.content}")


class DraftPane(Vertical):
    """The YAML side: the validator's verdict, its errors, then the draft."""

    DEFAULT_CSS = """
    DraftPane { height: auto; }
    DraftPane #draft-status { text-style: bold; }
    DraftPane #draft-status.-ok { color: $success; }
    DraftPane #draft-status.-bad { color: $error; }
    DraftPane #draft-status.-idle { color: $text-muted; }
    DraftPane #draft-errors { color: $error; height: auto; }
    DraftPane #draft-yaml { color: $text-muted; height: auto; }
    """

    STATE_CLASSES: ClassVar[tuple[str, ...]] = ("-ok", "-bad", "-idle")

    def compose(self) -> ComposeResult:
        yield Static(EMPTY_DRAFT, id="draft-status", classes="-idle")
        yield Static("", id="draft-errors", markup=False)
        yield Static("", id="draft-yaml", markup=False)

    def show(self, draft: str, status: Optional[DraftStatus]) -> None:
        """Render a draft and the verdict on it (``None`` = nothing checked)."""
        headline = self.query_one("#draft-status", Static)
        headline.remove_class(*self.STATE_CLASSES)
        if status is None:
            headline.add_class("-idle")
            headline.update(EMPTY_DRAFT)
        else:
            headline.add_class("-ok" if status.ok else "-bad")
            headline.update(status.headline())
        self.query_one("#draft-errors", Static).update(
            "" if status is None or status.ok else "\n".join(f"• {e}" for e in status.errors)
        )
        self.query_one("#draft-yaml", Static).update(draft_preview(draft))


class ChatScreen(ViewScreen):
    """Seed form, conversation, live-validated draft, save."""

    DEFAULT_CSS = """
    ChatScreen #chat-seed { height: auto; }
    ChatScreen #chat-hint { color: $text-muted; margin-bottom: 1; }
    ChatScreen #chat-seed-problems { color: $warning; height: auto; }
    ChatScreen #chat-panes { height: auto; }
    ChatScreen #chat-left { width: 3fr; height: auto; padding-right: 1; }
    ChatScreen #chat-right { width: 2fr; height: auto; }
    ChatScreen #chat-log { height: auto; }
    ChatScreen #chat-status { color: $text-muted; height: auto; }
    ChatScreen .-hidden { display: none; }

    /* Side by side needs width; below the breakpoint the panes stack. */
    ChatScreen.-compact #chat-panes { layout: vertical; }
    ChatScreen.-compact #chat-left,
    ChatScreen.-compact #chat-right { width: 1fr; padding-right: 0; }
    """

    BINDINGS: ClassVar[list[BindingType]] = [
        Binding("ctrl+s", "save_file", "Save to file"),
        Binding("ctrl+g", "save_library", "Save to library"),
        Binding("ctrl+r", "run_it", "Run it now"),
    ]

    def __init__(
        self,
        spec: ViewSpec,
        *,
        runner: Optional[TurnRunner] = None,
        library_dir: Optional[Path] = None,
        seed: Optional[Seed] = None,
    ) -> None:
        super().__init__(spec)
        self._runner = runner or default_runner()
        self._library_dir = library_dir
        self.conversation = Conversation(seed or Seed())
        self.started = seed is not None
        self.busy = False
        self.saved_path: Optional[Path] = None
        #: Whatever the status line last said — the screen's answer to "what
        #: happened when I pressed that?", kept as state so it is assertable.
        self.status_text = ""
        self._closing = False

    # -- composition ---------------------------------------------------------

    def compose_body(self) -> ComposeResult:
        yield Static(self.spec.name, classes="view-title")
        yield Static(self.spec.blurb, classes="view-blurb")
        seed = self.conversation.seed
        yield Vertical(
            Static(SEED_HINT, id="chat-hint"),
            Input(value=seed.name, placeholder="name", id="seed-name"),
            Input(value=seed.category, placeholder="category", id="seed-category"),
            Input(value=seed.goal, placeholder="what should it do?", id="seed-goal"),
            Static("", id="chat-seed-problems", markup=False),
            id="chat-seed",
            classes="-hidden" if self.started else "",
        )
        yield Horizontal(
            Vertical(
                Vertical(id="chat-log"),
                Input(placeholder="message the wizard (/save, /library, /run)", id="chat-message"),
                id="chat-left",
            ),
            Vertical(
                DraftPane(id="chat-draft"),
                Input(placeholder="path to save to…", id="chat-save-path"),
                id="chat-right",
            ),
            id="chat-panes",
            classes="" if self.started else "-hidden",
        )
        yield Static("", id="chat-status", markup=False)

    def _on_mount(self, event: Mount) -> None:
        super()._on_mount(event)
        if self.started:
            self._begin()
        else:
            self.query_one("#seed-name", Input).focus()

    def on_unmount(self) -> None:
        """Stop a turn from being posted into a screen that is going away."""
        self._closing = True

    # -- input ---------------------------------------------------------------

    def on_input_submitted(self, event: Input.Submitted) -> None:
        event.stop()
        which = event.input.id
        if which in ("seed-name", "seed-category", "seed-goal"):
            self.submit_seed()
        elif which == "chat-message":
            self.send(event.value)
        elif which == "chat-save-path":
            self.action_save_file()

    def submit_seed(self) -> None:
        """Validate the seed form; on the second Enter with it complete, start."""
        seed = Seed(
            name=self.query_one("#seed-name", Input).value,
            category=self.query_one("#seed-category", Input).value.strip().lower()
            or DEFAULT_CATEGORY,
            goal=self.query_one("#seed-goal", Input).value,
        )
        problems = seed.problems()
        self.query_one("#chat-seed-problems", Static).update("\n".join(problems))
        if problems:
            return
        self.conversation = Conversation(seed)
        self.started = True
        self.query_one("#chat-seed").add_class("-hidden")
        self.query_one("#chat-panes").remove_class("-hidden")
        self.query_one("#chat-save-path", Input).value = f"{seed.slug}.yml"
        self._begin()

    def _begin(self) -> None:
        """First turn: the seed alone, no user message yet."""
        self.query_one("#chat-message", Input).focus()
        self._run_turn()

    def send(self, text: str) -> None:
        """A message from the human — or a slash command."""
        text = text.strip()
        if not text or self.busy:
            return
        box = self.query_one("#chat-message", Input)
        box.value = ""
        if text.split(" ", 1)[0] in COMMANDS:
            self._command(text)
            return
        self.conversation.add_user(text)
        self._append("user", text)
        self._run_turn()

    def _command(self, text: str) -> None:
        verb, _, rest = text.partition(" ")
        if verb == "/save":
            if rest.strip():
                self.query_one("#chat-save-path", Input).value = rest.strip()
            self.action_save_file()
        elif verb == "/library":
            self.action_save_library()
        elif verb == "/run":
            self.action_run_it()

    # -- the turn ------------------------------------------------------------

    def _run_turn(self) -> None:
        self.busy = True
        self.query_one("#chat-message", Input).disabled = True
        self._status(THINKING)
        self.run_worker(self._work, thread=True, exclusive=True, group="wizard-turn")

    def _work(self) -> None:
        state = self.conversation.state()
        try:
            turn = self._runner(state)
        except Exception as exc:  # noqa: BLE001 - never let a worker die silently
            self._hand_back(None, f"{type(exc).__name__}: {exc}")
            return
        self._hand_back(turn, None)

    def _hand_back(self, turn: Optional[Turn], error: Optional[str]) -> None:
        if self._closing:
            return
        try:
            self.app.call_from_thread(self._finish, turn, error)
        except RuntimeError:
            # The app stopped between the turn finishing and the hand-off.
            self._closing = True

    def _finish(self, turn: Optional[Turn], error: Optional[str]) -> None:
        self.busy = False
        box = self.query_one("#chat-message", Input)
        box.disabled = False
        if turn is None:
            self._status(f"The turn failed: {error}")
            self._append("note", f"The turn failed: {error}")
            return
        self.conversation.record(turn)
        self._append("wizard", turn.say)
        self._paint_draft()
        self._status(DONE_NOTE if self.conversation.done else "")
        box.focus()

    # -- saving --------------------------------------------------------------

    def action_save_file(self) -> None:
        """Write the draft to the path in the save box."""
        if not self._savable():
            return
        box = self.query_one("#chat-save-path", Input)
        target = box.value.strip()
        if not target:
            self._status("Type a path to save to, then press Enter.")
            box.focus()
            return
        try:
            path = save_to_file(self.conversation.draft, Path(target))
        except InvalidDraft as exc:
            self._status(f"Not saved — the draft is not valid: {exc}")
            return
        except OSError as exc:
            self._status(f"Not saved — {exc}")
            return
        self.saved_path = path
        self._status(f"Saved {path}. Ctrl-R runs it.")

    def action_save_library(self) -> None:
        """Write the draft into the local library and index it."""
        if not self._savable():
            return
        directory = self._library_dir if self._library_dir is not None else default_library_dir()
        try:
            saved = save_to_library(
                self.conversation.draft, self.conversation.seed, library_dir=directory
            )
        except InvalidDraft as exc:
            self._status(f"Not saved — the draft is not valid: {exc}")
            return
        except OSError as exc:
            self._status(f"Not saved — {exc}")
            return
        self.saved_path = saved.path
        self._status(f"Saved {saved.name} to {saved.path}. Ctrl-R runs it.")

    def action_run_it(self) -> None:
        """Hand the saved file to the Run view."""
        if self.saved_path is None:
            self._status("Save it first — there is no file to run yet.")
            return
        launch = getattr(self.app, "launch_run", None)
        if not callable(launch):  # pragma: no cover - the app always has it
            self._status("This app has no Run view to hand it to.")
            return
        launch(self.saved_path)

    def _savable(self) -> bool:
        """Guard every save path with the pane's own verdict."""
        if self.conversation.can_save:
            return True
        if not self.conversation.draft:
            self._status("There is no draft to save yet.")
        else:
            self._status("The draft has not passed validation — it cannot be saved.")
        return False

    # -- rendering -----------------------------------------------------------

    def _append(self, role: str, content: str) -> None:
        self.query_one("#chat-log", Vertical).mount(MessageBubble(role, content))
        self.query_one("#body").scroll_end(animate=False)

    def _paint_draft(self) -> None:
        self.query_one("#chat-draft", DraftPane).show(
            self.conversation.draft, self.conversation.status
        )

    def _status(self, text: str) -> None:
        self.status_text = text
        self.query_one("#chat-status", Static).update(text)
