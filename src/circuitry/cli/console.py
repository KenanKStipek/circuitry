from __future__ import annotations

from rich.console import Console
from rich.theme import Theme

THEME = Theme(
    {
        "info": "dim cyan",
        "ok": "green",
        "warn": "yellow",
        "err": "bold red",
        "path": "magenta",
        "title": "bold white",
    }
)

console = Console(theme=THEME)
