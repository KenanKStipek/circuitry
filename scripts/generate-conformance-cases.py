#!/usr/bin/env python3
"""Generate `tests/conformance/cases/*/expected.json` by running each case's
document through `cof run` (electricity/DESIGN.md §12): the reference
implementation is the ground truth, so a case's expected state is never
hand-written, only captured.

    python scripts/generate-conformance-cases.py            # regenerate every case
    python scripts/generate-conformance-cases.py --check     # exit 1 if any case's
                                                               committed expected
                                                               state is stale
    python scripts/generate-conformance-cases.py c2-yaml-1-1-value-types
                                                               # regenerate one case

A "success" case stores the plain `--out` bytes exactly (insertion order,
no indent — key order is part of parity, see §3.4); generation never uses
`--pretty` for that file (`c23-out-plain-vs-pretty` is the one case that
also needs the `--pretty` serialization, via `also_pretty` in `case.json`,
stored separately as `expected.pretty.json`). A "failure" case — a
load/check failure the document is expected to hit before any effect
dispatches — records the failure's message, not a state: generation never
writes `expected.json` from `--out` for one of these, only from the CLI's
own `{"ok": false, "error": ...}` stdout.

`--out`'s own run id/timestamps/durations are freshly generated on every
invocation (`cof run` doesn't replay history), so regenerating a case that
hasn't semantically changed still rewrites different *bytes* every time.
`--check` therefore compares under the suite's own normalizer
(`tests/conformance/normalize.py`) rather than byte-for-byte against the
committed file — "stale" means the normalized value changed, not that a
fresh run id did.
"""

from __future__ import annotations

import argparse
import json
import sys
import tempfile
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT))

from tests.conformance import harness  # noqa: E402
from tests.conformance.normalize import (  # noqa: E402
    assert_states_equal,
    normalize,
    redact_leaked_paths,
)


def _redact_and_reserialize(raw_bytes: bytes, *, pretty: bool) -> bytes:
    """Replace the two absolute-path fields `effective_settings` echoes back
    (the generating machine's own checkout and temp-file locations) with
    their stable placeholder (`<out>`, `<case>`) before anything is written
    to disk, so a contributor's local paths never reach a committed file or
    a public PR. Re-serializes with
    the same rules `--out`/`--out --pretty` themselves use
    (`normalize.assert_out_serialization`), so the committed file still
    round-trips through that check."""
    data = json.loads(raw_bytes.decode("utf-8"))
    redacted = redact_leaked_paths(data)
    if pretty:
        text = json.dumps(redacted, indent=2, sort_keys=True, ensure_ascii=True) + "\n"
    else:
        text = json.dumps(redacted, ensure_ascii=True) + "\n"
    return text.encode("utf-8")


def _generate_success(case_dir: Path, metadata: dict[str, Any], tmp_root: Path) -> dict[str, bytes]:
    home_dir = tmp_root / "home"
    home_dir.mkdir(exist_ok=True)
    out_path = tmp_root / "out.json"
    result = harness.run_case(case_dir, metadata, out_path=out_path, home_dir=home_dir)
    if result.returncode != 0:
        raise RuntimeError(
            f"{case_dir.name}: expected success, `cof run` exited {result.returncode}\n"
            f"stdout: {result.stdout}\nstderr: {result.stderr}"
        )
    files = {
        "expected.json": _redact_and_reserialize(out_path.read_bytes(), pretty=False)
    }
    if metadata.get("also_pretty"):
        pretty_home = tmp_root / "home-pretty"
        pretty_home.mkdir(exist_ok=True)
        pretty_out = tmp_root / "out.pretty.json"
        pretty_result = harness.run_case(
            case_dir, metadata, out_path=pretty_out, home_dir=pretty_home, pretty=True
        )
        if pretty_result.returncode != 0:
            raise RuntimeError(
                f"{case_dir.name}: expected success (--pretty), `cof run` exited "
                f"{pretty_result.returncode}\nstdout: {pretty_result.stdout}\n"
                f"stderr: {pretty_result.stderr}"
            )
        files["expected.pretty.json"] = _redact_and_reserialize(
            pretty_out.read_bytes(), pretty=True
        )
    return files


def _generate_failure(case_dir: Path, metadata: dict[str, Any], tmp_root: Path) -> dict[str, bytes]:
    home_dir = tmp_root / "home"
    home_dir.mkdir(exist_ok=True)
    out_path = tmp_root / "out.json"
    result = harness.run_case(case_dir, metadata, out_path=out_path, home_dir=home_dir)
    if result.returncode == 0:
        raise RuntimeError(
            f"{case_dir.name}: expected a load/check failure, but `cof run` exited 0"
        )
    error = harness.parse_cli_error(result.stdout)
    payload = {"error": error}
    text = json.dumps(payload, indent=2, ensure_ascii=True, sort_keys=True) + "\n"
    return {"expected.json": text.encode("utf-8")}


def generate_case(case_dir: Path) -> dict[str, bytes]:
    metadata = harness.load_case(case_dir)
    with tempfile.TemporaryDirectory(prefix="circuitry-conformance-gen-") as tmp:
        tmp_root = Path(tmp)
        if metadata["expect"] == "success":
            return _generate_success(case_dir, metadata, tmp_root)
        if metadata["expect"] == "failure":
            return _generate_failure(case_dir, metadata, tmp_root)
        raise ValueError(f"{case_dir.name}: unknown case.json 'expect': {metadata['expect']!r}")


def _content_is_current(current: bytes | None, generated: bytes) -> bool:
    """Compare a committed expected file to a freshly generated one.
    JSON content is compared under the suite's normalizer (run id,
    timestamps, durations, and friends always differ byte-for-byte between
    two runs); anything else (there is nothing else today, but `--check`
    should degrade safely rather than silently pass) falls back to exact
    bytes."""
    if current is None:
        return False
    if current == generated:
        return True
    try:
        current_value = json.loads(current)
        generated_value = json.loads(generated)
    except json.JSONDecodeError:
        return False
    # A committed file still holding a raw absolute path (pre-redaction, or
    # hand-edited back in) is always stale, even though `normalize()` would
    # call it equivalent to a freshly redacted run — that's exactly the
    # placeholder-vs-real-value difference `normalize()` is built to ignore
    # at *comparison* time, which must not let a leak go unwritten here.
    if redact_leaked_paths(current_value) != current_value:
        return False
    try:
        assert_states_equal(normalize(current_value), normalize(generated_value))
    except AssertionError:
        return False
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit 1 if any case's committed expected state would change, without writing",
    )
    parser.add_argument(
        "case",
        nargs="*",
        help="case directory name(s) to regenerate (default: all cases)",
    )
    args = parser.parse_args()

    all_case_dirs = harness.list_case_dirs()
    if not all_case_dirs:
        print("no cases found under tests/conformance/cases/", file=sys.stderr)
        return 1

    if args.case:
        wanted = set(args.case)
        case_dirs = [d for d in all_case_dirs if d.name in wanted]
        missing = wanted - {d.name for d in case_dirs}
        if missing:
            print(f"unknown case(s): {', '.join(sorted(missing))}", file=sys.stderr)
            return 1
    else:
        case_dirs = all_case_dirs

    stale: list[str] = []
    for case_dir in case_dirs:
        try:
            files = generate_case(case_dir)
        except Exception as exc:  # stop at the first case that fails to run
            print(f"FAIL {case_dir.name}: {exc}", file=sys.stderr)
            return 1

        for filename, content in files.items():
            target = case_dir / filename
            current = target.read_bytes() if target.exists() else None
            if args.check:
                if not _content_is_current(current, content):
                    stale.append(str(target.relative_to(REPO_ROOT)))
            elif not _content_is_current(current, content):
                # Only touch files that actually changed: a fresh run id,
                # timestamp and duration differ on *every* invocation, so
                # writing unconditionally would make every regeneration (even
                # of an untouched case) a noisy diff.
                target.write_bytes(content)

    if args.check and stale:
        print(
            "stale conformance expected state (run "
            "`python scripts/generate-conformance-cases.py`):",
            file=sys.stderr,
        )
        for path in stale:
            print(f"  {path}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
