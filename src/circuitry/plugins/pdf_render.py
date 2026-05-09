"""PDF render tool plugin.

Renders HTML to PDF via either ``weasyprint`` or ``wkhtmltopdf``,
whichever is on PATH (preference: weasyprint). Pure-Python pdf engines
(Reportlab etc) aren't used here — those would be separate plugins
focused on programmatic PDF construction; this one targets HTML→PDF.

Params:
  - ``input``: HTML payload (string) when ``from_path`` is False,
    else a path to an HTML file.
  - ``output``: required path to write the PDF to.
  - ``from_path`` (bool, default False).
  - ``base_url`` (optional): used by weasyprint for relative-asset
    resolution; ignored by wkhtmltopdf.
"""

from __future__ import annotations

import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from ._subprocess import resolve_binary
from .base import ToolResult


_CANDIDATES = ("weasyprint", "wkhtmltopdf")


@dataclass(frozen=True)
class PdfRenderPlugin:
    name: str = "pdf_render"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        output = params.get("output")
        if not isinstance(output, str) or not output:
            raise ValueError("pdf_render: params['output'] required.")
        binary = resolve_binary(_CANDIDATES)
        if binary is None:
            raise RuntimeError(
                f"pdf_render: none of {list(_CANDIDATES)} on PATH."
            )

        from_path = bool(params.get("from_path"))
        src_payload = params.get("input")
        if not isinstance(src_payload, str):
            raise ValueError("pdf_render: params['input'] must be a string.")

        out_path = Path(output).expanduser()
        out_path.parent.mkdir(parents=True, exist_ok=True)

        # Build invocation per-binary. Both accept (input, output) as
        # positional args; weasyprint additionally supports --base-url.
        is_weasy = Path(binary).name == "weasyprint" or binary.endswith("/weasyprint")

        with tempfile.TemporaryDirectory() as tmp_dir:
            input_arg = src_payload
            if not from_path:
                input_arg = str(Path(tmp_dir) / "input.html")
                Path(input_arg).write_text(src_payload, encoding="utf-8")

            cmd = [binary, str(input_arg), str(out_path)]
            if is_weasy and isinstance(params.get("base_url"), str):
                cmd.insert(1, "--base-url")
                cmd.insert(2, str(params["base_url"]))

            proc = subprocess.run(
                cmd, capture_output=True, text=True,
                timeout=int(timeout_seconds), check=False,
            )

        if proc.returncode != 0:
            raise RuntimeError(
                f"pdf_render: {Path(binary).name} failed (exit {proc.returncode}): "
                f"{(proc.stderr or proc.stdout).strip()}"
            )

        return ToolResult(
            value=str(out_path),
            raw={"binary": binary, "args": cmd[1:]},
            stdout=proc.stdout,
            stderr=proc.stderr,
            exit_code=proc.returncode,
        )

    def check(self) -> CheckResult:
        if resolve_binary(_CANDIDATES):
            return CheckResult(ok=True, missing=[])
        return CheckResult(
            ok=False,
            missing=["binary:weasyprint"],
            message="install weasyprint (preferred) or wkhtmltopdf.",
        )
