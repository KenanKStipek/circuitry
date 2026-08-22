"""Latency: a keypress must produce the next frame, not a stalled one.

Two claims, both measured rather than asserted by inspection:

1. **Navigation is next-frame.** Opening a view builds its screen on the UI
   thread, so anything that view does eagerly — scanning the filesystem for
   orchestrations, reading a library manifest — is paid for by the keystroke
   that opened it. The budget here is deliberately loose (it has to hold on a
   loaded CI box), but it is tight enough to catch a view that starts doing
   real work in ``__init__``.

2. **Long work does not hold the keyboard.** A run that blocks inside the
   adapter must leave navigation, cancelling and the help overlay all live.
   That is what "never block input" means; a spinner over a frozen app is
   not a loading state.

Wall-clock assertions are a compromise. The alternative — asserting that no
view calls a blocking function on mount — reads well and catches nothing,
because the blocking call is always three frames down someone else's stack.
"""

from __future__ import annotations

import asyncio
import threading
import time
from dataclasses import dataclass, field
from typing import Any

import pytest

pytest.importorskip("textual")

from textual.pilot import Pilot

from circuitry.tui.app import CircuitryApp
from circuitry.tui.screens import VIEWS

#: Ceiling for building and painting one view, in seconds. A person notices
#: about 100ms; this leaves an order of magnitude of headroom for a shared
#: CI runner and still fails loudly if a view starts blocking on the network.
FRAME_BUDGET = 1.5

#: Ceiling for a keypress handled while a run is blocked in the adapter.
BUSY_BUDGET = 1.5


@dataclass
class BlockingAdapter:
    """Sits inside ``generate`` until released, like a slow backend would."""

    entered: threading.Event = field(default_factory=threading.Event)
    release: threading.Event = field(default_factory=threading.Event)
    name: str = "blocking"

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> Any:
        from circuitry.adapters.base import GenerateResult

        self.entered.set()
        self.release.wait(timeout=10)
        return GenerateResult(text=prompt, raw={})


@pytest.mark.parametrize("spec", VIEWS, ids=[spec.slug for spec in VIEWS])
def test_opening_a_view_renders_the_next_frame(run_app: Any, spec: Any) -> None:
    """The keystroke that opens a view also paints it."""

    async def scenario(pilot: Pilot[Any]) -> float:
        start = time.perf_counter()
        await pilot.press(spec.key)
        await pilot.pause()
        return time.perf_counter() - start

    elapsed = run_app(scenario, size=(100, 30))
    assert elapsed < FRAME_BUDGET, f"{spec.slug} took {elapsed:.3f}s to open"


def test_walking_every_view_stays_responsive(run_app: Any) -> None:
    """Tab all the way round the views; no single hop may stall."""

    async def scenario(pilot: Pilot[Any]) -> float:
        worst = 0.0
        for _ in range(len(VIEWS) + 1):
            start = time.perf_counter()
            await pilot.press("tab")
            await pilot.pause()
            worst = max(worst, time.perf_counter() - start)
        return worst

    assert run_app(scenario, size=(100, 30)) < FRAME_BUDGET


def test_a_blocked_run_still_answers_the_keyboard(run_app: Any, tmp_path: Any) -> None:
    """A run stuck in the adapter must not take the app down with it.

    The run is launched, the adapter is held inside ``generate``, and the app
    is then asked to do the three things a stuck user reaches for: open help,
    close it, and leave the view. All three have to land while the worker is
    still blocked.
    """
    import yaml

    from circuitry.cli.config import CircuitryConfig
    from circuitry.tui.launch import OrchestrationChoice
    from circuitry.tui.run_view import RunScreen

    spec = next(view for view in VIEWS if view.slug == "run")
    path = tmp_path / "slow.yml"
    path.write_text(
        yaml.dump(
            {
                "adapter": "blocking",
                "model": "m",
                "effects": [{"type": "prompt", "name": "wait", "template": "hi"}],
            },
            sort_keys=False,
        ),
        encoding="utf-8",
    )
    adapter = BlockingAdapter()

    def build(_: Any) -> RunScreen:
        return RunScreen(
            spec,
            config=CircuitryConfig(),
            choices=[
                OrchestrationChoice(key=str(path), label=path.name, path=path, source="local")
            ],
            adapter=adapter,
        )

    class SlowRunApp(CircuitryApp):
        """The real shell, with Run wired to an adapter that will not answer."""

        def on_mount(self) -> None:
            self.push_screen(build(spec))

    async def scenario(pilot: Pilot[Any]) -> tuple[bool, float, str]:
        await pilot.pause()
        # Ctrl-R launches; the adapter then parks the worker thread.
        await pilot.press("ctrl+r")
        deadline = asyncio.get_event_loop().time() + 10
        while not adapter.entered.is_set():
            if asyncio.get_event_loop().time() > deadline:
                break
            await pilot.pause()
            await asyncio.sleep(0.01)
        blocked = adapter.entered.is_set()

        start = time.perf_counter()
        await pilot.press("question_mark")  # help opens over a blocked run
        await pilot.pause()
        opened = type(pilot.app.screen).__name__
        await pilot.press("escape")  # and closes again
        await pilot.pause()
        await pilot.press("1")  # and the view keys still navigate
        await pilot.pause()
        worst = time.perf_counter() - start

        adapter.release.set()
        return blocked, worst, opened

    blocked, worst, opened = run_app(scenario, app=SlowRunApp(), size=(100, 30))
    assert blocked, "the adapter never blocked — the test proved nothing"
    assert opened == "HelpOverlay"
    assert worst < BUSY_BUDGET, f"input stalled for {worst:.3f}s behind a blocked run"
