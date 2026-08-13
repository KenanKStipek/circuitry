"""Pilot tests for the Runs view: live, post-run and file modes, plus replay.

The live cases drive the view the way a run does — publishing snapshots
into the app's :class:`~circuitry.tui.inspector.StateStore` from a worker
thread — so what these assert on is the same path a real run takes.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
import yaml

pytest.importorskip("textual")

from textual.pilot import Pilot
from textual.widgets import Input, Static, Tree

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.last_run import LastRun
from circuitry.cli.redaction import REDACTED
from circuitry.cli.runtime_shim import RunRequest, RunResult
from circuitry.tui.inspector import StateStore
from circuitry.tui.launch import OrchestrationChoice
from circuitry.tui.run_view import RunScreen
from circuitry.tui.runs_view import EMPTY_SOURCE, FILE, LIVE, POST_RUN, RunsScreen
from circuitry.tui.screens import VIEWS

from state_fixture import fixture_state

RUNS_SPEC = next(spec for spec in VIEWS if spec.slug == "runs")
RUN_SPEC = next(spec for spec in VIEWS if spec.slug == "run")

#: A stash with nothing in it, so a view under test never reads the
#: developer's real ~/.config/circuitry/last-run.json.
NO_LAST_RUN = LastRun(path=Path("last-run.json"), args={})


def _screen(
    *,
    store: StateStore | None = None,
    last_run: LastRun | None = NO_LAST_RUN,
    last_run_path: Path | None = None,
    runner: Any = None,
    config: CircuitryConfig | None = None,
) -> RunsScreen:
    return RunsScreen(
        RUNS_SPEC,
        store=store if store is not None else StateStore(),
        last_run=last_run,
        last_run_path=last_run_path,
        runner=runner,
        config=config,
    )


async def _open(pilot: Pilot[Any], screen: RunsScreen) -> RunsScreen:
    await pilot.app.push_screen(screen)
    await pilot.pause()
    return screen


async def _settle(pilot: Pilot[Any]) -> None:
    """Let the view's poll timer pick up whatever was just published."""
    await pilot.pause(0.35)
    await pilot.pause()


def _highlight(screen: RunsScreen, path: str) -> None:
    """Put the tree cursor on ``path``, expanding whatever hides it."""
    assert screen.select_path(path), f"no row for {path} in:\n{screen.tree_text}"


def _detail(screen: RunsScreen) -> str:
    return "\n".join(screen.detail_text_lines())


# -- post-run mode ------------------------------------------------------------


def test_a_finished_run_is_browsable_in_full(run_app: Any) -> None:
    store = StateStore()
    store.finish(fixture_state())

    async def scenario(pilot: Pilot[Any]) -> RunsScreen:
        return await _open(pilot, _screen(store=store))

    screen = run_app(scenario)
    assert screen.mode == POST_RUN
    tree = screen.tree_text
    for key in ("prime", "draft", "over_items", "iter_1", "helper", "runtime"):
        assert key in tree


def test_the_meta_panel_explains_the_highlighted_value(run_app: Any) -> None:
    store = StateStore()
    store.finish(fixture_state())

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await _open(pilot, _screen(store=store))
        _highlight(screen, "prime.draft.value")
        await pilot.pause()
        return _detail(screen)

    detail = run_app(scenario)
    assert detail.startswith("prime.draft.value")
    assert "meta — prime.draft" in detail
    assert "openai" in detail and "gpt-4o-mini" in detail
    assert "↑120 ↓340" in detail


def test_y_copies_a_template_usable_path(run_app: Any) -> None:
    store = StateStore()
    store.finish(fixture_state())

    async def scenario(pilot: Pilot[Any]) -> tuple[str, str]:
        screen = await _open(pilot, _screen(store=store))
        _highlight(screen, "prime.over_items.iter_0.handle.value")
        await pilot.pause()
        screen.query_one("#runs-tree", Tree).focus()
        await pilot.press("y")
        await pilot.pause()
        return screen.copied_path, screen.status_text

    copied, status = run_app(scenario)
    assert copied == "prime.over_items.iter_0.handle.value"
    assert status == f"Copied {copied}"


def test_an_empty_store_says_where_state_comes_from(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[str, str]:
        screen = await _open(pilot, _screen())
        return screen.status_text, _detail(screen)

    status, detail = run_app(scenario)
    assert status == EMPTY_SOURCE
    assert EMPTY_SOURCE in detail


# -- live mode ----------------------------------------------------------------


def test_the_tree_grows_as_snapshots_arrive(run_app: Any) -> None:
    """Live mode: the view polls the store, the run never waits on the view."""
    store = StateStore()

    async def scenario(pilot: Pilot[Any]) -> tuple[str, str, str]:
        screen = await _open(pilot, _screen(store=store))
        store.begin(label="demo.yml")
        store.publish({"prime": {"draft": {"value": "first", "meta": {"created_at": "x"}}}})
        await _settle(pilot)
        early = screen.tree_text
        mode = screen.mode
        store.publish(
            {
                "prime": {
                    "draft": {"value": "first", "meta": {"created_at": "x"}},
                    "polish": {"value": "second", "meta": {"created_at": "y"}},
                }
            }
        )
        await _settle(pilot)
        return early, mode, screen.tree_text

    early, mode, late = run_app(scenario)
    assert mode == LIVE
    assert "draft" in early and "polish" not in early
    assert "polish" in late


def test_the_cursor_stays_put_across_a_live_repaint(run_app: Any) -> None:
    """Inspecting a value mid-run must survive the next snapshot."""
    store = StateStore()
    store.begin(label="demo.yml")
    store.publish({"prime": {"draft": {"value": "first"}}})

    async def scenario(pilot: Pilot[Any]) -> tuple[str, str]:
        screen = await _open(pilot, _screen(store=store))
        _highlight(screen, "prime.draft.value")
        await pilot.pause()
        before = screen.selected_node.path if screen.selected_node else ""
        store.publish({"prime": {"draft": {"value": "first"}, "polish": {"value": "next"}}})
        await _settle(pilot)
        return before, screen.selected_node.path if screen.selected_node else ""

    before, after = run_app(scenario)
    assert before == after == "prime.draft.value"


def test_an_expanded_node_stays_expanded_across_a_live_repaint(run_app: Any) -> None:
    """Reading down into a value mid-run must not be undone by the next snapshot."""
    store = StateStore()
    store.begin(label="demo.yml")
    store.publish({"prime": {"draft": {"value": "first"}}})

    def expanded(screen: RunsScreen) -> set[str]:
        tree = screen.query_one("#runs-tree", Tree)
        found: set[str] = set()
        stack = list(tree.root.children)
        while stack:
            node = stack.pop()
            if node.is_expanded and node.data:
                found.add(str(node.data))
            stack.extend(node.children)
        return found

    async def scenario(pilot: Pilot[Any]) -> tuple[set[str], set[str]]:
        screen = await _open(pilot, _screen(store=store))
        _highlight(screen, "prime.draft.value")
        await pilot.pause()
        before = expanded(screen)
        store.publish({"prime": {"draft": {"value": "first"}, "polish": {"value": "next"}}})
        await _settle(pilot)
        return before, expanded(screen)

    before, after = run_app(scenario)
    assert "prime.draft" in before
    assert "prime.draft" in after


def test_a_run_launched_from_the_run_view_is_inspectable_here(
    run_app: Any, tmp_path: Path
) -> None:
    """End to end: the Run view's own event stream feeds this view."""
    orch = tmp_path / "demo.yml"
    orch.write_text(
        yaml.safe_dump(
            {
                "adapter": "scripted",
                "model": "test",
                "effects": [{"type": "prompt", "name": "draft", "template": "hello"}],
            }
        ),
        encoding="utf-8",
    )

    class ScriptedAdapter:
        name = "scripted"

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            return GenerateResult(text=prompt, raw={}, tokens_sent=3, tokens_received=5)

    run_screen = RunScreen(
        RUN_SPEC,
        config=CircuitryConfig(),
        choices=[OrchestrationChoice("demo", "demo.yml", orch, "local")],
        adapter=ScriptedAdapter(),
    )

    async def scenario(pilot: Pilot[Any]) -> str:
        await pilot.app.push_screen(run_screen)
        await pilot.pause()
        run_screen.query_one("#run-orchestration").value = "demo"
        await pilot.pause()
        run_screen.action_launch()
        for _ in range(200):
            await pilot.pause(0.05)
            if run_screen.last_result is not None:
                break
        assert run_screen.last_result is not None and run_screen.last_result.ok
        # Navigating to the inspector is what a person does next.
        inspector = RunsScreen(RUNS_SPEC, last_run=NO_LAST_RUN)
        await pilot.app.switch_screen(inspector)
        await pilot.pause()
        return inspector.tree_text

    tree = run_app(scenario)
    assert "draft" in tree
    assert "value" in tree


# -- file mode ----------------------------------------------------------------


def test_a_saved_out_file_opens_into_the_inspector(run_app: Any, tmp_path: Path) -> None:
    path = tmp_path / "yesterday.json"
    path.write_text(json.dumps(fixture_state()), encoding="utf-8")

    async def scenario(pilot: Pilot[Any]) -> tuple[str, str, str]:
        screen = await _open(pilot, _screen())
        box = screen.query_one("#runs-file", Input)
        box.focus()
        box.value = str(path)
        await pilot.press("enter")
        await pilot.pause()
        return screen.mode, screen.status_text, screen.tree_text

    mode, status, tree = run_app(scenario)
    assert mode == FILE
    assert str(path) in status
    assert "over_items" in tree


def test_malformed_json_gets_an_error_state_not_a_traceback(
    run_app: Any, tmp_path: Path
) -> None:
    path = tmp_path / "half-written.json"
    path.write_text('{"prime": {"draft":', encoding="utf-8")

    async def scenario(pilot: Pilot[Any]) -> tuple[str, str, str, bool]:
        screen = await _open(pilot, _screen())
        screen.open_file(path)
        await pilot.pause()
        error = screen.query_one("#runs-error", Static)
        return screen.status_text, _detail(screen), screen.tree_text, error.display

    status, detail, tree, shown = run_app(scenario)
    assert "did not open" in status
    assert "Not valid JSON" in detail and "live-state" in detail
    assert tree == ""
    assert shown


def test_file_mode_does_not_follow_the_running_run(run_app: Any, tmp_path: Path) -> None:
    """Opening yesterday's file must not be overwritten by today's run."""
    path = tmp_path / "yesterday.json"
    path.write_text(json.dumps({"prime": {"archived": {"value": 1}}}), encoding="utf-8")
    store = StateStore()

    async def scenario(pilot: Pilot[Any]) -> tuple[str, str]:
        screen = await _open(pilot, _screen(store=store))
        screen.open_file(path)
        await pilot.pause()
        store.begin(label="today.yml")
        store.publish({"prime": {"fresh": {"value": 2}}})
        await _settle(pilot)
        held = screen.tree_text
        # Esc leaves file mode and hands the view back to the run.
        screen.query_one("#runs-tree", Tree).focus()
        await pilot.press("escape")
        await pilot.pause()
        return held, screen.tree_text

    held, released = run_app(scenario)
    assert "archived" in held and "fresh" not in held
    assert "fresh" in released


# -- replaying the last run ---------------------------------------------------


def _stash(orch: Path, **extra: Any) -> LastRun:
    args = {"orchestration": str(orch), "env_vars": ["topic=cats"], **extra}
    return LastRun(path=Path("last-run.json"), args=args)


def test_replaying_the_last_run_fills_the_tree_as_it_goes(
    run_app: Any, tmp_path: Path
) -> None:
    orch = tmp_path / "demo.yml"
    orch.write_text("effects: []\n", encoding="utf-8")
    seen: dict[str, Any] = {}

    def runner(request: RunRequest) -> RunResult:
        """Stand in for runtime_shim.run: publish a snapshot, then land."""
        seen["request"] = request
        state = {"prime": {"draft": {"value": "replayed", "meta": {"created_at": "x"}}}}
        if request.state_observer is not None:
            request.state_observer(state)
        return RunResult(ok=True, state=state, warnings=[])

    async def scenario(pilot: Pilot[Any]) -> tuple[str, str]:
        screen = await _open(pilot, _screen(last_run=_stash(orch), runner=runner))
        screen.action_replay()
        for _ in range(100):
            await pilot.pause(0.05)
            if screen.status_text.endswith("replay finished"):
                break
        return screen.tree_text, screen.status_text

    tree, status = run_app(scenario)
    request = seen["request"]
    assert request.orchestration_path == orch
    assert request.initial_state == {"topic": "cats"}
    # A keypress in a browser must not overwrite the artefacts on disk.
    assert request.out_path is None and request.live_state_path is None
    assert "replayed" in tree
    assert "replay finished" in status


def test_replay_merges_a_stashed_state_file_under_its_inline_values(
    run_app: Any, tmp_path: Path
) -> None:
    """``-e`` beats ``--state``, the way ``cof run`` ranks them."""
    orch = tmp_path / "demo.yml"
    orch.write_text("effects: []\n", encoding="utf-8")
    state_file = tmp_path / "in.json"
    state_file.write_text(json.dumps({"topic": "dogs", "tone": "dry"}), encoding="utf-8")
    seen: dict[str, Any] = {}

    def runner(request: RunRequest) -> RunResult:
        seen["request"] = request
        return RunResult(ok=True, state={}, warnings=[])

    async def scenario(pilot: Pilot[Any]) -> None:
        screen = await _open(
            pilot, _screen(last_run=_stash(orch, state=str(state_file)), runner=runner)
        )
        screen.action_replay()
        for _ in range(100):
            await pilot.pause(0.05)
            if "request" in seen:
                break

    run_app(scenario)
    assert seen["request"].initial_state == {"topic": "cats", "tone": "dry"}


def test_replay_carries_the_stashed_adapter_and_model(run_app: Any, tmp_path: Path) -> None:
    orch = tmp_path / "demo.yml"
    orch.write_text("effects: []\n", encoding="utf-8")
    seen: dict[str, Any] = {}

    def runner(request: RunRequest) -> RunResult:
        seen["request"] = request
        return RunResult(ok=True, state={}, warnings=[])

    async def scenario(pilot: Pilot[Any]) -> None:
        screen = await _open(
            pilot,
            _screen(
                last_run=_stash(orch, adapter="ollama", model="llama3.1:8b", dry_run=True),
                runner=runner,
            ),
        )
        screen.action_replay()
        for _ in range(100):
            await pilot.pause(0.05)
            if "request" in seen:
                break

    run_app(scenario)
    request = seen["request"]
    assert request.adapter_override == "ollama"
    assert request.model_override == "llama3.1:8b"
    assert request.dry_run is True


def test_replay_refuses_a_run_that_stashed_redacted_secrets(
    run_app: Any, tmp_path: Path
) -> None:
    orch = tmp_path / "demo.yml"
    orch.write_text("effects: []\n", encoding="utf-8")
    called: list[RunRequest] = []

    def runner(request: RunRequest) -> RunResult:  # pragma: no cover - must not run
        called.append(request)
        return RunResult(ok=True, state={}, warnings=[])

    stash = _stash(orch, env_vars=[f"api_key={REDACTED}"])

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await _open(pilot, _screen(last_run=stash, runner=runner))
        screen.action_replay()
        await pilot.pause()
        return screen.status_text

    status = run_app(scenario)
    assert "redacted" in status
    assert not called


def test_replay_says_so_when_there_is_nothing_stashed(run_app: Any, tmp_path: Path) -> None:
    # Read a stash path that does not exist rather than injecting the answer:
    # this is the "no last-run.json on this machine" case end to end.
    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await _open(
            pilot, _screen(last_run=None, last_run_path=tmp_path / "last-run.json")
        )
        screen.action_replay()
        await pilot.pause()
        return screen.status_text

    assert "No previous run stashed" in run_app(scenario)


def test_replay_reports_a_failed_run(run_app: Any, tmp_path: Path) -> None:
    orch = tmp_path / "demo.yml"
    orch.write_text("effects: []\n", encoding="utf-8")

    def runner(request: RunRequest) -> RunResult:
        return RunResult(ok=False, state={}, warnings=[], error="adapter exploded")

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await _open(pilot, _screen(last_run=_stash(orch), runner=runner))
        screen.action_replay()
        for _ in range(100):
            await pilot.pause(0.05)
            if "failed" in screen.status_text:
                break
        return screen.status_text

    assert "adapter exploded" in run_app(scenario)


def test_a_vanished_orchestration_is_reported_not_raised(run_app: Any, tmp_path: Path) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await _open(pilot, _screen(last_run=_stash(tmp_path / "gone.yml")))
        screen.action_replay()
        await pilot.pause()
        return screen.status_text

    assert "gone.yml" in run_app(scenario)


# -- snapshots ----------------------------------------------------------------

#: A stash with fixed contents, so the last-run line renders the same everywhere.
SNAPSHOT_STASH = LastRun(
    path=Path("last-run.json"),
    args={"orchestration": "demo.yml", "adapter": "openai", "model": "gpt-4o-mini"},
)


def test_post_run_snapshot(run_app: Any, capture_frame: Any, snapshot: Any) -> None:
    store = StateStore()
    store.label = "demo.yml"
    store.finish(fixture_state())

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await _open(pilot, _screen(store=store, last_run=SNAPSHOT_STASH))
        _highlight(screen, "prime.draft.value")
        await pilot.pause()
        return str(capture_frame(pilot.app))

    snapshot.assert_match(run_app(scenario, size=(100, 30)), "runs-post-run-100x30")


def test_live_snapshot(run_app: Any, capture_frame: Any, snapshot: Any) -> None:
    store = StateStore()
    store.begin(label="demo.yml")
    state = fixture_state()
    # Mid-run: the loop has one iteration and the second effect has not run.
    del state["prime"]["over_items"]["iter_1"]
    del state["prime"]["helper"]
    del state["prime"]["gate"]
    store.publish(state)

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await _open(pilot, _screen(store=store, last_run=SNAPSHOT_STASH))
        _highlight(screen, "prime.over_items.iter_0.handle")
        await pilot.pause()
        return str(capture_frame(pilot.app))

    snapshot.assert_match(run_app(scenario, size=(100, 30)), "runs-live-100x30")


def test_file_mode_snapshots(
    run_app: Any,
    capture_frame: Any,
    snapshot: Any,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Relative paths, so the frame does not carry a temp directory into a snapshot."""
    monkeypatch.chdir(tmp_path)
    (tmp_path / "yesterday.json").write_text(json.dumps(fixture_state()), encoding="utf-8")
    (tmp_path / "half-written.json").write_text('{"prime": {"draft":', encoding="utf-8")

    def frame(name: str) -> str:
        async def scenario(pilot: Pilot[Any]) -> str:
            screen = await _open(pilot, _screen(last_run=SNAPSHOT_STASH))
            screen.open_file(Path(name))
            await pilot.pause()
            return str(capture_frame(pilot.app))

        return str(run_app(scenario, size=(100, 30)))

    snapshot.assert_match(frame("yesterday.json"), "runs-file-100x30")
    snapshot.assert_match(frame("half-written.json"), "runs-file-error-100x30")


# -- chrome -------------------------------------------------------------------


def test_the_view_is_reachable_from_home(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[str, bool]:
        await pilot.press("4")
        await pilot.pause()
        screen = pilot.app.screen
        return pilot.app.current_view().slug, isinstance(screen, RunsScreen)

    assert run_app(scenario) == ("runs", True)


def test_the_app_owns_one_store_per_app(run_app: Any) -> None:
    """Two apps must not share a run store (a stale run would leak across)."""

    async def scenario(pilot: Pilot[Any]) -> StateStore:
        return pilot.app.run_states

    first, second = run_app(scenario), run_app(scenario)
    assert first is not second
