"""Validate view: a path in, every class of error out.

The broken fixture trips schema, compile, cycle and preflight at once, which is
the point — the CLI stops at the first gate, and this view exists because a
person fixing a file wants the rest of the list too.
"""

from __future__ import annotations

import asyncio
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

pytest.importorskip("textual")

from textual.pilot import Pilot
from textual.widgets import Input

from circuitry.cli.config import CircuitryConfig
from circuitry.tui.app import CircuitryApp
from circuitry.tui.diagnostics import ValidationIssue, ValidationReport, validate_report
from circuitry.tui.screens import VIEWS, CircuitryScreen
from circuitry.tui.validate import ValidateScreen

FIXTURES = Path(__file__).parent / "fixtures"
BROKEN = FIXTURES / "broken.yml"
VALID = FIXTURES / "valid.yml"


def spec_for(slug: str) -> Any:
    return next(spec for spec in VIEWS if spec.slug == slug)


class ViewApp(CircuitryApp):
    """The real shell with one view screen at the bottom of the stack."""

    def __init__(self, screen: CircuitryScreen) -> None:
        super().__init__()
        self._view = screen

    def get_default_screen(self) -> CircuitryScreen:
        return self._view


def offline_validator(path: Path) -> ValidationReport:
    """Validate with a config, so the allowlist and preflight gates both run."""
    return validate_report(path, config=CircuitryConfig())


def validate_app(
    *,
    path: Path | None = None,
    validator: Callable[[Path], ValidationReport] = offline_validator,
) -> ViewApp:
    return ViewApp(
        ValidateScreen(spec_for("validate"), validator=validator, path=path)
    )


async def settle(pilot: Pilot[Any], done: Callable[[], bool], tries: int = 200) -> bool:
    for _ in range(tries):
        if done():
            return True
        await pilot.pause()
        await asyncio.sleep(0.01)
    return done()


def reported(pilot: Pilot[Any]) -> bool:
    screen: ValidateScreen = pilot.app.screen  # type: ignore[assignment]
    return screen.report is not None


# -- the four error classes ----------------------------------------------------


def test_a_broken_file_renders_every_error_class(run_app: Any, capture_frame: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        await settle(pilot, lambda: reported(pilot))
        return capture_frame(pilot.app)

    frame = run_app(scenario, app=validate_app(path=BROKEN), size=(110, 50))
    assert "Schema (1)" in frame
    assert "Compile (1)" in frame
    assert "Cycle (1)" in frame
    assert "Preflight (1)" in frame


def test_each_class_shows_its_own_message(run_app: Any, capture_frame: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        await settle(pilot, lambda: reported(pilot))
        return capture_frame(pilot.app)

    frame = run_app(scenario, app=validate_app(path=BROKEN), size=(140, 60))
    assert "bad name" in frame  # schema + compile
    assert "broken.yml" in frame  # the cycle names the file
    assert "definitely_not_an_adapter" in frame  # preflight


def test_a_clean_file_says_so(run_app: Any, capture_frame: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        await settle(pilot, lambda: reported(pilot))
        return capture_frame(pilot.app)

    frame = run_app(
        scenario,
        app=validate_app(path=VALID, validator=lambda p: validate_report(p, skip_preflight=True)),
        size=(110, 40),
    )
    assert "No problems found." in frame


def test_gates_that_did_not_run_are_named(run_app: Any, capture_frame: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        await settle(pilot, lambda: reported(pilot))
        return capture_frame(pilot.app)

    frame = run_app(
        scenario,
        app=validate_app(path=VALID, validator=lambda p: validate_report(p, skip_preflight=True)),
        size=(110, 40),
    )
    assert "Not checked:" in frame
    assert "preflight" in frame


# -- driving it ----------------------------------------------------------------


def test_typing_a_path_validates_it(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[str, tuple[str, ...]]:
        pilot.app.screen.query_one("#validate-path", Input).focus()
        await pilot.pause()
        for key in str(BROKEN):
            await pilot.press(key)
        await pilot.press("enter")
        await settle(pilot, lambda: reported(pilot))
        screen: ValidateScreen = pilot.app.screen  # type: ignore[assignment]
        assert screen.report is not None
        return screen.report.path.name, screen.report.kinds()

    name, kinds = run_app(scenario, app=validate_app(), size=(120, 40))
    assert name == "broken.yml"
    assert kinds == ("schema", "compile", "cycle", "preflight")


def test_an_empty_path_returns_to_the_empty_state(run_app: Any, capture_frame: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        await settle(pilot, lambda: reported(pilot))
        screen: ValidateScreen = pilot.app.screen  # type: ignore[assignment]
        screen.query_one("#validate-path", Input).value = ""
        screen.query_one("#validate-path", Input).focus()
        await pilot.pause()
        await pilot.press("enter")
        await pilot.pause()
        return capture_frame(pilot.app)

    frame = run_app(scenario, app=validate_app(path=VALID), size=(110, 40))
    assert "Type the path to an orchestration file" in frame


def test_revalidate_runs_the_file_again(run_app: Any) -> None:
    calls: list[Path] = []

    def counting(path: Path) -> ValidationReport:
        calls.append(path)
        return offline_validator(path)

    async def scenario(pilot: Pilot[Any]) -> int:
        await settle(pilot, lambda: reported(pilot))
        await pilot.press("ctrl+r")
        await settle(pilot, lambda: len(calls) >= 2)
        return len(calls)

    assert run_app(scenario, app=validate_app(path=BROKEN, validator=counting)) == 2


def test_a_missing_file_is_reported_not_crashed(run_app: Any, capture_frame: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        await settle(pilot, lambda: reported(pilot))
        return capture_frame(pilot.app)

    frame = run_app(
        scenario, app=validate_app(path=FIXTURES / "nope.yml"), size=(110, 40)
    )
    assert "Load (1)" in frame


def test_a_validator_that_raises_becomes_a_load_error(run_app: Any, capture_frame: Any) -> None:
    def exploding(path: Path) -> ValidationReport:
        raise RuntimeError("validator blew up")

    async def scenario(pilot: Pilot[Any]) -> str:
        await settle(pilot, lambda: reported(pilot))
        return capture_frame(pilot.app)

    frame = run_app(
        scenario, app=validate_app(path=BROKEN, validator=exploding), size=(110, 40)
    )
    assert "validator blew up" in frame


def test_preflight_problems_carry_their_next_step(run_app: Any, capture_frame: Any) -> None:
    def unreachable(path: Path) -> ValidationReport:
        config = CircuitryConfig(
            runtime={"adapters": {"ollama": {"base_url": "http://127.0.0.1:1"}}}
        )
        return validate_report(VALID, config=config)

    async def scenario(pilot: Pilot[Any]) -> str:
        await settle(pilot, lambda: reported(pilot))
        return capture_frame(pilot.app)

    frame = run_app(
        scenario, app=validate_app(path=VALID, validator=unreachable), size=(120, 40)
    )
    assert "→ Start the service at" in frame


# -- snapshot ------------------------------------------------------------------


#: A hand-written report for the snapshot. The real messages are asserted
#: against the real fixture above; pinning the *layout* to canned text keeps
#: the snapshot from churning every time an adapter is added to the registry
#: or the suite runs from a different directory.
SNAPSHOT_REPORT = ValidationReport(
    Path("tests/tui/fixtures/broken.yml"),
    (
        ValidationIssue("schema", "'bad name' does not match '^[A-Za-z_]*$'", "/effects/0/name"),
        ValidationIssue("compile", "Invalid name 'bad name' for prompt at 'prime.effects[0]'."),
        ValidationIssue("cycle", "broken.yml → broken.yml"),
        ValidationIssue(
            "preflight",
            "missing host:http://localhost:11434",
            "adapter:ollama",
            ("Start the service at http://localhost:11434, or point the config at a host that is up.",),
        ),
    ),
)


def test_validate_snapshot(run_app: Any, capture_frame: Any, snapshot: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        await settle(pilot, lambda: reported(pilot))
        return capture_frame(pilot.app)

    frame = run_app(
        scenario,
        app=validate_app(
            path=SNAPSHOT_REPORT.path, validator=lambda _path: SNAPSHOT_REPORT
        ),
        size=(100, 30),
    )
    snapshot.assert_match(frame, "validate-100x30")
