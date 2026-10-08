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
        "files": {
            relpath: text
            | {"symlink": target}
            | {"bytes_hex": hex, "mode": octal_str}
            | {"repeat": text, "count": int}
        },
        "entry": relpath,          # which file is the orchestration itself
        "options": {"skip_preflight": bool, "trust_document": bool},  # optional
        "error_modes": {                                              # optional
            "validate_errors": ["exact" | "location", ...],
            "run_error": "exact" | "location" | None,
        },
    }

A `{"bytes_hex": ...}` entry may also carry an optional `"mode"` (an
octal permission string, e.g. `"000"`) -- applied with `os.chmod` after
the file is written, for a lane D case exercising an unreadable prompt
file (`core/prompt_files.py`'s "could not be read" branch). Not
meaningful on Windows; a case that needs it is Unix-only by nature
(permission bits), same as a symlink-escape case already is.

A `{"repeat": text, "count": int}` entry writes *text* repeated *count*
times (`text * count`) -- a generated-content spec for a case whose
file content is large and uniform (a lane D case one byte over the
prompt-file size limit, say), so the golden JSON this helper writes
records the short spec rather than the file's own megabyte of bytes.

`run_case` materializes *case*'s files into a fresh temporary directory
(that directory -- its resolved, symlink-free path, matching the Rust
harness's own canonicalization -- is also the working directory and
`HOME` for the duration of the run, and the global-config path
`circuitry.cli.config` binds at import time is patched underneath it too
-- the repository's rule for any subprocess or in-process run that
could otherwise touch a real `HOME`/`trusted.json`), then runs, against
`case["entry"]`:

- `runtime_shim.validate(...)` -> `{ok, errors, warnings}`;
- `runtime_shim.run(RunRequest(..., validate_only=True,
  skip_preflight=True, trust_document=True))` -> `RunResult.error`;
- `compile_orchestration`, confined to the document's own project (the
  nearest `config.json`, like Circuitry's own loader), dumped as JSON on
  success (dataclasses -> dicts, tuples -> lists, frozensets -> sorted
  lists, the reflector prime -> `"<REFLECTOR_PRIME>"`) into `definition`,
  or `None` with the exception's text in `definition_error` on failure;
- `document_content_digest`, the same digest `use` and capability
  consent hash a document's bytes with;
- `core.lint.lint_orchestration(orch)` alone (not the rest of
  `validate()`'s own combined `warnings` list), recorded separately as
  `lint_warnings` -- electricity-compiler's `check_report` does not
  reproduce these advisories at all (issue #428), so a case whose
  document trips one carries it here rather than in `validate`'s own
  `warnings`, which the Rust harness compares after subtracting this
  list.

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
from pathlib import Path
from typing import Any

from circuitry.cli import config as _config_module
from circuitry.cli.orchestration_loader import load_orchestration_file
from circuitry.cli.runtime_shim import RunRequest, run, validate
from circuitry.core.compiler import compile_orchestration
from circuitry.core.lint import lint_orchestration
from circuitry.core.primes import REFLECTOR_PRIME_V1
from circuitry.core.prompt_compose import document_content_digest
from circuitry.core.prompt_files import default_project_root

_LEAKED_PATH_PATTERNS = [
    re.compile(re.escape(str(Path.home()))),
    re.compile(re.escape(tempfile.gettempdir())),
    re.compile(r"/private/"),
    # A Windows drive letter: a bare letter (not part of a longer word)
    # followed by ':' and either two literal backslashes (how a real
    # path's single `\` round-trips through JSON escaping) or a single
    # `/` not itself followed by another `/` -- the latter exclusion is
    # what keeps an ordinary `https://` URL or a one-letter YAML key
    # (`x:` at end of line) from matching.
    re.compile(r"(?<![A-Za-z0-9_])[A-Za-z]:(?:\\\\|/(?!/))"),
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
            if "mode" in content:
                dest.chmod(int(content["mode"], 8))
        elif isinstance(content, dict) and "repeat" in content:
            dest.write_text(content["repeat"] * content["count"], encoding="utf-8")
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


class _IsolatedRun:
    """Runs its body with the current working directory and `HOME` both
    set to *root* (issue #408's Test strategy: "that directory as the
    working directory and a temporary HOME"), restoring both afterward --
    even on an exception.

    Also isolates the global-config path the way `tests/conftest.py`'s
    `_hermetic_global_config` fixture does for pytest: `circuitry.cli.
    config.GLOBAL_CONFIG_PATH`/`GLOBAL_CONFIG_DIR` are bound once, from
    `Path.home()`, at import time -- long before this context manager's
    `HOME` override takes effect -- so `trust_store_path()` (read by
    `runtime_shim.run`'s capability-consent gate on every case) would
    otherwise still resolve to the real `~/.config/circuitry/trusted.
    json` and read or write it.
    """

    def __init__(self, root: Path) -> None:
        self._root = root
        self._old_cwd: str | None = None
        self._old_home: str | None = None
        self._old_global_config_dir: Path | None = None
        self._old_global_config_path: Path | None = None

    def __enter__(self) -> None:
        self._old_cwd = os.getcwd()
        self._old_home = os.environ.get("HOME")
        self._old_global_config_dir = _config_module.GLOBAL_CONFIG_DIR
        self._old_global_config_path = _config_module.GLOBAL_CONFIG_PATH
        os.chdir(self._root)
        os.environ["HOME"] = str(self._root)
        fake_global_config_dir = self._root / ".config-circuitry-global"
        _config_module.GLOBAL_CONFIG_DIR = fake_global_config_dir
        _config_module.GLOBAL_CONFIG_PATH = fake_global_config_dir / "config.json"

    def __exit__(self, *exc_info: object) -> None:
        if self._old_cwd is not None:
            os.chdir(self._old_cwd)
        if self._old_home is None:
            os.environ.pop("HOME", None)
        else:
            os.environ["HOME"] = self._old_home
        assert self._old_global_config_dir is not None
        assert self._old_global_config_path is not None
        _config_module.GLOBAL_CONFIG_DIR = self._old_global_config_dir
        _config_module.GLOBAL_CONFIG_PATH = self._old_global_config_path


def run_case(case: dict[str, Any]) -> dict[str, Any]:
    """Materializes and runs *case*, returning its recorded result (not
    yet path-normalized -- see `render_corpus`).
    """
    options = case.get("options") or {}
    skip_preflight = options.get("skip_preflight", True)
    trust_document = options.get("trust_document", True)

    with tempfile.TemporaryDirectory(prefix="electricity-compiler-corpus-") as tmp:
        root = Path(tmp).resolve()
        _materialize(case["files"], root)
        entry_path = root / case["entry"]

        with _IsolatedRun(root):
            try:
                validate_result = validate(
                    entry_path,
                    config=None,
                    skip_preflight=skip_preflight,
                    trust_document=trust_document,
                )
            except Exception as exc:
                # `validate()` reads the orchestration file's raw text
                # itself, outside the broad `except Exception` the rest
                # of the function is wrapped in (`cli/runtime_shim.py`'s
                # own `text = orchestration_path.read_text(...)`), so a
                # missing file or undecodable bytes raise straight out
                # of this call instead of returning the usual `{ok,
                # errors, warnings}` shape -- recorded the same way a
                # `definition_error` below is, not swallowed.
                validate_result = {
                    "ok": False,
                    "errors": [f"{type(exc).__name__}: {exc}"],
                    "warnings": [],
                }
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

            document_dir = entry_path.resolve().parent
            confinement_root = default_project_root(document_dir)

            definition: Any = None
            definition_error: str | None = None
            # `validate()`'s own `lint_orchestration(orch)` call -- the
            # advisory warnings electricity-compiler's `check_report`
            # does not reproduce at all (issue #428: lint parity is a
            # follow-up, not assigned to any #408 lane). Recorded
            # separately from `warnings` below so the Rust harness can
            # compare everything else exactly while subtracting these.
            # Only computed once the document actually loads -- same as
            # `validate()`, whose own `lint_warnings` variable never
            # gains `lint_orchestration`'s contribution on a load error.
            lint_warnings: list[str] = []
            try:
                orch = load_orchestration_file(entry_path)
            except Exception as exc:  # recorded, not swallowed
                definition_error = f"{type(exc).__name__}: {exc}"
                # Best-effort only for the digest below, which (like
                # Circuitry's own `document_content_digest`) tolerates a
                # malformed/absent `orch` -- never for `compile_orchestration`,
                # which must not report a fabricated empty definition for a
                # document that never actually loaded.
                orch = {}
            else:
                lint_warnings = lint_orchestration(orch)
                try:
                    compiled = compile_orchestration(
                        orch=orch,
                        document_dir=document_dir,
                        confinement_root=confinement_root,
                    )
                    definition = _dump_definition(compiled)
                except Exception as exc:  # recorded, not swallowed
                    definition_error = f"{type(exc).__name__}: {exc}"

            try:
                digest = document_content_digest(
                    entry_path, orch, confinement_root=confinement_root
                )
            except OSError:
                # `document_content_digest` reads *entry_path*'s own raw
                # bytes itself, unconditionally and outside any of its
                # own `try`/`except` -- a case whose entry is missing
                # (or otherwise unreadable) never reaches it in
                # ordinary use (`validate`/`run` would have failed on
                # the same read already), but this helper calls it
                # regardless of whether `orch` loaded, so it must
                # tolerate the same failure here.
                digest = None

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
            "options": {
                "skip_preflight": skip_preflight,
                "trust_document": trust_document,
            },
            "validate": {
                "ok": validate_result.get("ok", False),
                "errors": validate_errors,
                "warnings": validate_result.get("warnings", []),
            },
            "lint_warnings": lint_warnings,
            "run_error": run_result.error,
            "definition": definition,
            "definition_error": definition_error,
            "digest": digest,
            "comparison": comparison,
            "_root": str(root),
        }


def _normalize_paths(obj: Any, root: str) -> Any:
    if isinstance(obj, str):
        return obj.replace(root, "<root>")
    if isinstance(obj, list):
        return [_normalize_paths(v, root) for v in obj]
    if isinstance(obj, dict):
        return {
            k: _normalize_paths(v, root)
            for k, v in obj.items()
            if not k.startswith("_")
        }
    return obj


def render_corpus(results: list[dict[str, Any]]) -> str:
    """JSON-renders *results* (each from `run_case`), with every
    temporary root replaced by `<root>`, refusing to produce output that
    still leaks a real local path.
    """
    normalized = [_normalize_paths(result, result["_root"]) for result in results]

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
