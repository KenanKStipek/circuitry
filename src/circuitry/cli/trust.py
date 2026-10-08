"""cof trust / cof untrust — decide which project config files may apply.

See :mod:`circuitry.cli.config_trust` for why a discovered project config
needs trusting and how trust is recorded.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import typer
from rich.console import Console
from rich.markup import escape
from rich.table import Table
from rich.text import Text

from .config import (
    DEFAULT_CONFIG_FILENAMES,
    ConfigError,
    discover_project_config,
    parse_config_bytes,
    read_config_bytes,
    trust_store_path,
)
from .config_trust import (
    TRUST_STATE_LABELS,
    TrustStoreError,
    check_trust,
    config_digest,
    flatten_settings,
    host_sensitive_reason,
    read_trust_entries,
    record_trust,
    remove_trust,
)

console = Console()


def _target_path(path: Path | None) -> Path:
    """*path*, or the project config discovered in the working directory."""
    if path is not None:
        return path
    found = discover_project_config()
    if found is None:
        looked_for = ", ".join(DEFAULT_CONFIG_FILENAMES)
        raise ConfigError(
            f"No project config in {Path.cwd()} (looked for {looked_for}). "
            "Pass the file to trust as an argument."
        )
    return found


def _fail(message: str) -> typer.Exit:
    console.print(f"[red]Error:[/red] {escape(message)}")
    return typer.Exit(code=1)


def _trust_document(path: Path, *, yes: bool, store: Path, config: Path | None) -> None:
    """Capability consent for an orchestration document (#275).

    The interactive counterpart to the in-run prompt
    (:mod:`circuitry.cli.document_consent`): shows what the document and its
    statically reachable ``use`` children need, then records a yes against
    its content digest in the same store a project config's trust lives in
    (a different top-level key, see :mod:`circuitry.cli.config_trust`) — so
    approving it here is exactly what answering "y" to the in-run prompt
    does, just ahead of time, for a CI job or a document reached through
    ``use: ref:`` from code that never prompts.
    """
    from .config import resolve_config
    from .document_consent import (
        consented_capabilities,
        record_consent,
        required_capabilities,
    )
    from .orchestration_loader import load_orchestration_file

    try:
        orch = load_orchestration_file(path)
    except (OSError, ValueError) as exc:
        raise _fail(str(exc)) from exc

    cfg = resolve_config(explicit_path=config)
    needed = required_capabilities(orch, root_path=path, runtime=cfg.runtime)
    # Shared with `cli.document_consent`/`core.use` (#396) — includes every
    # `{file: ...}` prompt source *orch* references, so editing one of those
    # (not just the orchestration YAML itself) asks again, and so a `ref:`
    # child trusted here is found by the run-time check in `core.use`, which
    # hashes the same way.
    from ..core.prompt_compose import document_content_digest
    from .library_sources import (
        LibraryRegistry,
        LibrarySourceError,
        confinement_root_for_document,
    )

    try:
        library_registry = LibraryRegistry.from_runtime(cfg.runtime)
    except LibrarySourceError:
        library_registry = LibraryRegistry.default()
    resolved_path = path.resolve()
    # Same cache-path confinement rule `cli.runtime_shim.run()`/`validate()`
    # apply (#396 second-review finding 6): `cof trust <cache path>` must
    # hash against the fetched tree's own root, not whatever project config
    # happens to sit above the cache directory on this machine, or it could
    # disagree with the run-time check over what the document's own content
    # even is.
    digest = document_content_digest(
        path,
        orch,
        confinement_root=confinement_root_for_document(
            path,
            resolved_path.parent,
            library_registry=library_registry,
            is_cache_path=library_registry.is_cache_path(path),
        ),
    )
    consented = consented_capabilities(digest, store_path=store)

    console.print(f"[bold]Orchestration:[/bold] {escape(str(path.resolve()))}")
    if not needed:
        console.print(
            "It uses none of the gated capabilities (shell, python_eval, "
            "fs-write, network) — nothing to consent to."
        )
        return

    currently = (
        f"consented ({', '.join(sorted(consented))})" if consented else "not consented"
    )
    console.print(f"[bold]Currently:[/bold] {currently}")
    console.print()
    console.print("[bold]It uses:[/bold] " + ", ".join(sorted(needed)))
    console.print()

    if not yes and not typer.confirm("Allow it?", default=False):
        console.print("Not consented; nothing changed.")
        raise typer.Exit(code=1)

    try:
        entry = record_consent(digest, needed, store_path=store)
    except OSError as exc:
        raise _fail(str(exc)) from exc
    console.print(
        f"[green]Consented:[/green] {escape(str(path.resolve()))} "
        f"(sha256 {entry.sha256[:12]}…) may use {', '.join(entry.capabilities)}. "
        "Applies until the file changes; an edited document needs `cof trust` again."
    )


def _print_settings(settings: dict[str, Any]) -> None:
    """What the file sets, one line per leaf, host-sensitive settings flagged."""
    pairs = flatten_settings(settings)
    if not pairs:
        console.print("It sets nothing.")
        return
    console.print("[bold]It sets:[/bold]")
    sensitive = 0
    for key, value in pairs:
        reason = host_sensitive_reason(key)
        line = Text(f"{key} = {json.dumps(value)}")
        if reason is None:
            console.print(Text("  ") + line, soft_wrap=True)
            continue
        sensitive += 1
        line.stylize("yellow")
        console.print(Text("! ", style="bold yellow") + line, soft_wrap=True)
        console.print(Text(f"    {reason}", style="yellow"), soft_wrap=True)
    if sensitive:
        console.print(
            f"[yellow]{sensitive} host-sensitive setting"
            f"{'' if sensitive == 1 else 's'} (marked !):[/yellow] they decide "
            "where prompts and credentials go, what runs on this machine, and "
            "where state is written."
        )


def register_trust(app: typer.Typer) -> None:
    @app.command(
        "trust",
        help=(
            "Trust a project config file, or consent to an orchestration "
            "document's capabilities (#275). A .yml/.yaml PATH is a "
            "document (shows the capabilities it needs and asks); anything "
            "else is a project config file (shows what it sets and asks)."
        ),
    )
    def trust_cmd(
        path: Path | None = typer.Argument(
            None,
            help="Config file or .yml/.yaml orchestration document to trust "
            "(default: circuitry.config.json or config.json in the current "
            "directory).",
            show_default=False,
        ),
        yes: bool = typer.Option(False, "--yes", "-y", help="Trust without asking."),
        list_: bool = typer.Option(
            False, "--list", help="List trusted files and whether each still matches."
        ),
        config: Path | None = typer.Option(
            None, "--config", "-c",
            help="Config to resolve library sources from, for a .yml/.yaml "
            "document's use: ref: children (or use CIRCUITRY_CONFIG).",
        ),
    ) -> None:
        store = trust_store_path()
        if list_:
            if path is not None:
                raise typer.BadParameter("--list takes no PATH.")
            try:
                _list_trusted(store)
            except TrustStoreError as exc:
                raise _fail(str(exc)) from exc
            return

        if path is not None and path.suffix.lower() in (".yml", ".yaml"):
            _trust_document(path, yes=yes, store=store, config=config)
            return

        try:
            target = _target_path(path)
            data = read_config_bytes(target)
            settings = parse_config_bytes(target, data)
            state = check_trust(target, data, store_path=store)
        except (ConfigError, TrustStoreError) as exc:
            raise _fail(str(exc)) from exc

        console.print(f"[bold]Project config:[/bold] {escape(str(target.resolve()))}")
        console.print(f"[bold]Currently:[/bold] {TRUST_STATE_LABELS[state]}")
        console.print()
        _print_settings(settings)
        console.print()

        if not yes and not typer.confirm("Trust this file?", default=False):
            console.print("Not trusted; nothing changed.")
            raise typer.Exit(code=1)

        try:
            entry = record_trust(target, data, store_path=store)
        except (TrustStoreError, OSError) as exc:
            raise _fail(str(exc)) from exc
        console.print(
            f"[green]Trusted:[/green] {escape(entry.path)} "
            f"(sha256 {entry.sha256[:12]}…). It applies until it changes; "
            "an edited file needs `cof trust` again."
        )

    @app.command("untrust", help="Stop applying a project config file you trusted.")
    def untrust_cmd(
        path: Path | None = typer.Argument(
            None,
            help="Config file to untrust (default: circuitry.config.json or "
            "config.json in the current directory).",
            show_default=False,
        ),
    ) -> None:
        try:
            target = _target_path(path)
            removed = remove_trust(target, store_path=trust_store_path())
        except (ConfigError, TrustStoreError, OSError) as exc:
            raise _fail(str(exc)) from exc
        shown = escape(str(target.resolve()))
        if removed:
            console.print(f"[green]No longer trusted:[/green] {shown}")
        else:
            console.print(f"{shown} was not trusted; nothing changed.")


def _list_trusted(store: Path) -> None:
    entries = read_trust_entries(store)
    if not entries:
        console.print(f"No trusted project configs ({escape(str(store))}).")
        return
    table = Table(title="Trusted project configs", show_header=True, header_style="bold cyan")
    table.add_column("File", overflow="fold")
    table.add_column("State")
    table.add_column("Trusted at")
    for entry in entries:
        try:
            matches = config_digest(Path(entry.path).read_bytes()) == entry.sha256
            state = "[green]matches[/green]" if matches else "[yellow]changed — skipped[/yellow]"
        except OSError:
            state = "[red]missing[/red]"
        table.add_row(Text(entry.path), state, entry.trusted_at or "—")
    console.print(table)
