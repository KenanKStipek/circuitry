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
stored separately as `expected.pretty.json`). A "failure" case — one the
document is expected to fail, whether at load/check time before any effect
ever dispatches, or during the run itself (a tool exhausting its retries,
a `stop_on_error` cancellation, a failed `finally:`) — records the
failure's message, not a state: generation never writes `expected.json`
from `--out` for one of these, only from the CLI's own
`{"ok": false, "error": ...}` stdout. It also records `--out` itself
(`expected.out.json`), since `cof run` writes one on any failure, not
only on success.

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
    assert_events_equal,
    assert_states_equal,
    normalize,
    redact_leaked_paths,
    redact_runtime_strings,
)


def _redact_and_reserialize(
    raw_bytes: bytes, *, pretty: bool, replacements: list[tuple[str, str]]
) -> bytes:
    """Replace the two absolute-path fields `effective_settings` echoes back
    (the generating machine's own checkout and temp-file locations) with
    their stable placeholder (`<out>`, `<case>`), then every case-directory/
    fakes-directory/mock-port substring *anywhere* a string leaf carries one
    (`redact_runtime_strings`), before anything is written to disk, so a
    contributor's local paths never reach a committed file or a public PR.
    Re-serializes with the same rules `--out`/`--out --pretty` themselves use
    (`normalize.assert_out_serialization`), so the committed file still
    round-trips through that check."""
    data = json.loads(raw_bytes.decode("utf-8"))
    redacted = redact_runtime_strings(redact_leaked_paths(data), replacements)
    if pretty:
        text = json.dumps(redacted, indent=2, sort_keys=True, ensure_ascii=True) + "\n"
    else:
        text = json.dumps(redacted, ensure_ascii=True) + "\n"
    return text.encode("utf-8")


def _assert_no_leftover_replies(leftover_path: Path, *, case_name: str) -> None:
    leftover = harness.read_leftover_replies(leftover_path)
    if leftover:
        raise RuntimeError(
            f"{case_name}: scripted replies left over after the run: {leftover} "
            "-- remove them from the replies file or make the document consume them"
        )


def _generate_success(
    case_dir: Path,
    metadata: dict[str, Any],
    tmp_root: Path,
    *,
    server: Any,
    replacements: list[tuple[str, str]],
) -> dict[str, bytes]:
    home_dir = tmp_root / "home"
    home_dir.mkdir(exist_ok=True)
    out_path = tmp_root / "out.json"
    events_path = tmp_root / "events.jsonl"
    leftover_replies_path = tmp_root / "leftover-replies.json"
    result = harness.run_case(
        case_dir,
        metadata,
        out_path=out_path,
        home_dir=home_dir,
        events_path=events_path,
        leftover_replies_path=leftover_replies_path,
        mock_http_port=server.port if server else None,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"{case_dir.name}: expected success, `cof run` exited {result.returncode}\n"
            f"stdout: {result.stdout}\nstderr: {result.stderr}"
        )
    _assert_no_leftover_replies(leftover_replies_path, case_name=case_dir.name)
    files = {
        "expected.json": _redact_and_reserialize(
            out_path.read_bytes(), pretty=False, replacements=replacements
        ),
        # No path redaction needed: `--events` carries `path`/`orchestration`
        # (already relative) and `run_id`/`ts`/`pid`/`engine` (not absolute
        # paths at all) -- nothing a contributor's own checkout or temp
        # directory could leak into (`tests/conformance/normalize.py`'s
        # events comparator normalizes these at comparison time instead).
        "expected.events.jsonl": events_path.read_bytes(),
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
            pretty_out.read_bytes(), pretty=True, replacements=replacements
        )
    if server is not None:
        requests_json = [r.to_json() for r in server.requests]
        redacted_requests = redact_runtime_strings(requests_json, replacements)
        files["expected.http_requests.json"] = (
            json.dumps(redacted_requests, indent=2, ensure_ascii=True) + "\n"
        ).encode("utf-8")
    return files


def _generate_failure(
    case_dir: Path,
    metadata: dict[str, Any],
    tmp_root: Path,
    *,
    server: Any,
    replacements: list[tuple[str, str]],
) -> dict[str, bytes]:
    home_dir = tmp_root / "home"
    home_dir.mkdir(exist_ok=True)
    out_path = tmp_root / "out.json"
    leftover_replies_path = tmp_root / "leftover-replies.json"
    result = harness.run_case(
        case_dir,
        metadata,
        out_path=out_path,
        home_dir=home_dir,
        leftover_replies_path=leftover_replies_path,
        mock_http_port=server.port if server else None,
    )
    if result.returncode == 0:
        raise RuntimeError(f"{case_dir.name}: expected a failure, but `cof run` exited 0")
    _assert_no_leftover_replies(leftover_replies_path, case_name=case_dir.name)
    error = redact_runtime_strings(harness.parse_cli_error(result.stdout), replacements)
    payload = {"error": error}
    text = json.dumps(payload, indent=2, ensure_ascii=True, sort_keys=True) + "\n"
    # `cof run` writes `--out` on any failure too (run-wiring step 20,
    # issue #431): a load/check failure gets a minimal seed state plus
    # whatever of `runtime`/`effective_settings` got resolved before the
    # failure; a run failure gets the full state as it stood when the run
    # gave up. Either way it varies by *which* step failed. Captured and
    # normalized the same way a success case's state is, so electricity's
    # own `--out` on the matching failure can be compared against it, not
    # just the error text.
    return {
        "expected.json": text.encode("utf-8"),
        "expected.out.json": _redact_and_reserialize(
            out_path.read_bytes(), pretty=False, replacements=replacements
        ),
    }


def generate_case(case_dir: Path) -> dict[str, bytes]:
    metadata = harness.load_case(case_dir)
    with tempfile.TemporaryDirectory(prefix="circuitry-conformance-gen-") as tmp:
        tmp_root = Path(tmp)
        with harness.mock_http_server(case_dir, metadata) as server:
            replacements = harness.case_redaction_replacements(
                case_dir, mock_http_port=server.port if server else None
            )
            if metadata["expect"] == "success":
                return _generate_success(
                    case_dir, metadata, tmp_root, server=server, replacements=replacements
                )
            if metadata["expect"] == "failure":
                return _generate_failure(
                    case_dir, metadata, tmp_root, server=server, replacements=replacements
                )
        raise ValueError(f"{case_dir.name}: unknown case.json 'expect': {metadata['expect']!r}")


def _content_is_current(current: bytes | None, generated: bytes, *, filename: str) -> bool:
    """Compare a committed expected file to a freshly generated one.
    A `.jsonl` file (`expected.events.jsonl`) is compared with the events
    comparator (`assert_events_equal`) — it is a stream of JSON objects,
    one per line, not a single JSON document `json.loads` could parse
    whole. Everything else is compared under the suite's state normalizer
    (run id, timestamps, durations, and friends always differ byte-for-byte
    between two runs); anything that is neither (there is nothing else
    today, but `--check` should degrade safely rather than silently pass)
    falls back to exact bytes."""
    if current is None:
        return False
    if current == generated:
        return True
    if filename.endswith(".jsonl"):
        try:
            assert_events_equal(generated.decode("utf-8"), current.decode("utf-8"))
        except AssertionError:
            return False
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
                if not _content_is_current(current, content, filename=filename):
                    stale.append(str(target.relative_to(REPO_ROOT)))
            elif not _content_is_current(current, content, filename=filename):
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
