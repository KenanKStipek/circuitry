"""Wikipedia article fetch tool plugin via wikipedia-api.

Optional dep: ``wikipedia-api``. Install with
``pip install circuitry-cof[wikipedia]``.

Params:
  - ``title`` (required): article title.
  - ``language`` (optional, default ``"en"``): wiki language code.
  - ``mode`` (optional, default ``"summary"``):
    ``"summary"`` returns the short lead summary;
    ``"text"`` returns the full article plaintext;
    ``"sections"`` returns a tree of section titles + text.
  - ``user_agent`` (optional, default identifies circuitry): Wikipedia
    requires a UA per their API policy.
"""

from __future__ import annotations

import importlib.util
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


def _walk_sections(sections: Any) -> list[dict[str, Any]]:
    return [
        {
            "title": section.title,
            "text": section.text,
            "subsections": _walk_sections(section.sections),
        }
        for section in sections or []
    ]


@dataclass(frozen=True)
class WikipediaPlugin:
    name: str = "wikipedia"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        try:
            import wikipediaapi  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "wikipedia: wikipedia-api not installed. "
                "Install with: pip install wikipedia-api"
            ) from exc

        title = params.get("title")
        if not isinstance(title, str) or not title.strip():
            raise ValueError("wikipedia requires params['title'].")
        language = str(params.get("language") or "en")
        mode = str(params.get("mode") or "summary").lower()
        ua = str(params.get("user_agent") or "circuitry/0.1 (https://github.com/kenankstipek/circuitry)")

        wiki = wikipediaapi.Wikipedia(user_agent=ua, language=language)
        page = wiki.page(title.strip())
        if not page.exists():
            return ToolResult(
                value=None,
                raw={"title": title, "language": language, "exists": False},
                stdout=None, stderr=f"page not found: {title}", exit_code=1,
            )

        if mode == "summary":
            value: Any = page.summary
        elif mode == "text":
            value = page.text
        elif mode == "sections":
            value = _walk_sections(page.sections)
        else:
            raise ValueError(f"wikipedia: unknown mode {mode!r}")

        return ToolResult(
            value=value,
            raw={
                "title": title,
                "language": language,
                "mode": mode,
                "url": page.fullurl,
            },
            stdout=None, stderr=None, exit_code=0,
        )

    def check(self) -> CheckResult:
        if importlib.util.find_spec("wikipediaapi") is None:
            return CheckResult(
                ok=False,
                missing=["library:wikipedia-api"],
                message="pip install wikipedia-api",
            )
        return CheckResult(ok=True, missing=[])
