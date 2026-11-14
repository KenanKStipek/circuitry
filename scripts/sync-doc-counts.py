#!/usr/bin/env python3
"""Keep the tool-plugin, provider and electricity-tool counts/lists the docs
quote in sync with the registries they come from (issue #777).

    python scripts/sync-doc-counts.py            # rewrite every marked span
    python scripts/sync-doc-counts.py --check     # exit 1 if any span is stale

Each count or list lives between a pair of HTML comment markers,
``<!-- doc-counts:start NAME -->`` ... ``<!-- doc-counts:end NAME -->``, in a
doc file; this script only ever rewrites the text strictly between a marker
pair it recognizes, so nothing else in those files is touched. The values
themselves come from:

- ``tool-plugin-count`` — ``len(circuitry.plugins.factory.PLUGIN_REGISTRY)``.
- ``provider-count`` — ``len(circuitry.adapters.factory.ADAPTER_REGISTRY)``.
- ``electricity-tool-count``/``electricity-tool-list``/
  ``electricity-unsupported-count`` — the names inserted between the
  ``doc-counts:electricity-tools:start``/``:end`` comments in
  ``electricity/crates/electricity-tools/src/registry.rs``'s own
  ``builders()``, which is electricity's actual, hand-written tool-provider
  registry (not Circuitry's full one).

Standard library only, except for importing circuitry itself.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

from circuitry.adapters.factory import ADAPTER_REGISTRY  # noqa: E402
from circuitry.plugins.factory import PLUGIN_REGISTRY  # noqa: E402

ELECTRICITY_REGISTRY_RS = (
    REPO_ROOT / "electricity" / "crates" / "electricity-tools" / "src" / "registry.rs"
)

_RS_MARKER_START = "doc-counts:electricity-tools:start"
_RS_MARKER_END = "doc-counts:electricity-tools:end"


def _electricity_tool_names() -> list[str]:
    text = ELECTRICITY_REGISTRY_RS.read_text(encoding="utf-8")
    start = text.index(_RS_MARKER_START)
    end = text.index(_RS_MARKER_END, start)
    span = text[start:end]
    return sorted(re.findall(r'map\.insert\(\s*"([a-zA-Z0-9_]+)"', span))


def _format_list(names: list[str]) -> str:
    quoted = [f"`{name}`" for name in names]
    if len(quoted) <= 1:
        return "".join(quoted)
    return ", ".join(quoted[:-1]) + " and " + quoted[-1]


def _values() -> dict[str, str]:
    tool_plugin_count = len(PLUGIN_REGISTRY)
    provider_count = len(ADAPTER_REGISTRY)
    electricity_tools = _electricity_tool_names()
    return {
        "tool-plugin-count": str(tool_plugin_count),
        "provider-count": str(provider_count),
        "electricity-tool-count": str(len(electricity_tools)),
        "electricity-tool-list": _format_list(electricity_tools),
        "electricity-unsupported-count": str(tool_plugin_count - len(electricity_tools)),
    }


_MARKER_PATTERN = re.compile(
    r"<!-- doc-counts:start (?P<name>[a-z0-9-]+) -->.*?<!-- doc-counts:end (?P=name) -->",
    re.DOTALL,
)


def _rewrite(text: str, values: dict[str, str]) -> str:
    def replace(match: re.Match[str]) -> str:
        name = match.group("name")
        if name not in values:
            raise KeyError(f"unknown doc-counts marker {name!r}")
        return f"<!-- doc-counts:start {name} -->{values[name]}<!-- doc-counts:end {name} -->"

    return _MARKER_PATTERN.sub(replace, text)


DOC_FILES = [
    REPO_ROOT / "README.md",
    REPO_ROOT / "electricity" / "README.md",
]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit 1 if any marked span would change, without writing it",
    )
    args = parser.parse_args(argv)

    values = _values()
    stale = []
    for doc in DOC_FILES:
        current = doc.read_text(encoding="utf-8")
        updated = _rewrite(current, values)
        if updated != current:
            if args.check:
                stale.append(doc)
            else:
                doc.write_text(updated, encoding="utf-8")

    if args.check and stale:
        for doc in stale:
            try:
                label = doc.relative_to(REPO_ROOT)
            except ValueError:
                label = doc
            print(
                f"{label} is out of sync with the registries — "
                "run scripts/sync-doc-counts.py",
                file=sys.stderr,
            )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
