"""The Runs view: browse a run's state like a filesystem, and replay the last one.

Left pane is the state tree — every key of the run's state dict, ``prime``
first, each row showing what is under it. Right pane is the detail: the
selected node's dot-path, the ``meta`` of the effect it belongs to
(adapter, model, tokens, created/completed, error), and the value in full,
which is how a truncated row is expanded. ``y`` copies the dot-path, so
what you found here pastes straight into a template.

Three sources, one tree:

*live* — the run currently in flight, read from the app's
:class:`~circuitry.tui.inspector.StateStore`. The run publishes snapshots
from its own thread and this view polls the store on a timer, so watching
never blocks the run, and a run launched from the Run view keeps updating
here after you navigate away from it.

*post-run* — the same store once the run has landed.

*file* — a JSON document an earlier run wrote with ``--out`` or
``--live-state``, opened by typing its path. A file that is missing, is
not JSON, or does not hold an object gets a written-out error state rather
than an empty pane.

``Ctrl-R`` replays the last ``cof run`` — the arguments stashed in
``~/.config/circuitry/last-run.json``, the same ones ``cof run --last``
reads — through :class:`~circuitry.tui.launch.RunSession`, publishing into
the store so the tree fills in as it goes.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import TYPE_CHECKING, Any, ClassVar, cast

from rich.text import Text
from textual.app import ComposeResult
from textual.binding import Binding, BindingType
from textual.containers import Horizontal, Vertical, VerticalScroll
from textual.css.query import NoMatches
from textual.message import Message
from textual.widget import Widget
from textual.widgets import Input, Static, Tree

from ..cli.config import CircuitryConfig, resolve_config
from ..cli.last_run import LastRun, read_last_run
from ..cli.runtime_shim import RunRequest, RunResult
from .inspector import (
    LoadedState,
    StateNode,
    StateStore,
    build_state_nodes,
    detail_lines,
    find_node,
    flatten,
    load_state_file,
    render_text,
)
from .launch import RunSession, Runner
from .screens import ViewScreen, ViewSpec

if TYPE_CHECKING:  # pragma: no cover - typing only
    from textual.events import Mount

    from circuitry.adapters import Adapter

    from .app import CircuitryApp

__all__ = ["EMPTY_SOURCE", "FILE", "LIVE", "POST_RUN", "RunsScreen"]

#: Source modes, which is also what the status line leads with.
LIVE = "live"
POST_RUN = "post-run"
FILE = "file"

#: Shown when there is nothing to inspect yet.
EMPTY_SOURCE = "No run state yet — launch a run from the Run view, or open a --out file below."

#: Placeholder for the tree root before anything is loaded.
NO_STATE = "no state"

#: How often the live source is checked for a new snapshot. Fast enough to
#: read as live, slow enough that a chatty run costs a handful of repaints.
POLL_SECONDS = 0.2

#: Paths expanded when a tree is first drawn: the run's own namespace.
DEFAULT_EXPANDED = frozenset({"prime"})


class RunsScreen(ViewScreen):
    """State inspector over a live run, a finished run, or a saved file."""

    #: The panes scroll inside themselves (the tree and the detail pane both
    #: do), so the body must not scroll under them — the status line and the
    #: last-run line have to stay on screen while the tree is walked.
    BODY_CONTAINER: ClassVar[type[Widget]] = Vertical

    CSS = """
    RunsScreen #runs-panes {
        height: 1fr;
    }

    RunsScreen #runs-tree {
        width: 2fr;
    }

    RunsScreen #runs-detail-pane {
        width: 3fr;
        border-left: solid $panel;
        padding-left: 1;
    }

    RunsScreen #runs-error {
        color: $error;
        height: auto;
    }

    RunsScreen #runs-status {
        height: 1;
        color: $text-muted;
    }

    RunsScreen #runs-status.-failed {
        color: $error;
    }

    RunsScreen #runs-last {
        height: 1;
        color: $text-muted;
    }

    /* Below the breakpoint the two panes stack and the chrome thins out. */
    RunsScreen.-compact #runs-panes {
        layout: vertical;
    }

    RunsScreen.-compact #runs-tree,
    RunsScreen.-compact #runs-detail-pane {
        width: 1fr;
        border-left: none;
        padding-left: 0;
    }

    RunsScreen.-tiny #runs-file,
    RunsScreen.-tiny #runs-last,
    RunsScreen.-tiny #runs-status {
        display: none;
    }
    """

    BINDINGS: ClassVar[list[BindingType]] = [
        Binding("y", "copy_path", "Copy path"),
        Binding("o", "open_file", "Open state file"),
        Binding("ctrl+r", "replay", "Replay last run"),
        Binding("ctrl+x", "cancel_replay", "Cancel replay", show=False),
        Binding("escape", "close_file_or_back", "Back", show=False),
    ]

    class ReplayState(Message):
        """A snapshot from the replay's worker thread (already deep-copied)."""

        def __init__(self, state: dict[str, Any]) -> None:
            super().__init__()
            self.state = state

    class ReplayFinished(Message):
        """The replay's worker thread is done, successfully or not."""

        def __init__(self, result: RunResult, *, cancelled: bool) -> None:
            super().__init__()
            self.result = result
            self.cancelled = cancelled

    def __init__(
        self,
        spec: ViewSpec,
        *,
        store: StateStore | None = None,
        last_run: LastRun | None = None,
        last_run_path: Path | None = None,
        config: CircuitryConfig | None = None,
        adapter: Adapter | None = None,
        runner: Runner | None = None,
        name: str | None = None,
        id: str | None = None,  # noqa: A002 - Textual's parameter name
        classes: str | None = None,
    ) -> None:
        super().__init__(spec, name=name, id=id, classes=classes)
        # Injection points for tests and embedders: the store to read, the
        # stash to replay, and a stand-in for runtime_shim.run.
        self._store = store
        self._last_run_path = last_run_path
        self._last_run: LastRun | None = last_run
        self._last_run_read = last_run is not None
        self._config = config
        self._adapter = adapter
        self._runner = runner

        self._state_nodes: tuple[StateNode, ...] = ()
        self._mode = POST_RUN
        self._revision = -1
        self._file: LoadedState | None = None
        self._expanded: set[str] = set(DEFAULT_EXPANDED)
        self._selected: str = ""
        self._status = EMPTY_SOURCE
        self._status_failed = False
        self._session: RunSession | None = None
        self._poll_timer: Any = None
        #: Last path handed to the clipboard, for tests and for the status line.
        self.copied_path = ""

    # -- composition ---------------------------------------------------------

    def compose_body(self) -> ComposeResult:
        yield Static(f"{self.spec.name} — {self.spec.blurb}", classes="view-blurb")
        yield Input(
            placeholder="path to a state JSON (--out / --live-state) — Enter opens it",
            id="runs-file",
        )
        yield Static("", id="runs-error", markup=False)
        yield Horizontal(
            Tree(NO_STATE, id="runs-tree"),
            VerticalScroll(
                Static("", id="runs-detail", markup=False),
                id="runs-detail-pane",
            ),
            id="runs-panes",
        )
        yield Static("", id="runs-status", markup=False)
        yield Static("", id="runs-last", markup=False)

    def _on_mount(self, event: Mount) -> None:
        super()._on_mount(event)
        if self._store is None:
            self._store = _app_store(self.app)
        self._refresh_last_run()
        self._adopt_store(force=True)
        self._poll_timer = self.set_interval(POLL_SECONDS, self._poll)
        # The tree takes the keyboard, not the path box: ``y`` and ``o``
        # have to work the moment the view opens, and a focused Input
        # would swallow them as text.
        self._tree.focus()

    # -- sources -------------------------------------------------------------

    @property
    def store(self) -> StateStore:
        """The live source; an empty one when the app has none (tests)."""
        if self._store is None:
            self._store = StateStore()
        return self._store

    def _poll(self) -> None:
        """Pick up a new snapshot, if the run published one and we want it."""
        if self._mode == FILE:
            return
        self._adopt_store()

    def _adopt_store(self, *, force: bool = False) -> None:
        revision, state, running = self.store.snapshot()
        if not force and revision == self._revision:
            return
        self._revision = revision
        self._mode = LIVE if running else POST_RUN
        self._file = None
        self._show_error(None)
        self._set_nodes(build_state_nodes(state))
        self._set_status(self._source_line(), failed=False)

    def _source_line(self) -> str:
        if self._mode == FILE:
            loaded = self._file
            where = str(loaded.path) if loaded is not None and loaded.path else "—"
            return f"{FILE} — {where}"
        label = self.store.label
        if not self._state_nodes:
            return EMPTY_SOURCE if not label else f"{self._mode} — {label} (no state yet)"
        return f"{self._mode} — {label}" if label else self._mode

    def open_file(self, path: Path) -> LoadedState:
        """Switch to file mode on ``path``; returns what happened."""
        loaded = load_state_file(path)
        self._file = loaded
        self._mode = FILE
        if loaded.ok:
            self._show_error(None)
            self._set_nodes(build_state_nodes(loaded.state))
            self._set_status(self._source_line(), failed=False)
        else:
            self._show_error(loaded)
            self._set_nodes(())
            # Terse on the status line, explained in the detail pane — the
            # banner over the tree is the third of the same story, so it
            # carries only the headline.
            self._set_status(f"{FILE} — {path} did not open", failed=True)
        return loaded

    def close_file(self) -> None:
        """Leave file mode and go back to whatever the run source holds."""
        self._file = None
        self._clear_path_box()
        self._adopt_store(force=True)

    def _clear_path_box(self) -> None:
        box = self._widget("#runs-file", Input)
        if box is not None:
            box.value = ""

    # -- the tree ------------------------------------------------------------

    def _widget(self, selector: str, kind: type[Any]) -> Any:
        """The widget, or ``None`` when this screen has no body yet.

        Every repaint goes through here: the model is updated whether or
        not there is anything to draw on, so a screen that is not on the
        stack (or is still mounting) is never a special case.
        """
        try:
            return self.query_one(selector, kind)
        except NoMatches:
            return None

    def _set_nodes(self, nodes: tuple[StateNode, ...]) -> None:
        self._state_nodes = nodes
        # Keep the cursor where it was across a live repaint; land it on the
        # run's own namespace the first time there is one.
        if nodes and find_node(nodes, self._selected) is None:
            self._selected = nodes[0].path
        elif not nodes:
            self._selected = ""
        self._build_tree()
        self._refresh_detail()

    def _build_tree(self) -> None:
        tree = self._widget("#runs-tree", Tree)
        if tree is None:
            return
        tree.clear()
        tree.root.label = self._root_label()
        tree.root.data = ""
        tree.root.expand()
        placed: dict[str, Any] = {}
        for node in self._state_nodes:
            self._add(tree.root, node, placed)
        target = placed.get(self._selected)
        if target is not None:
            tree.move_cursor(target)

    def _root_label(self) -> str:
        if self._mode == FILE and self._file is not None and self._file.path is not None:
            return self._file.path.name
        return self.store.label or ("state" if self._state_nodes else NO_STATE)

    def _add(self, parent: Any, node: StateNode, placed: dict[str, Any]) -> None:
        label = _row_label(node)
        if node.children:
            row = parent.add(label, data=node.path, expand=node.path in self._expanded)
        else:
            row = parent.add_leaf(label, data=node.path)
        placed[node.path] = row
        for child in node.children:
            self._add(row, child, placed)
        if node.omitted:
            row.add_leaf(Text(f"… {node.omitted} more", style="dim"), data=node.path)

    @property
    def _tree(self) -> Tree[str]:
        return self.query_one("#runs-tree", Tree)

    def select_path(self, path: str) -> bool:
        """Reveal ``path``, expanding whatever hides it, and select it.

        Returns ``False`` when the current state has no such path — which
        is the honest answer for a path from an older snapshot.
        """
        if find_node(self._state_nodes, path) is None:
            return False
        self._expanded |= {
            node.path
            for node in flatten(self._state_nodes)
            if node.children and _is_ancestor(node.path, path)
        }
        self._selected = path
        self._build_tree()
        self._refresh_detail()
        return True

    @property
    def selected_node(self) -> StateNode | None:
        """The highlighted state node, or ``None`` when the tree is empty."""
        return find_node(self._state_nodes, self._selected) if self._selected else None

    def on_tree_node_highlighted(self, event: Tree.NodeHighlighted[str]) -> None:
        event.stop()
        self._selected = str(event.node.data or "")
        self._refresh_detail()

    def on_tree_node_expanded(self, event: Tree.NodeExpanded[str]) -> None:
        event.stop()
        path = str(event.node.data or "")
        if path:
            self._expanded.add(path)

    def on_tree_node_collapsed(self, event: Tree.NodeCollapsed[str]) -> None:
        event.stop()
        self._expanded.discard(str(event.node.data or ""))

    # -- the detail pane -----------------------------------------------------

    def _refresh_detail(self) -> None:
        detail = self._widget("#runs-detail", Static)
        if detail is not None:
            detail.update("\n".join(self.detail_text_lines()))

    def detail_text_lines(self) -> list[str]:
        """What the detail pane says right now (used by tests)."""
        loaded = self._file
        if self._mode == FILE and loaded is not None and not loaded.ok:
            return [loaded.error, "", loaded.hint] if loaded.hint else [loaded.error]
        if not self._state_nodes:
            return [EMPTY_SOURCE]
        return detail_lines(self.selected_node)

    def _show_error(self, loaded: LoadedState | None) -> None:
        box = self._widget("#runs-error", Static)
        if box is None:
            return
        if loaded is None or loaded.ok:
            box.display = False
            box.update("")
            return
        box.display = True
        box.update(loaded.error)

    # -- the last run --------------------------------------------------------

    @property
    def last_run(self) -> LastRun | None:
        """The stashed run, read once and cached."""
        if not self._last_run_read:
            self._last_run = read_last_run(self._last_run_path)
            self._last_run_read = True
        return self._last_run

    def _refresh_last_run(self) -> None:
        box = self._widget("#runs-last", Static)
        if box is not None:
            box.update(self.last_run_text)

    @property
    def last_run_text(self) -> str:
        """The one-line last-run summary under the panes."""
        stashed = self.last_run
        if stashed is None:
            return "Last run — none stashed yet (cof run records one on success)."
        if stashed.error:
            return f"Last run — {stashed.error}"
        bits = [stashed.orchestration or "—"]
        bits += [value for label, value in stashed.summary_rows() if label in ("adapter", "model")]
        return f"Last run — {' · '.join(bits)}  (Ctrl-R replays it)"

    # -- actions -------------------------------------------------------------

    def action_copy_path(self) -> None:
        """``y`` — put the highlighted node's dot-path on the clipboard."""
        node = self.selected_node
        if node is None:
            self._set_status("Nothing highlighted to copy.", failed=True)
            return
        self.copied_path = node.path
        copy = getattr(self.app, "copy_to_clipboard", None)
        if callable(copy):
            copy(node.path)
        self._set_status(f"Copied {node.path}", failed=False)

    def action_open_file(self) -> None:
        """``o`` — jump to the path box."""
        self.query_one("#runs-file", Input).focus()

    def action_close_file_or_back(self) -> None:
        """Esc leaves file mode, then falls back to the app's Back."""
        box = self._widget("#runs-file", Input)
        if self._mode == FILE or (box is not None and box.value):
            self.close_file()
            self._tree.focus()
            return
        cast("CircuitryApp", self.app).action_back_or_quit()

    def on_input_submitted(self, event: Input.Submitted) -> None:
        event.stop()
        text = event.value.strip()
        if not text:
            self.close_file()
            return
        self.open_file(Path(text).expanduser())
        self._tree.focus()

    # -- replaying the last run ---------------------------------------------

    def action_replay(self) -> None:
        """``Ctrl-R`` — re-run the stashed ``cof run`` and watch it here.

        The run goes through the same session the Run view launches, with
        the stashed orchestration, inputs and adapter/model overrides. It
        does *not* re-write the stashed ``--out`` / ``--live-state`` files:
        a keypress in a browser should not overwrite an artefact on disk,
        and the state it would have written is on screen.
        """
        if self._session is not None and self._session.running:
            return
        stashed = self.last_run
        if stashed is None:
            self._set_status(
                "No previous run stashed — run one from the Run view or with cof run.",
                failed=True,
            )
            return
        blocked = stashed.blocked_reason
        if blocked:
            self._set_status(blocked, failed=True)
            return

        orch = Path(stashed.orchestration)
        if not orch.exists():
            self._set_status(f"Stashed orchestration is gone: {orch}", failed=True)
            return

        initial_state, state_path = _replay_inputs(stashed)
        request = RunRequest(
            orchestration_path=orch,
            state_path=state_path,
            out_path=None,
            dry_run=stashed.dry_run,
            validate_only=False,
            initial_state=initial_state,
            config=self._config if self._config is not None else _safe_config(stashed),
            adapter=self._adapter,
            adapter_override=stashed.adapter or None,
            model_override=stashed.model or None,
            skip_preflight=stashed.skip_preflight,
            profile_name=stashed.profile or None,
        )
        self.store.begin(label=orch.name)
        self._mode = LIVE
        self._file = None
        self._clear_path_box()
        self._revision = -1
        session = RunSession(
            request,
            on_state=self._replay_state,
            on_finish=self._replay_finished,
            runner=self._runner,
        )
        self._session = session
        self._set_status(f"Replaying {orch.name}…", failed=False)
        session.start()

    def action_cancel_replay(self) -> None:
        """Ask an in-flight replay to stop; a no-op when nothing is running."""
        session = self._session
        if session is None or not session.running:
            return
        session.cancel()
        self._set_status("Cancelling replay…", failed=False)

    def _replay_state(self, state: dict[str, Any]) -> None:
        """Worker-thread callback: hand the snapshot to the UI thread."""
        self.post_message(self.ReplayState(state))

    def _replay_finished(self, result: RunResult) -> None:
        """Worker-thread callback: hand the result back to the UI thread."""
        cancelled = self._session is not None and self._session.cancelled
        self.post_message(self.ReplayFinished(result, cancelled=cancelled))

    def on_runs_screen_replay_state(self, message: ReplayState) -> None:
        message.stop()
        # Published rather than drawn: the poll timer picks it up, so a
        # burst of snapshots costs one rebuild, not one each.
        self.store.publish(message.state)

    def on_runs_screen_replay_finished(self, message: ReplayFinished) -> None:
        message.stop()
        self.store.finish(message.result.state if message.result.ok else None)
        self._adopt_store(force=True)
        if message.cancelled:
            self._set_status("Replay cancelled.", failed=True)
        elif message.result.ok:
            self._set_status(f"{self._source_line()}  ·  replay finished", failed=False)
        else:
            self._set_status(f"Replay failed: {message.result.error}", failed=True)

    # -- status --------------------------------------------------------------

    def _set_status(self, text: str, *, failed: bool) -> None:
        self._status = text
        self._status_failed = failed
        status = self._widget("#runs-status", Static)
        if status is None:
            return
        status.set_class(failed, "-failed")
        status.update(text)

    @property
    def status_text(self) -> str:
        """The status line as plain text (used by tests)."""
        return self._status

    @property
    def mode(self) -> str:
        """Which source the tree is showing: live, post-run or file."""
        return self._mode

    @property
    def state_nodes(self) -> tuple[StateNode, ...]:
        """The state tree as last built (used by tests)."""
        return self._state_nodes

    @property
    def tree_text(self) -> str:
        """The tree as plain text, outliving the widget."""
        return render_text(self._state_nodes)


def _replay_inputs(stashed: LastRun) -> tuple[dict[str, Any] | None, Path | None]:
    """``(initial_state, state_path)`` for a replay, ranked as ``cof run`` ranks them.

    ``-e`` values win over a ``--state`` file, and the runtime reads only
    one of the two — so when the stash has both, the file is merged here
    exactly as the CLI merges it. An unreadable state file degrades to the
    inline values rather than failing the replay.
    """
    inline = stashed.initial_state()
    if not inline:
        return None, stashed.state_path
    merged: dict[str, Any] = {}
    if stashed.state_path is not None:
        try:
            loaded = json.loads(stashed.state_path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            loaded = None
        if isinstance(loaded, dict):
            merged.update(loaded)
    merged.update(inline)
    return merged, None


def _is_ancestor(parent: str, child: str) -> bool:
    """True when ``parent`` is a prefix path of ``child`` at a key boundary."""
    return child.startswith(parent) and child[len(parent) : len(parent) + 1] in (".", "[")


def _row_label(node: StateNode) -> Text:
    """A tree row: the key, then a dimmed preview of what is under it."""
    label = Text(node.key, style="bold")
    label.append("  ")
    label.append(node.preview, style="dim")
    return label


def _app_store(app: Any) -> StateStore | None:
    store = getattr(app, "run_states", None)
    return store if isinstance(store, StateStore) else None


def _safe_config(stashed: LastRun) -> CircuitryConfig:
    """The stashed run's config, falling back to defaults rather than failing."""
    try:
        return resolve_config(explicit_path=stashed.config_path)
    except Exception:  # noqa: BLE001 - a bad config file must not kill the view
        return CircuitryConfig()
