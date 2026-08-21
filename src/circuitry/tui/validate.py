"""Validate screen — point it at an orchestration file, get every problem.

``cof check`` stops at the first gate that trips, because all it owes the shell
is an exit code. Someone staring at a file they are trying to fix wants the
whole list, so this view runs every gate independently
(:func:`~circuitry.tui.diagnostics.validate_report`) and groups what comes back
by class: schema, allowlist, compile, cycle, preflight. Preflight rows carry the
same next-step sentences the Doctor view shows.

Validation reads files and may probe the network, so it runs in a worker and
the view sits in a ``Validating…`` state until the report lands.
"""

from __future__ import annotations

from collections.abc import Callable
from pathlib import Path
from typing import TYPE_CHECKING, ClassVar

from textual.app import ComposeResult
from textual.binding import Binding, BindingType
from textual.containers import Vertical
from textual.widgets import Input, Static

from .diagnostics import (
    KIND_LABELS,
    ValidationIssue,
    ValidationReport,
    validate_report,
)
from .screens import ViewScreen, ViewSpec

if TYPE_CHECKING:  # pragma: no cover - typing only
    from textual.events import Mount

__all__ = ["ValidateScreen", "default_validator", "issue_lines"]

EMPTY_STATE = "Type the path to an orchestration file and press Enter."
PENDING_STATE = "Validating…"
OK_STATE = "No problems found."


def default_validator(path: Path) -> ValidationReport:
    """Validate against the config ``cof check`` would resolve from disk."""
    from ..cli.config import resolve_config

    return validate_report(path, config=resolve_config())


def issue_lines(issue: ValidationIssue) -> list[str]:
    """The issue itself, then one indented line per next step."""
    return [f"  {issue.line()}", *(f"      → {hint}" for hint in issue.hints)]


class IssueList(Vertical):
    """The report body: a heading per error class, its issues underneath."""

    DEFAULT_CSS = """
    IssueList { height: auto; }
    IssueList .issue-kind { text-style: bold; color: $warning; margin-top: 1; }
    IssueList .issue-body { height: auto; }
    IssueList .issue-ok { color: $success; }
    IssueList .issue-skipped { color: $text-muted; margin-top: 1; }
    IssueList .issue-warning { color: $warning; }
    """

    def show(self, report: ValidationReport | None) -> None:
        """Replace the contents with ``report`` (``None`` clears the panel)."""
        self.remove_children()
        if report is None:
            return
        if report.ok:
            self.mount(Static(OK_STATE, classes="issue-ok"))
        if report.warnings:
            # Advisory, so it gets its own heading rather than joining the
            # error classes — none of these make the file invalid.
            self.mount(
                Static(f"Warnings ({len(report.warnings)})", classes="issue-kind")
            )
            self.mount(
                Static(
                    "\n".join(f"  {w}" for w in report.warnings),
                    classes="issue-warning",
                    markup=False,
                )
            )
        for kind in report.kinds():
            issues = report.of_kind(kind)
            self.mount(
                Static(f"{KIND_LABELS[kind]} ({len(issues)})", classes="issue-kind")
            )
            body = "\n".join(
                line for issue in issues for line in issue_lines(issue)
            )
            # markup=False: validator messages quote schema patterns, and
            # "[A-Za-z_]" is not a style tag.
            self.mount(Static(body, classes="issue-body", markup=False))
        if report.skipped:
            skipped = ", ".join(KIND_LABELS[kind].lower() for kind in report.skipped)
            self.mount(
                Static(f"Not checked: {skipped}.", classes="issue-skipped")
            )


class ValidateScreen(ViewScreen):
    """File path in, every validation error out."""

    DEFAULT_CSS = """
    ValidateScreen #validate-path { margin-bottom: 1; }
    ValidateScreen #validate-status { color: $text-muted; }
    """

    BINDINGS: ClassVar[list[BindingType]] = [
        Binding("ctrl+r", "revalidate", "Re-validate"),
    ]

    def __init__(
        self,
        spec: ViewSpec,
        *,
        validator: Callable[[Path], ValidationReport] | None = None,
        path: Path | None = None,
    ) -> None:
        super().__init__(spec)
        self._validate = validator or default_validator
        self._path = path
        self.report: ValidationReport | None = None
        self._closing = False

    def compose_body(self) -> ComposeResult:
        yield Static(self.spec.name, classes="view-title")
        yield Static(self.spec.blurb, classes="view-blurb")
        yield Input(
            value=str(self._path) if self._path else "",
            placeholder="path/to/orchestration.yml",
            id="validate-path",
        )
        yield Static(EMPTY_STATE, id="validate-status", markup=False)
        yield IssueList(id="validate-issues")

    def _on_mount(self, event: Mount) -> None:
        super()._on_mount(event)
        if self._path is not None:
            self.validate_path(self._path)

    def on_unmount(self) -> None:
        """Stop a report from being posted into a screen that is going away."""
        self._closing = True

    # -- actions -------------------------------------------------------------

    def on_input_submitted(self, event: Input.Submitted) -> None:
        """Enter in the path box validates whatever is in it."""
        event.stop()
        text = event.value.strip()
        if not text:
            self._set_status(EMPTY_STATE)
            self._show(None)
            return
        self.validate_path(Path(text).expanduser())

    def action_revalidate(self) -> None:
        """Run the last path again — the file changed under you, usually."""
        if self._path is not None:
            self.validate_path(self._path)

    def validate_path(self, path: Path) -> None:
        """Kick off validation of ``path`` in a worker."""
        self._path = path
        self._show(None)
        self._set_status(f"{PENDING_STATE}  {path}")
        self.run_worker(self._run, thread=True, exclusive=True, group="validate")

    # -- worker --------------------------------------------------------------

    def _run(self) -> None:
        path = self._path
        if path is None:
            return
        try:
            report = self._validate(path)
        except Exception as exc:
            report = ValidationReport(
                path, (ValidationIssue("load", f"{type(exc).__name__}: {exc}"),)
            )
        if self._closing:
            return
        try:
            self.app.call_from_thread(self._finish, report)
        except RuntimeError:
            # The app stopped between the report finishing and the hand-off.
            self._closing = True

    def _finish(self, report: ValidationReport) -> None:
        self.report = report
        count = len(report.issues)
        summary = OK_STATE if report.ok else f"{count} problem{'s' if count > 1 else ''}"
        self._set_status(f"{summary}  {report.path}")
        self._show(report)

    # -- rendering -----------------------------------------------------------

    def _set_status(self, text: str) -> None:
        self.query_one("#validate-status", Static).update(text)

    def _show(self, report: ValidationReport | None) -> None:
        self.query_one("#validate-issues", IssueList).show(report)
