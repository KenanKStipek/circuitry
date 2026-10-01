"""Doctor and Settings views: async checks, actionable rows, redacted settings.

Every test drives a screen over a fixture machine (a fake
:class:`~circuitry.tui.diagnostics.DiagnosticsSource`) rather than the host, so
what is asserted is the view's behaviour and not whether the CI box happens to
have ffmpeg installed.
"""

from __future__ import annotations

import asyncio
import threading
from collections.abc import Callable, Iterable
from pathlib import Path
from typing import Any

import pytest

pytest.importorskip("textual")

from textual.app import App
from textual.pilot import Pilot

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.effective_settings import resolve_effective_settings
from circuitry.cli.redaction import REDACTED
from circuitry.tui.app import CircuitryApp
from circuitry.tui.diagnostics import (
    CheckTarget,
    ExtensionCheck,
    SettingRow,
    settings_rows,
)
from circuitry.tui.doctor import CheckRow, DoctorScreen, SettingsScreen
from circuitry.tui.screens import VIEWS, CircuitryScreen

#: A machine with one healthy adapter, one adapter missing an env var, one
#: runtime-injected adapter and one tool needing a binary — the demo in #23.
FIXTURE_CHECKS: tuple[ExtensionCheck, ...] = (
    ExtensionCheck(
        CheckTarget("adapter", "ollama"), "ok", (), "reachable at http://localhost:11434"
    ),
    ExtensionCheck(CheckTarget("adapter", "openai"), "missing", ("env:OPENAI_API_KEY",)),
    ExtensionCheck(
        CheckTarget("adapter", "host_claude"), "deferred", (), "runtime-injected"
    ),
    ExtensionCheck(CheckTarget("tool", "ffmpeg"), "missing", ("binary:ffmpeg",)),
)


def spec_for(slug: str) -> Any:
    return next(spec for spec in VIEWS if spec.slug == slug)


class FakeDiagnostics:
    """A canned machine. ``gate`` holds every check until the test releases it."""

    def __init__(
        self,
        checks: Iterable[ExtensionCheck] = FIXTURE_CHECKS,
        rows: tuple[SettingRow, ...] = (),
        gate: threading.Event | None = None,
        warnings: tuple[str, ...] = (),
    ) -> None:
        self._checks = {check.target: check for check in checks}
        self._rows = rows
        self._gate = gate
        self._warnings = warnings
        self.calls: list[CheckTarget] = []

    def targets(self) -> tuple[CheckTarget, ...]:
        return tuple(self._checks)

    def check(self, target: CheckTarget) -> ExtensionCheck:
        if self._gate is not None:
            self._gate.wait(timeout=5)
        self.calls.append(target)
        return self._checks[target]

    def rows(self) -> tuple[SettingRow, ...]:
        return self._rows

    def warnings(self) -> tuple[str, ...]:
        return self._warnings


class ViewApp(CircuitryApp):
    """The real shell with one view screen at the bottom of the stack."""

    def __init__(self, screen: CircuitryScreen) -> None:
        super().__init__()
        self._view = screen

    def get_default_screen(self) -> CircuitryScreen:
        return self._view


def doctor_app(diagnostics: FakeDiagnostics) -> ViewApp:
    return ViewApp(DoctorScreen(spec_for("doctor"), diagnostics=diagnostics))


async def settle(pilot: Pilot[Any], done: Callable[[], bool], tries: int = 200) -> bool:
    """Pump the event loop until ``done()`` or we run out of patience."""
    for _ in range(tries):
        if done():
            return True
        await pilot.pause()
        await asyncio.sleep(0.01)
    return done()


def rows_of(app: App[Any]) -> list[CheckRow]:
    return list(app.screen.query(CheckRow))


def all_resolved(app: App[Any]) -> bool:
    rows = rows_of(app)
    return bool(rows) and not any(row.check.pending for row in rows)


# -- states --------------------------------------------------------------------


def test_every_state_renders_its_own_row(run_app: Any, capture_frame: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        await settle(pilot, lambda: all_resolved(pilot.app))
        return capture_frame(pilot.app)

    frame = run_app(scenario, app=doctor_app(FakeDiagnostics()), size=(100, 40))
    assert "ollama" in frame
    assert "ok" in frame
    assert "deferred" in frame
    assert "missing deps" in frame


def test_a_missing_binary_says_what_to_do_about_it(run_app: Any, capture_frame: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        await settle(pilot, lambda: all_resolved(pilot.app))
        return capture_frame(pilot.app)

    frame = run_app(scenario, app=doctor_app(FakeDiagnostics()), size=(100, 40))
    assert "Install ffmpeg and make sure it is on your PATH." in frame


def test_a_missing_env_var_says_what_to_do_about_it(run_app: Any, capture_frame: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        await settle(pilot, lambda: all_resolved(pilot.app))
        return capture_frame(pilot.app)

    frame = run_app(scenario, app=doctor_app(FakeDiagnostics()), size=(100, 40))
    assert "Set the OPENAI_API_KEY environment variable" in frame


def test_the_summary_counts_what_came_back(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        await settle(pilot, lambda: all_resolved(pilot.app))
        screen: DoctorScreen = pilot.app.screen  # type: ignore[assignment]
        return screen.summary()

    summary = run_app(scenario, app=doctor_app(FakeDiagnostics()))
    assert "1 ok" in summary
    assert "1 deferred" in summary
    assert "2 missing deps" in summary
    assert "3 adapters" in summary


# -- asynchrony ----------------------------------------------------------------


def test_slow_checks_leave_a_loading_state_per_row_and_a_live_keyboard(
    run_app: Any,
) -> None:
    """A probe waiting on a socket must not cost the user their keyboard."""
    gate = threading.Event()
    diagnostics = FakeDiagnostics(gate=gate)

    async def scenario(pilot: Pilot[Any]) -> tuple[list[str], bool, bool]:
        await pilot.pause()
        pending = [row.check.state for row in rows_of(pilot.app)]
        await pilot.press("question_mark")
        await pilot.pause()
        help_opened = pilot.app.help_is_open
        await pilot.press("escape")
        await pilot.pause()
        gate.set()
        resolved = await settle(pilot, lambda: all_resolved(pilot.app))
        return pending, help_opened, resolved

    try:
        pending, help_opened, resolved = run_app(scenario, app=doctor_app(diagnostics))
    finally:
        gate.set()

    assert pending == ["checking"] * len(FIXTURE_CHECKS)
    assert help_opened, "the help overlay must still open while checks are running"
    assert resolved


def test_pending_rows_say_they_are_checking(run_app: Any, capture_frame: Any) -> None:
    gate = threading.Event()

    async def scenario(pilot: Pilot[Any]) -> str:
        await pilot.pause()
        return capture_frame(pilot.app)

    try:
        frame = run_app(
            scenario, app=doctor_app(FakeDiagnostics(gate=gate)), size=(100, 40)
        )
    finally:
        gate.set()

    assert "checking…" in frame


def test_recheck_runs_the_whole_walk_again(run_app: Any) -> None:
    diagnostics = FakeDiagnostics()

    async def scenario(pilot: Pilot[Any]) -> int:
        await settle(pilot, lambda: all_resolved(pilot.app))
        await pilot.press("ctrl+r")
        await settle(pilot, lambda: len(diagnostics.calls) >= 2 * len(FIXTURE_CHECKS))
        return len(diagnostics.calls)

    assert run_app(scenario, app=doctor_app(diagnostics)) == 2 * len(FIXTURE_CHECKS)


def test_a_check_that_raises_becomes_an_error_row(run_app: Any, capture_frame: Any) -> None:
    class Exploding(FakeDiagnostics):
        def check(self, target: CheckTarget) -> ExtensionCheck:
            raise RuntimeError("check blew up")

    async def scenario(pilot: Pilot[Any]) -> str:
        await settle(pilot, lambda: all_resolved(pilot.app))
        return capture_frame(pilot.app)

    frame = run_app(scenario, app=doctor_app(Exploding()), size=(100, 40))
    assert "error" in frame
    assert "check blew up" in frame


def test_an_empty_machine_says_so_rather_than_showing_a_blank_panel(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        await pilot.pause()
        screen: DoctorScreen = pilot.app.screen  # type: ignore[assignment]
        return screen.summary()

    assert "Nothing to check" in run_app(scenario, app=doctor_app(FakeDiagnostics(())))


# -- effective settings --------------------------------------------------------


def seeded_rows() -> tuple[SettingRow, ...]:
    config = CircuitryConfig(
        default_model="llama3.1:8b",
        default_adapter="ollama",
        runtime={"adapters": {"openai": {"api_key": "sk-livetoken0000000000000000000000"}}},
    )
    return settings_rows(resolve_effective_settings(cfg=config, orch={}))


def test_doctor_shows_the_effective_settings_with_their_sources(
    run_app: Any, capture_frame: Any
) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        await settle(pilot, lambda: all_resolved(pilot.app))
        return capture_frame(pilot.app)

    frame = run_app(
        scenario,
        app=doctor_app(FakeDiagnostics(rows=seeded_rows())),
        size=(110, 50),
    )
    assert "llama3.1:8b" in frame
    assert "(from config)" in frame


def test_a_seeded_token_is_redacted_before_it_reaches_the_screen(
    run_app: Any, capture_frame: Any
) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        await settle(pilot, lambda: all_resolved(pilot.app))
        return capture_frame(pilot.app)

    frame = run_app(
        scenario,
        app=doctor_app(FakeDiagnostics(rows=seeded_rows())),
        size=(110, 50),
    )
    assert REDACTED in frame
    assert "sk-livetoken" not in frame


def test_settings_view_renders_the_same_panel_on_its_own(
    run_app: Any, capture_frame: Any
) -> None:
    screen = SettingsScreen(
        spec_for("settings"), diagnostics=FakeDiagnostics(rows=seeded_rows())
    )

    async def scenario(pilot: Pilot[Any]) -> str:
        await pilot.pause()
        return capture_frame(pilot.app)

    frame = run_app(scenario, app=ViewApp(screen), size=(110, 40))
    assert "adapter" in frame
    assert "(from config)" in frame
    assert REDACTED in frame


# -- snapshots -----------------------------------------------------------------


def test_doctor_snapshot(run_app: Any, capture_frame: Any, snapshot: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        await settle(pilot, lambda: all_resolved(pilot.app))
        return capture_frame(pilot.app)

    frame = run_app(
        scenario,
        app=doctor_app(FakeDiagnostics(rows=seeded_rows())),
        size=(100, 30),
    )
    snapshot.assert_match(frame, "doctor-100x30")


def test_settings_snapshot(run_app: Any, capture_frame: Any, snapshot: Any) -> None:
    screen = SettingsScreen(
        spec_for("settings"), diagnostics=FakeDiagnostics(rows=seeded_rows())
    )

    async def scenario(pilot: Pilot[Any]) -> str:
        await pilot.pause()
        return capture_frame(pilot.app)

    snapshot.assert_match(
        run_app(scenario, app=ViewApp(screen), size=(100, 30)), "settings-100x30"
    )


# -- a missing/broken config degrades to a warning, never a crash (#259) -------


def _screen_text(app: Any) -> str:
    """Inline copy of ``conftest.screen_text`` — these two tests drive a
    bare ``CircuitryApp`` without the ``run_app``/``capture_frame``
    fixtures, since they need to inspect ``app.is_running`` after the pilot
    block closes."""
    strips = app.screen._compositor.render_strips()
    return "\n".join(strip.text for strip in strips)


def _lock_down_doctor_env(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """The real ``CircuitryApp`` Doctor/Settings screens below run the actual
    ``_check_extensions`` walk over every compiled-in adapter (default-open,
    no allowlist) — including ``cyberdiner``, which makes a real network
    call to a paid production service whenever ``CYBERDINER_TOKEN``/
    ``CYBERDINER_EXPO_URL`` happen to be set in the environment running the
    suite; their mere presence must never be enough on its own. Locking the
    allowlists empty and moving off the repo's own cwd keeps this hermetic
    (#265 part 5 / CLAUDE.md)."""
    monkeypatch.delenv("CIRCUITRY_MODEL", raising=False)
    monkeypatch.delenv("CIRCUITRY_ADAPTER", raising=False)
    monkeypatch.setenv("CIRCUITRY_ENABLED_ADAPTERS", "")
    monkeypatch.setenv("CIRCUITRY_ENABLED_TOOLS", "")
    monkeypatch.setenv("CIRCUITRY_ENABLED_PLUGINS", "")
    monkeypatch.chdir(tmp_path)


def test_doctor_mount_does_not_crash_on_a_stale_circuitry_config(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """The exact repro from #259: a stale/typo'd ``CIRCUITRY_CONFIG`` pointing
    at a file that does not exist used to take out the whole app via an
    unhandled ``ConfigError`` when the Doctor screen mounted. A *discovered*
    broken config degrades gracefully (``resolve_config`` itself tolerates
    it, same as ``cof run``/``cof doctor``), so the real Doctor screen — not
    a fixture one — must mount and render normally.
    """
    _lock_down_doctor_env(monkeypatch, tmp_path)
    monkeypatch.setenv("CIRCUITRY_CONFIG", "/tmp/does-not-exist/config.json")

    async def _drive() -> tuple[bool, str]:
        app = CircuitryApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await pilot.press("5")  # Doctor
            await pilot.pause()
            running = app.is_running
            frame = _screen_text(app) if running else ""
            await pilot.press("q")
            await pilot.pause()
            return running, frame

    running, frame = asyncio.run(_drive())
    assert running is True
    # Degraded to an on-screen warning, the same message `cof doctor` prints
    # for this case, rather than vanishing with no trace (#259).
    assert "does-not-exist" in frame
    assert "CIRCUITRY_CONFIG" in frame


def test_settings_mount_does_not_crash_on_a_stale_circuitry_config(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _lock_down_doctor_env(monkeypatch, tmp_path)
    monkeypatch.setenv("CIRCUITRY_CONFIG", "/tmp/does-not-exist/config.json")

    async def _drive() -> tuple[bool, str]:
        app = CircuitryApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await pilot.press("6")  # Settings
            await pilot.pause()
            running = app.is_running
            return running, _screen_text(app) if running else ""

    running, frame = asyncio.run(_drive())
    assert running is True
    assert "does-not-exist" in frame
    assert "CIRCUITRY_CONFIG" in frame


def test_doctor_screen_shows_an_error_for_a_broken_explicit_config(
    run_app: Any, capture_frame: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    """An *explicitly* named broken config is a `ConfigError` `resolve_config`
    does not degrade — the screen must catch it and show it, not crash."""
    import circuitry.tui.doctor as doctor_module
    from circuitry.cli.config import ConfigError

    def _raise() -> None:
        raise ConfigError("Config file not found: /no/such/config.json")

    monkeypatch.setattr(doctor_module, "load_diagnostics", _raise)

    screen = DoctorScreen(spec_for("doctor"))

    async def scenario(pilot: Pilot[Any]) -> str:
        await pilot.pause()
        return capture_frame(pilot.app)

    frame = run_app(scenario, app=ViewApp(screen), size=(100, 30))
    assert "Error:" in frame
    assert "Config file not found" in frame
