"""Doctor and Settings screens — is this machine ready to run anything?

The Doctor view answers that in two panels. The top one runs the same
``check()`` walk ``cof doctor`` runs, one row per extension, translating the
``env:``/``binary:``/``host:`` grammar into a sentence telling you what to do
next. The bottom one is the effective configuration with the layer each value
came from, redacted. The Settings view is that second panel on its own.

Checks are slow — several of them open sockets — so nothing runs on the
compositor's thread. Every row is painted in a ``checking…`` state up front and
filled in as its result lands, which is what keeps the keyboard live while a
probe waits out a connection timeout.

The screens take their data through a
:class:`~circuitry.tui.diagnostics.DiagnosticsSource`, so a test hands them a
fixture machine instead of whatever the machine running the suite happens to
have installed.
"""

from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor, as_completed
from typing import TYPE_CHECKING, ClassVar

from textual.app import ComposeResult
from textual.binding import Binding, BindingType
from textual.containers import Vertical
from textual.widgets import Static

from .diagnostics import (
    CATEGORIES,
    STATE_LABELS,
    CheckTarget,
    DiagnosticsSource,
    ExtensionCheck,
    SettingRow,
    counted,
    load_diagnostics,
)
from .screens import ViewScreen, ViewSpec

if TYPE_CHECKING:  # pragma: no cover - typing only
    from textual.events import Mount

__all__ = ["CheckRow", "DoctorScreen", "EffectiveSettingsPanel", "SettingsScreen"]

#: How many checks may be in flight at once. Enough to hide the latency of a
#: handful of unreachable hosts without opening one socket per extension.
CHECK_CONCURRENCY = 8

#: Column width for the extension name, so statuses line up down the panel.
NAME_COLUMN = 22


def settings_lines(rows: tuple[SettingRow, ...]) -> list[str]:
    """Align setting rows into ``key   value   (from layer)`` lines."""
    if not rows:
        return ["No settings resolved."]
    width = min(32, max(len(row.key) for row in rows))
    return [
        f"{row.key.ljust(width)}  {row.value}  (from {row.source})" for row in rows
    ]


class CheckRow(Static):
    """One extension's status, updated in place when its check comes back."""

    DEFAULT_CSS = """
    CheckRow { height: auto; }
    CheckRow.-ok { color: $success; }
    CheckRow.-deferred { color: $text-muted; }
    CheckRow.-missing { color: $warning; }
    CheckRow.-error { color: $error; }
    CheckRow.-checking { color: $text-muted; }
    """

    STATE_CLASSES: ClassVar[tuple[str, ...]] = tuple(
        f"-{state}" for state in STATE_LABELS
    )

    def __init__(self, target: CheckTarget) -> None:
        # markup=False: a check message may contain anything, including
        # square brackets that Textual would otherwise eat as markup tags.
        super().__init__(id=f"check-{target.slug}", classes="check-row", markup=False)
        self.target = target
        self.check: ExtensionCheck = ExtensionCheck(target)

    def on_mount(self) -> None:
        self._repaint()

    def set_check(self, check: ExtensionCheck) -> None:
        """Swap in a result (or a fresh ``checking`` placeholder) and repaint."""
        self.check = check
        self._repaint()

    def text(self) -> str:
        """Two lines: the status, then the detail or next step under it."""
        head = f"{self.target.name.ljust(NAME_COLUMN)}{STATE_LABELS[self.check.state]}"
        if self.check.pending:
            return head
        return f"{head}\n    {self.check.detail}"

    def _repaint(self) -> None:
        self.remove_class(*self.STATE_CLASSES)
        self.add_class(f"-{self.check.state}")
        self.update(self.text())


class EffectiveSettingsPanel(Static):
    """The resolved configuration, one row per value, with its source layer."""

    DEFAULT_CSS = """
    EffectiveSettingsPanel { height: auto; }
    """

    def __init__(
        self,
        rows: tuple[SettingRow, ...] = (),
        *,
        id: str | None = None,
    ) -> None:
        super().__init__(id=id, markup=False)
        self.rows = rows

    def on_mount(self) -> None:
        self.show(self.rows)

    def show(self, rows: tuple[SettingRow, ...]) -> None:
        self.rows = rows
        self.update("\n".join(settings_lines(rows)))


class _DiagnosticsScreen(ViewScreen):
    """Shared plumbing: resolve the environment once, lazily."""

    def __init__(
        self,
        spec: ViewSpec,
        *,
        diagnostics: DiagnosticsSource | None = None,
    ) -> None:
        super().__init__(spec)
        self._diagnostics = diagnostics

    @property
    def diagnostics(self) -> DiagnosticsSource:
        """The environment under inspection; read from disk on first use."""
        if self._diagnostics is None:
            self._diagnostics = load_diagnostics()
        return self._diagnostics


class DoctorScreen(_DiagnosticsScreen):
    """Preflight dashboard: per-extension checks over the effective settings."""

    DEFAULT_CSS = """
    DoctorScreen .panel-title { text-style: bold; margin-top: 1; }
    DoctorScreen #doctor-summary { color: $text-muted; }
    DoctorScreen #doctor-checks { height: auto; }
    """

    BINDINGS: ClassVar[list[BindingType]] = [
        Binding("ctrl+r", "recheck", "Re-run checks"),
    ]

    def __init__(
        self,
        spec: ViewSpec,
        *,
        diagnostics: DiagnosticsSource | None = None,
    ) -> None:
        super().__init__(spec, diagnostics=diagnostics)
        self._rows: dict[str, CheckRow] = {}
        self._results: dict[str, ExtensionCheck] = {}
        self._closing = False

    # -- composition ---------------------------------------------------------

    def compose_body(self) -> ComposeResult:
        yield Static(self.spec.name, classes="view-title")
        yield Static(self.spec.blurb, classes="view-blurb")
        yield Static("Environment checks", classes="panel-title")
        yield Static("", id="doctor-summary", markup=False)
        yield Vertical(id="doctor-checks")
        yield Static("Effective settings", classes="panel-title")
        yield EffectiveSettingsPanel(id="doctor-settings")

    def _on_mount(self, event: Mount) -> None:
        super()._on_mount(event)
        self.query_one("#doctor-settings", EffectiveSettingsPanel).show(
            self.diagnostics.rows()
        )
        self.action_recheck()

    def on_unmount(self) -> None:
        """Stop results from being posted into a screen that is going away."""
        self._closing = True

    # -- checks --------------------------------------------------------------

    def action_recheck(self) -> None:
        """Repaint every row as pending and run the whole walk again."""
        targets = self.diagnostics.targets()
        self._results.clear()
        container = self.query_one("#doctor-checks", Vertical)
        if not self._rows:
            self._rows = {target.label: CheckRow(target) for target in targets}
            container.mount(*self._rows.values())
        else:
            for target in targets:
                self._rows[target.label].set_check(ExtensionCheck(target))
        self._refresh_summary()
        self.run_worker(
            self._run_checks, thread=True, exclusive=True, group="doctor-checks"
        )

    def _run_checks(self) -> None:
        """Worker body: fan the checks out, hand each result back as it lands."""
        targets = self.diagnostics.targets()
        if not targets:
            return
        with ThreadPoolExecutor(max_workers=CHECK_CONCURRENCY) as pool:
            futures = {
                pool.submit(self.diagnostics.check, target): target for target in targets
            }
            for future in as_completed(futures):
                target = futures[future]
                try:
                    check = future.result()
                except Exception as exc:
                    check = ExtensionCheck(target, "error", (), str(exc))
                if self._closing:
                    return
                self._post(check)

    def _post(self, check: ExtensionCheck) -> None:
        try:
            self.app.call_from_thread(self._apply, check)
        except RuntimeError:
            # The app stopped between the check finishing and the hand-off.
            self._closing = True

    def _apply(self, check: ExtensionCheck) -> None:
        row = self._rows.get(check.target.label)
        if row is None:
            return
        self._results[check.target.label] = check
        row.set_check(check)
        self._refresh_summary()

    def _refresh_summary(self) -> None:
        self.query_one("#doctor-summary", Static).update(self.summary())

    def summary(self) -> str:
        """One line counting each state, so the panel reads at a glance."""
        if not self._rows:
            return "Nothing to check — every extension category is empty."
        counts = dict.fromkeys(STATE_LABELS, 0)
        for check in self._results.values():
            counts[check.state] += 1
        counts["checking"] = len(self._rows) - len(self._results)
        states = " · ".join(
            f"{counts[state]} {STATE_LABELS[state]}"
            for state in STATE_LABELS
            if counts[state]
        )
        per_category = {
            category: sum(
                1 for row in self._rows.values() if row.target.category == category
            )
            for category in CATEGORIES
        }
        categories = ", ".join(
            counted(count, category)
            for category, count in per_category.items()
            if count
        )
        return f"{states}  —  {categories}"


class SettingsScreen(_DiagnosticsScreen):
    """Effective configuration on its own, with source attribution."""

    DEFAULT_CSS = """
    SettingsScreen .panel-title { text-style: bold; margin-top: 1; }
    """

    def compose_body(self) -> ComposeResult:
        yield Static(self.spec.name, classes="view-title")
        yield Static(self.spec.blurb, classes="view-blurb")
        yield Static(
            "Credentials are redacted before they reach the screen.",
            classes="view-note",
        )
        yield EffectiveSettingsPanel(id="settings-panel")

    def _on_mount(self, event: Mount) -> None:
        super()._on_mount(event)
        self.query_one("#settings-panel", EffectiveSettingsPanel).show(
            self.diagnostics.rows()
        )
