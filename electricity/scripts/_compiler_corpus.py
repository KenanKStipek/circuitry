"""Shared helper for electricity-compiler's golden corpora (issue #408's
Test strategy section).

Not itself a `generate_*.py` script (the generated-files workflow's glob
skips a leading underscore), but imported by every one that is:
`generate_compiler_smoke_corpus.py` (lane A, exercising this helper end
to end) and lane B/C/D's own `generate_compiler_{load,compile,compose}_
corpus.py`.

Each case is a dict:

    {
        "name": str,
        "files": {relpath: text | {"symlink": target} | {"bytes_hex": hex}},
        "entry": relpath,          # which file is the orchestration itself
        "options": {"skip_preflight": bool, "trust_document": bool},  # optional
        "error_modes": {                                              # optional
            "validate_errors": ["exact" | "location", ...],
            "run_error": "exact" | "location" | None,
        },
    }

`run_case` materializes *case*'s files into a fresh temporary directory
(that directory is also the working directory and `HOME` for the
duration of the run -- the repository's rule for any subprocess or
in-process run that could otherwise touch a real `HOME`/`trusted.json`),
then runs, against `case["entry"]`:

- `runtime_shim.validate(...)` -> `{ok, errors, warnings}`;
- `runtime_shim.run(RunRequest(..., validate_only=True,
  skip_preflight=True, trust_document=True))` -> `RunResult.error`;
- when the entry document loads as a mapping, `compile_orchestration`
  dumped as JSON (dataclasses -> dicts, tuples -> lists, frozensets ->
  sorted lists, the reflector prime -> `"<REFLECTOR_PRIME>"`), else
  `None`.

Every recorded string has the temporary root replaced with the literal
`<root>` (so two runs on different machines/paths produce the same
file), and `render_corpus` refuses to write output that still contains
a real home, temp, `/private/`, or Windows drive-letter path -- the
repository's rule that a committed generated file must carry no local
path (`LANE-CONTRACT.md`).
"""

from __future__ import annotations

import dataclasses
import json
import os
import re
import sys
import tempfile
import uuid
from pathlib import Path
from typing import Any

from circuitry.cli.orchestration_loader import load_orchestration_file
from circuitry.cli.runtime_shim import RunRequest, run, validate
from circuitry.core.compiler import compile_orchestration
from circuitry.core.primes import REFLECTOR_PRIME_V1

_LEAKED_PATH_PATTERNS = [
    re.compile(re.escape(str(Path.home()))),
    re.compile(re.escape(tempfile.gettempdir())),
    re.compile(r"/private/"),
    re.compile(r"\b[A-Za-z]:[\\/]"),  # a Windows drive letter
]


def _materialize(files: dict[str, Any], root: Path) -> None:
    # Symlinks first-pass-deferred: a symlink's target may itself be a
    # file this same case writes, so plain files land before any link
    # that might point at them.
    symlinks: list[tuple[Path, str]] = []
    for relpath, content in files.items():
        dest = root / relpath
        dest.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(content, dict) and "symlink" in content:
            symlinks.append((dest, content["symlink"]))
        elif isinstance(content, dict) and "bytes_hex" in content:
            dest.write_bytes(bytes.fromhex(content["bytes_hex"]))
        else:
            dest.write_text(content, encoding="utf-8")
    for dest, target in symlinks:
        os.symlink(target, dest)


def _dump_definition(obj: Any) -> Any:
    if dataclasses.is_dataclass(obj) and not isinstance(obj, type):
        result: dict[str, Any] = {}
        for field in dataclasses.fields(obj):
            value = getattr(obj, field.name)
            if field.name == "prime_template" and value == REFLECTOR_PRIME_V1:
                result[field.name] = "<REFLECTOR_PRIME>"
            else:
                result[field.name] = _dump_definition(value)
        return result
    if isinstance(obj, frozenset):
        return sorted(_dump_definition(v) for v in obj)
    if isinstance(obj, (list, tuple)):
        return [_dump_definition(v) for v in obj]
    if isinstance(obj, dict):
        return {k: _dump_definition(v) for k, v in obj.items()}
    return obj


class _DirectoryAndHome:
    """Runs its body with the current working directory and `HOME` both
    set to *root*, restoring both afterward -- even on an exception.
    """

    def __init__(self, root: Path) -> None:
        self._root = root
        self._old_cwd: str | None = None
        self._old_home: str | None = None

    def __enter__(self) -> None:
        self._old_cwd = os.getcwd()
        self._old_home = os.environ.get("HOME")
        os.chdir(self._root)
        os.environ["HOME"] = str(self._root)

    def __exit__(self, *exc_info: object) -> None:
        if self._old_cwd is not None:
            os.chdir(self._old_cwd)
        if self._old_home is None:
            os.environ.pop("HOME", None)
        else:
            os.environ["HOME"] = self._old_home


def run_case(case: dict[str, Any]) -> dict[str, Any]:
    """Materializes and runs *case*, returning its recorded result (not
    yet path-normalized -- see `render_corpus`).
    """
    options = case.get("options") or {}
    skip_preflight = options.get("skip_preflight", True)
    trust_document = options.get("trust_document", True)

    with tempfile.TemporaryDirectory(prefix="electricity-compiler-corpus-") as tmp:
        root = Path(tmp).resolve()
        entry_home = root / f"home-{uuid.uuid4().hex}"
        entry_home.mkdir()
        _materialize(case["files"], root)
        entry_path = root / case["entry"]

        with _DirectoryAndHome(entry_home):
            validate_result = validate(
                entry_path,
                config=None,
                skip_preflight=skip_preflight,
                trust_document=trust_document,
            )
            run_result = run(
                RunRequest(
                    orchestration_path=entry_path,
                    state_path=None,
                    out_path=None,
                    dry_run=False,
                    validate_only=True,
                    skip_preflight=skip_preflight,
                    trust_document=trust_document,
                )
            )

            definition: Any = None
            try:
                orch = load_orchestration_file(entry_path)
                compiled = compile_orchestration(
                    orch=orch,
                    document_dir=entry_path.resolve().parent,
                    confinement_root=entry_path.resolve().parent,
                )
                definition = _dump_definition(compiled)
            except Exception:
                definition = None

        error_modes = case.get("error_modes") or {}
        validate_errors = validate_result.get("errors", [])
        comparison = {
            "validate_errors": error_modes.get(
                "validate_errors", ["exact"] * len(validate_errors)
            ),
            "run_error": error_modes.get(
                "run_error", "exact" if run_result.error else None
            ),
        }

        return {
            "name": case["name"],
            "files": case["files"],
            "entry": case["entry"],
            "validate": {
                "ok": validate_result.get("ok", False),
                "errors": validate_errors,
                "warnings": validate_result.get("warnings", []),
            },
            "run_error": run_result.error,
            "definition": definition,
            "comparison": comparison,
            "_root": str(root),
            "_entry_home": str(entry_home),
        }


def _normalize_paths(obj: Any, roots: list[str]) -> Any:
    if isinstance(obj, str):
        text = obj
        for root in roots:
            text = text.replace(root, "<root>")
        return text
    if isinstance(obj, list):
        return [_normalize_paths(v, roots) for v in obj]
    if isinstance(obj, dict):
        return {
            k: _normalize_paths(v, roots)
            for k, v in obj.items()
            if not k.startswith("_")
        }
    return obj


def render_corpus(results: list[dict[str, Any]]) -> str:
    """JSON-renders *results* (each from `run_case`), with every
    temporary root replaced by `<root>`, refusing to produce output that
    still leaks a real local path.
    """
    normalized = []
    for result in results:
        roots = [result["_root"], result["_entry_home"]]
        normalized.append(_normalize_paths(result, roots))

    text = json.dumps(normalized, indent=2, ensure_ascii=False) + "\n"
    for pattern in _LEAKED_PATH_PATTERNS:
        match = pattern.search(text)
        if match:
            raise ValueError(
                f"generated corpus still contains a local path ({pattern.pattern!r} "
                f"matched {match.group(0)!r}) -- fix _compiler_corpus.py's "
                "normalization before writing this file"
            )
    return text


def write_or_check(output: Path, text: str, *, check: bool) -> int:
    output.parent.mkdir(parents=True, exist_ok=True)
    if check:
        current = output.read_text() if output.exists() else ""
        if current != text:
            print(
                f"{output} is stale; run without --check to regenerate", file=sys.stderr
            )
            return 1
        return 0
    output.write_text(text)
    print(f"wrote {output}")
    return 0
