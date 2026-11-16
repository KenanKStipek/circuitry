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
import re
import sys
import tempfile
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT))

from tests.conformance import harness  # noqa: E402
from tests.conformance.normalize import (  # noqa: E402
    apply_third_party_text,
    assert_events_equal,
    assert_states_equal,
    canonicalize_http_requests,
    mask_known_third_party_text,
    mask_known_third_party_text_in,
    normalize,
    redact_events_text_pair,
    redact_leaked_paths,
    redact_runtime_strings,
    redact_third_party_text_lines,
    serialize_events_text,
)


def _check_error_not_pinning_third_party_text(
    case_dir: Path, metadata: dict[str, Any], error: str
) -> None:
    """Issue #778 step 6: an `error_compare: "exact"` (the default when the
    key is omitted) on a failure case's captured error pins it byte for
    byte -- refused when that text matches `tests/conformance/normalize.py`'s
    own message-shape catalogue (the same one `assert_errors_equal` masks
    with at comparison time), since that means it embeds a third-party
    library's own wording. Use `error_compare: "location"` instead -- the
    catalogue then masks the third-party tail and still compares every
    Circuitry-owned part (prefix, location, effect name) exactly."""
    if metadata.get("error_compare", "exact") != "exact":
        return
    if mask_known_third_party_text(error) == error:
        return
    raise ValueError(
        f'{case_dir.name}: case.json sets error_compare: "exact", but the captured error '
        f'matches tests/conformance/normalize.py\'s own message-shape catalogue -- set '
        f'error_compare: "location" instead (tests/conformance/README.md\'s "Message '
        f'shapes" section):\n  {error!r}'
    )


def _resolve_third_party_text_value(state: Any, location: str) -> Any:
    node = state
    segments = location.split(".")
    for segment in segments[:-1]:
        node = node[int(segment)] if isinstance(node, list) else node[segment]
    last = segments[-1]
    return node[int(last)] if isinstance(node, list) else node[last]


#: A sentinel `mask_known_third_party_text`'s own *placeholder* parameter
#: can be given instead of `THIRD_PARTY_TEXT_PLACEHOLDER`, for
#: `_check_third_party_text_not_redundant`'s own self-check below --
#: distinct from the real placeholder, and from anything a genuine
#: third-party message could ever contain, so a pattern that only
#: matches because it happens to also match this literal string (rather
#: than because it genuinely wildcards the masked span) is not a
#: realistic false negative.
_THIRD_PARTY_TEXT_SELF_CHECK_SENTINEL = "\x00CONFORMANCE_GENERATOR_SELF_CHECK_SENTINEL\x00"


def _check_third_party_text_not_redundant(
    case_dir: Path, specs: list[dict[str, str]], state: Any
) -> None:
    """Issue #778 step 6: a case's own `third_party_text` entry whose
    `pattern` still pins the exact text a message-shape catalogue entry
    already masks -- refused, since the entry is then redundant with (and
    no safer than) just leaving the location uncovered. A pattern that is
    a literal, non-wildcard copy of the whole captured value
    (`re.escape(value) == pattern`) is one way to do that, but not the
    only one: a pattern can wildcard some of the value while still
    spelling the catalogue's own masked span out literally (the exact gap
    `ollama-timeout-then-success-mock`'s old pattern and
    `stream-disconnect-retries`'s old alternation both had). Checked
    generally instead: mask the value with a *distinct* sentinel in place
    of `THIRD_PARTY_TEXT_PLACEHOLDER`
    (`_THIRD_PARTY_TEXT_SELF_CHECK_SENTINEL`) and see whether *pattern*
    still `re.fullmatch`es the result. If the catalogue recognised
    nothing in *value* at all, there is nothing to be redundant with, so
    this is skipped. If it did, but the pattern no longer matches once
    that masked span is replaced with unrelated text, the pattern's own
    success depended on that literal text -- it pins the catalogue's own
    masked span rather than wildcarding over it."""
    for spec in specs:
        location = spec["location"]
        pattern = spec["pattern"]
        value = _resolve_third_party_text_value(state, location)
        if not isinstance(value, str):
            continue
        if mask_known_third_party_text(value) == value:
            continue  # the catalogue recognises nothing here at all
        sentinel_value = mask_known_third_party_text(
            value, placeholder=_THIRD_PARTY_TEXT_SELF_CHECK_SENTINEL
        )
        if re.fullmatch(pattern, sentinel_value) is None:
            raise ValueError(
                f"{case_dir.name}: case.json's third_party_text location {location!r} has a "
                f"pattern that stops matching once the catalogue's own masked span is replaced "
                f"with unrelated text -- it pins text tests/conformance/normalize.py's own "
                f"message-shape catalogue already masks; drop this entry:\n  {value!r}"
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


def _warnings_text(stderr: str, metadata: dict[str, Any]) -> bytes:
    """`expected.warnings.txt` (issue #442's "Warning lines on stderr"
    item): one line per `harness.warning_lines` entry, sorted when
    `case.json` sets `sort_warnings` (a tree `dynamic` case, where two
    branches' own warnings can land in either relative order between
    runs) -- always written, even empty, so a case that stops producing
    a warning it used to is caught by `--check` the same way any other
    stale expected file is, rather than leaving a no-longer-applicable
    file silently uncompared.
    """
    # No message-shape masking here (issue #778 step 6) -- the committed
    # fixture holds the real, unmasked capture, same as `expected.json`
    # does for state; both pytest runners mask *this* file too (not just
    # the live stderr) at comparison time, so a committed file written
    # before a shape existed never needs regenerating when the catalogue
    # changes.
    lines = harness.warning_lines(stderr, sort=metadata.get("sort_warnings", False))
    text = "".join(f"{line}\n" for line in lines)
    return text.encode("utf-8")


def _run_warnings_text(stderr: str) -> bytes:
    """`expected.run_warnings.txt` -- `case.json`'s own `compare_run_
    warnings` key (issue #495): `cli/app.py::_print_run_warnings`'s own
    `Warning: ...` lines, never written unless that key is set."""
    lines = harness.run_warning_lines(stderr)
    text = "".join(f"{line}\n" for line in lines)
    return text.encode("utf-8")


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
    harness.assert_no_leftover_replies(
        leftover_replies_path, case_dir=case_dir, metadata=metadata, case_name=case_dir.name
    )
    files = {
        "expected.json": _redact_and_reserialize(
            out_path.read_bytes(), pretty=False, replacements=replacements
        ),
        # `--events` carries `path`/`orchestration` (already relative) and
        # `run_id`/`ts`/`pid`/`engine` (normalized at comparison time by the
        # events comparator instead) -- but a failed prompt dispatch's own
        # `end` event embeds the full failure message, which, for a
        # mock-HTTP-backed adapter case, can carry the mock server's own
        # ephemeral port (issue #503) the same way a captured `base_url`/
        # recorded request does, so this still needs the same redaction
        # state gets. No message-shape masking here -- `serialize_events_
        # text` commits the real, unmasked capture, same as `expected.json`
        # does for state; `redact_events_text_pair` masks both sides only
        # at comparison time (issue #778 step 6).
        "expected.events.jsonl": serialize_events_text(
            events_path.read_text(encoding="utf-8"), replacements
        ).encode("utf-8"),
        "expected.warnings.txt": _warnings_text(result.stderr, metadata),
    }
    if metadata.get("compare_run_warnings"):
        files["expected.run_warnings.txt"] = _run_warnings_text(result.stderr)
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
        canonical_requests = canonicalize_http_requests(
            redacted_requests, sort_requests=bool(metadata.get("sort_http_requests"))
        )
        files["expected.http_requests.json"] = (
            json.dumps(canonical_requests, indent=2, ensure_ascii=True) + "\n"
        ).encode("utf-8")
    third_party_text = metadata.get("third_party_text", [])
    if third_party_text:
        _check_third_party_text_not_redundant(
            case_dir, third_party_text, json.loads(files["expected.json"])
        )
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
    harness.assert_no_leftover_replies(
        leftover_replies_path, case_dir=case_dir, metadata=metadata, case_name=case_dir.name
    )
    error = redact_runtime_strings(harness.parse_cli_error(result.stdout), replacements)
    _check_error_not_pinning_third_party_text(case_dir, metadata, error)
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
    files = {
        "expected.json": text.encode("utf-8"),
        "expected.out.json": _redact_and_reserialize(
            out_path.read_bytes(), pretty=False, replacements=replacements
        ),
        "expected.warnings.txt": _warnings_text(result.stderr, metadata),
    }
    if metadata.get("compare_run_warnings"):
        files["expected.run_warnings.txt"] = _run_warnings_text(result.stderr)
    third_party_text = metadata.get("third_party_text", [])
    if third_party_text:
        _check_third_party_text_not_redundant(
            case_dir, third_party_text, json.loads(files["expected.out.json"])
        )
    return files


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


def _content_is_current(
    current: bytes | None,
    generated: bytes,
    *,
    filename: str,
    third_party_text: list[dict[str, str]],
) -> bool:
    """Compare a committed expected file to a freshly generated one.
    A `.jsonl` file (`expected.events.jsonl`) is compared with the events
    comparator (`assert_events_equal`) — it is a stream of JSON objects,
    one per line, not a single JSON document `json.loads` could parse
    whole. A `.txt` file (`expected.warnings.txt`/`expected.run_warnings.
    txt`) is a plain line per `cof run`'s own stderr warning, masked by
    the message-shape catalogue the same way both pytest runners mask it
    (`redact_third_party_text_lines`) before comparing line for line --
    these two commit the real, unmasked capture (same convention as
    `expected.json`), so a catalogue change alone must never make one
    look stale, the same as every other committed fixture. Everything
    else is compared under the suite's state normalizer (run id,
    timestamps, durations, and friends always differ byte-for-byte
    between two runs); anything that is none of these (there is nothing
    else today, but `--check` should degrade safely rather than silently
    pass) falls back to exact bytes. *third_party_text* (issue #503)
    masks a case's own curl-version-dependent leaf the same way the
    pytest runners do, before any of these comparisons -- otherwise a
    case that legitimately varies there would always show as stale."""
    if current is None:
        return False
    if current == generated:
        return True
    if filename.endswith(".jsonl"):
        try:
            generated_masked, current_masked = redact_events_text_pair(
                generated.decode("utf-8"), current.decode("utf-8"), [], third_party_text
            )
            assert_events_equal(generated_masked, current_masked)
        except AssertionError:
            return False
        return True
    if filename.endswith(".txt"):
        current_lines = redact_third_party_text_lines(
            current.decode("utf-8").splitlines(), third_party_text
        )
        generated_lines = redact_third_party_text_lines(
            generated.decode("utf-8").splitlines(), third_party_text
        )
        return current_lines == generated_lines
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
        current_normalized = normalize(mask_known_third_party_text_in(current_value))
        generated_normalized = normalize(mask_known_third_party_text_in(generated_value))
        if third_party_text:
            current_normalized = apply_third_party_text(current_normalized, third_party_text)
            generated_normalized = apply_third_party_text(generated_normalized, third_party_text)
        assert_states_equal(current_normalized, generated_normalized)
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
    failed = False
    for case_dir in case_dirs:
        metadata = harness.load_case(case_dir)
        python_runner_versions = metadata["python_runner_versions"]
        if python_runner_versions is not None:
            running_version = harness.running_python_version()
            if running_version not in python_runner_versions:
                # The case's expected output depends on the CPython version
                # running cof (CEL nesting limits, version-specific exception
                # wording) -- running this generator under any other
                # interpreter would silently rewrite the committed expected
                # output with that version's own behaviour.
                print(
                    f"SKIP {case_dir.name}: expected output is pinned to Python "
                    f"{'/'.join(python_runner_versions)}, not {running_version}",
                    file=sys.stderr,
                )
                continue

        try:
            files = generate_case(case_dir)
        except Exception as exc:  # keep going, so one run reports every failing case
            print(f"FAIL {case_dir.name}: {exc}", file=sys.stderr)
            failed = True
            continue

        third_party_text = metadata.get("third_party_text", [])
        for filename, content in files.items():
            target = case_dir / filename
            current = target.read_bytes() if target.exists() else None
            if args.check:
                if not _content_is_current(
                    current, content, filename=filename, third_party_text=third_party_text
                ):
                    stale.append(str(target.relative_to(REPO_ROOT)))
            elif not _content_is_current(
                current, content, filename=filename, third_party_text=third_party_text
            ):
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
        failed = True

    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
