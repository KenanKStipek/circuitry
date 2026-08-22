"""XML tool plugin via lxml.

Optional dep: ``lxml``. Install with ``pip install circuitry-cof[xml]``.

Params:
  - ``mode``: ``"parse" | "xpath" | "to_string"``.
  - ``input``: XML string. When ``from_path`` is True, treated as a file
    path.
  - ``from_path`` (bool, default False).
  - ``xpath`` (xpath mode, str): XPath expression.
  - ``namespaces`` (xpath mode, optional dict): prefix → URI bindings.
  - ``pretty`` (to_string mode, bool, default False).

Returns:
  - parse → tag/attrib/text dict tree.
  - xpath → list of matched nodes (each as tag/attrib/text dict, or
    string for attribute / text() results).
  - to_string → serialised XML as a string.
"""

from __future__ import annotations

import importlib.util
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


def _node_to_dict(node: Any) -> dict[str, Any]:
    return {
        "tag": str(node.tag),
        "attrib": dict(node.attrib.items()),
        "text": node.text or "",
        "children": [_node_to_dict(c) for c in list(node)],
    }


@dataclass(frozen=True)
class XmlPlugin:
    name: str = "xml"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        try:
            from lxml import etree  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "xml: lxml not installed. Install with: pip install lxml"
            ) from exc

        mode = str(params.get("mode") or "parse").lower()
        from_path = bool(params.get("from_path"))
        text = params.get("input")
        if not isinstance(text, str):
            raise ValueError("xml requires params['input'] as a string.")
        if from_path:
            xml_bytes = Path(text).expanduser().read_bytes()
        else:
            xml_bytes = text.encode("utf-8")

        try:
            tree = etree.fromstring(xml_bytes)
        except etree.XMLSyntaxError as exc:
            raise ValueError(f"xml: parse failed: {exc}") from exc

        if mode == "parse":
            value: Any = _node_to_dict(tree)
        elif mode == "xpath":
            xpath_expr = params.get("xpath")
            if not isinstance(xpath_expr, str) or not xpath_expr:
                raise ValueError("xml: xpath mode requires params['xpath'].")
            namespaces = params.get("namespaces") or {}
            try:
                results = tree.xpath(xpath_expr, namespaces=namespaces)
            except etree.XPathEvalError as exc:
                raise ValueError(f"xml: invalid xpath: {exc}") from exc
            value = []
            for r in results:
                if hasattr(r, "tag"):
                    value.append(_node_to_dict(r))
                else:
                    value.append(str(r))
        elif mode == "to_string":
            pretty = bool(params.get("pretty"))
            value = etree.tostring(tree, pretty_print=pretty).decode("utf-8")
        else:
            raise ValueError(f"xml: unknown mode {mode!r}")

        return ToolResult(
            value=value,
            raw={"mode": mode},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        if importlib.util.find_spec("lxml") is None:
            return CheckResult(
                ok=False,
                missing=["library:lxml"],
                message="pip install lxml",
            )
        return CheckResult(ok=True, missing=[])
