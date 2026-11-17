"""Normalization rules for the conformance suite (electricity/DESIGN.md §12).

Shared by the Python runner (`test_python_runner.py`) and the electricity
runner (`test_electricity_runner.py`) so both engines are checked against
exactly the same rules — a divergence in the normalizer itself would be a
silent hole in the suite, not a caught parity bug.

Most fields are classified by **key name**, regardless of nesting depth,
per `_FIELD_RULES` below — the same field name means the same thing
everywhere in state (timestamps, durations, run ids/UUIDs). This category
is included here because it is test-*environment* dependent rather than
document-content dependent, and would otherwise make every case fail
outside the machine that generated its `expected.json`.

The absolute paths `effective_settings` echoes back (the `--out` path
itself, and the case's own directory under `_orchestration_dir` — both
absolute, both naming something on the machine and the moment that
generated `expected.json`, never portable) and the run's own orchestration
path (`orchestration_path`, already relative — stable, nothing to redact)
are the same kind of environment leakage, but are matched by their **exact
dotted location** from the state root (`_PATH_FIELD_RULES`) instead of by
key name: a key literally named `out` also occurs at
`effective_settings.sources.out`, where its value is a provenance tag
(`"cli"`/`"config"`/...), not a path, and must never be rewritten there.
The two absolute-path fields get a named, stable placeholder (`<out>`,
`<case>`) rather than their real value, both at comparison time
(`normalize()`) and — via the same table, `redact_leaked_paths()` — before
a case's `expected.json` is ever written to disk, so no contributor's own
checkout path or OS temp directory is committed.

A third category, `redact_runtime_strings`, is matched by neither key name
nor exact location: a literal substring (a case directory's own absolute
path, its `fakes/` subdirectory, a mock HTTP server's ephemeral port)
replaced wherever it appears inside any string leaf, because — unlike the
first two categories — it can land at a genuinely unpredictable key (a
subprocess timeout message naming the binary it resolved, a configured
`base_url` `effective_settings.runtime` echoes back). Applied the same way,
at both comparison time and before a case's expected state is written.
"""

from __future__ import annotations

import copy
import json
import math
import re
from collections import Counter
from collections.abc import Sequence
from datetime import datetime
from typing import Any

_UUID_RE = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$", re.IGNORECASE
)
_COMPACT_TIMESTAMP_RE = re.compile(r"^\d{8}_\d{6}$")

# Key name -> normalization kind. Checked against the *last* path segment
# (dict key) a value is stored under, independent of where in the tree it
# appears — the same field name means the same thing everywhere in state.
_FIELD_RULES: dict[str, str] = {
    "run_id": "uuid",
    "_run_id": "uuid",
    "started_at": "timestamp_iso",
    "completed_at": "timestamp_iso",
    "created_at": "timestamp_iso",
    "resolved_at": "timestamp_iso",
    "_timestamp": "timestamp_compact",
    "wall_time_s": "duration",
    "eta_s": "duration",
    "elapsed_s": "duration",
    # The `service` plugin's own `value.pgid`/`value.log` (start/stop) --
    # a process id and an absolute log path, neither stable across runs;
    # `log`'s own directory is state_dir-dependent (the test's own sandboxed
    # HOME), so only its basename is checked.
    "pgid": "pid",
    "log": "path_basename",
}

# Full dotted key-path from the state root -> normalization kind, for the
# fields that leak the test machine's own filesystem layout. Matched by
# exact location rather than key name: ``out`` and ``orchestration_path``
# are reused elsewhere in state for values that are *not* paths (a
# provenance tag, a relative filename already free of machine detail) and
# must not be touched there. A case that puts one of these fields
# somewhere new (e.g. a future CLI-stdout case needing ``state_out``) adds
# its exact path here rather than widening a key-name match.
_PATH_FIELD_RULES: dict[str, str] = {
    "runtime.effective_settings.out": "out_path",
    "runtime.effective_settings.runtime._orchestration_dir": "case_dir",
    "runtime.last_run.orchestration_path": "path_basename",
}

UUID_PLACEHOLDER = "<UUID>"
TIMESTAMP_PLACEHOLDER = "<TIMESTAMP>"
DURATION_PLACEHOLDER = "<DURATION>"
PID_PLACEHOLDER = "<PID>"

#: Stable, named placeholders for the two absolute-path fields — not a
#: generic `<PATH>` — so a reader of `expected.json` can tell *which* path
#: was redacted without cross-referencing this module. Keyed by the same
#: `_PATH_FIELD_RULES` kind, so `_normalize_leaf` (comparison time) and
#: `redact_leaked_paths` (write time) can never disagree on the token.
_PATH_PLACEHOLDERS: dict[str, str] = {
    "out_path": "<out>",
    "case_dir": "<case>",
}


class NormalizationError(AssertionError):
    """A value didn't have the shape its field name promises (e.g. a
    non-UUID string under a `run_id` key) — a real divergence, not noise to
    normalize away."""


def _normalize_leaf(key: str, value: Any, *, field_path: str | None) -> Any:
    kind = _PATH_FIELD_RULES.get(field_path) if field_path is not None else None
    if kind is None:
        kind = _FIELD_RULES.get(key)
    if kind is None:
        return value
    if kind == "uuid":
        if not isinstance(value, str) or not _UUID_RE.match(value):
            raise NormalizationError(f"{key!r}: expected a UUID string, got {value!r}")
        return UUID_PLACEHOLDER
    if kind == "timestamp_iso":
        if not isinstance(value, str):
            raise NormalizationError(f"{key!r}: expected an ISO-8601 string, got {value!r}")
        try:
            datetime.fromisoformat(value)
        except ValueError as exc:
            raise NormalizationError(f"{key!r}: not ISO-8601: {value!r}") from exc
        return TIMESTAMP_PLACEHOLDER
    if kind == "timestamp_compact":
        if not isinstance(value, str) or not _COMPACT_TIMESTAMP_RE.match(value):
            raise NormalizationError(
                f"{key!r}: expected %Y%m%d_%H%M%S, got {value!r}"
            )
        return TIMESTAMP_PLACEHOLDER
    if kind == "duration":
        # `meta.progress.eta_s` (electricity M1-H, #461) is legitimately
        # `None` before the first pass completes or when `total` itself
        # is unknown (an uncapped `while` loop) — `core/loop.py::
        # _loop_progress`'s own contract, not a missing/malformed value
        # to reject.
        if value is None:
            return None
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise NormalizationError(f"{key!r}: expected a numeric duration, got {value!r}")
        if value < 0:
            raise NormalizationError(f"{key!r}: duration must be >= 0, got {value!r}")
        return DURATION_PLACEHOLDER
    if kind == "path_basename":
        if not isinstance(value, str) or not value:
            raise NormalizationError(f"{key!r}: expected a non-empty path string, got {value!r}")
        return value.rsplit("/", 1)[-1]
    if kind == "pid":
        # `value.pgid` is `None` on a `service` stop that found nothing
        # running (`_stop`'s own no-record branch) -- a real absence, not
        # a missing field to reject.
        if value is None:
            return None
        if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
            raise NormalizationError(f"{key!r}: expected a positive pid, got {value!r}")
        return PID_PLACEHOLDER
    if kind in _PATH_PLACEHOLDERS:
        if not isinstance(value, str) or not value:
            raise NormalizationError(f"{key!r}: expected a non-empty path string, got {value!r}")
        return _PATH_PLACEHOLDERS[kind]
    raise AssertionError(f"unreachable normalization kind {kind!r}")  # pragma: no cover


def normalize(value: Any, *, key: str | None = None, field_path: str | None = None) -> Any:
    """Recursively replace volatile/environment-dependent leaves with a
    fixed placeholder, after checking the leaf has the shape its field name
    promises. Dict/list structure and key order are preserved exactly —
    order is part of what the suite compares (§3.4). `field_path` is the
    dotted path of dict keys from the state root (list indices don't widen
    it — a list's elements share their parent's path), used by
    `_PATH_FIELD_RULES` to match a field by exact location rather than by
    name alone."""
    if isinstance(value, dict):
        return {
            k: normalize(v, key=k, field_path=f"{field_path}.{k}" if field_path else k)
            for k, v in value.items()
        }
    if isinstance(value, list):
        return [normalize(v, key=key, field_path=field_path) for v in value]
    if key is None:
        return value
    return _normalize_leaf(key, value, field_path=field_path)


#: Key names whose value is an absolute, generating-machine-specific path
#: that `normalize()` only truncates to a basename (`_FIELD_RULES`'s
#: `"path_basename"` kind) -- unlike `run_id`/timestamp fields, keeping
#: that basename on disk loses nothing a committed fixture needs, so
#: `redact_leaked_paths` truncates it before writing too, the same way
#: `_PATH_FIELD_RULES` entries get their placeholder before writing. The
#: `service` plugin's own `value.log` (a start's log file, under whatever
#: `runtime.plugins.service.state_dir` the generating machine resolved) is
#: the only field here today; `runtime.last_run.orchestration_path` is the
#: same kind but already relative at run time, so it needs no write-time
#: truncation and stays out of this set.
_LEAKY_BASENAME_KEYS: frozenset[str] = frozenset({"log"})


def redact_leaked_paths(value: Any, *, field_path: str | None = None) -> Any:
    """Replace the absolute-path fields `_PATH_FIELD_RULES` names in
    `_PATH_PLACEHOLDERS` (`<out>`, `<case>`) with that stable token, and
    truncate every `_LEAKY_BASENAME_KEYS` field to its basename, leaving
    everything else (including run id/timestamp/duration fields, which must
    stay real data on disk — `normalize()` validates their *shape* at
    comparison time, and a placeholder string wouldn't have it) untouched.
    `runtime.last_run.orchestration_path` (`"path_basename"`) is already a
    relative, portable value and is deliberately not redacted here — there
    is nothing of the generating machine left in it to remove. Used by
    `scripts/generate-conformance-cases.py` before a case's expected state
    is written to disk, so a contributor's own checkout path or OS temp
    directory is never committed (electricity/DESIGN.md §12; the suite's
    own comparisons already ignore these fields via `normalize()`, so
    nothing is lost)."""
    if isinstance(value, dict):
        result = {}
        for k, v in value.items():
            child_path = f"{field_path}.{k}" if field_path else k
            placeholder = _PATH_PLACEHOLDERS.get(_PATH_FIELD_RULES.get(child_path, ""))
            if placeholder is not None:
                result[k] = placeholder
            elif k in _LEAKY_BASENAME_KEYS and isinstance(v, str) and v:
                result[k] = v.rsplit("/", 1)[-1]
            else:
                result[k] = redact_leaked_paths(v, field_path=child_path)
        return result
    if isinstance(value, list):
        return [redact_leaked_paths(v, field_path=field_path) for v in value]
    return value


def redact_runtime_strings(value: Any, replacements: Sequence[tuple[str, str]]) -> Any:
    """Recursively replace, in every string leaf regardless of key name or
    nesting location, each ``(literal, placeholder)`` pair in
    *replacements*, applied in order -- unlike `normalize()`'s key-name/
    exact-location rules, this catches a value Circuitry's own code embeds
    somewhere unpredictable: a case directory's absolute path inside a
    subprocess timeout message naming the resolved binary it ran
    (`plugins._subprocess.run_binary`'s own text), or inside a tool
    result's own ``raw.binary`` field; an ephemeral mock-HTTP-server port
    inside a configured ``base_url`` `effective_settings.runtime` echoes
    back, or inside a recorded request's own ``Host`` header. Also usable
    directly on a bare string (a failure case's own error text), not just
    a state tree. Order matters when one replacement's literal is a prefix
    of another's -- a case directory is always a prefix of its own
    ``fakes/`` subdirectory, so replace the more specific (longer) one
    first; callers build *replacements* in that order."""
    if isinstance(value, dict):
        return {k: redact_runtime_strings(v, replacements) for k, v in value.items()}
    if isinstance(value, list):
        return [redact_runtime_strings(v, replacements) for v in value]
    if isinstance(value, str):
        for literal, placeholder in replacements:
            value = value.replace(literal, placeholder)
        return value
    return value


def apply_known_divergence(state: Any, *, location: str, value: Any) -> Any:
    """A deep copy of *state* with the value at dotted *location* replaced
    by *value* -- a `case.json` `known_divergence`'s own documented,
    pinned electricity value, applied to the Python-engine-captured
    `expected.json` before it's compared against electricity's own run
    (both engines succeed; only this one value is allowed to differ).
    A segment that parses as a non-negative integer indexes a list;
    otherwise it's a dict key. Raises if *location* doesn't resolve to an
    existing leaf -- a divergence with nowhere left to land is a stale
    fixture, not something to silently skip."""
    root = copy.deepcopy(state)
    node: Any = root
    segments = location.split(".")
    for segment in segments[:-1]:
        node = node[int(segment)] if isinstance(node, list) else node[segment]
    last = segments[-1]
    if isinstance(node, list):
        node[int(last)] = value
    else:
        if last not in node:
            raise KeyError(f"known_divergence location {location!r} does not exist in state")
        node[last] = value
    return root


def normalize_for_comparison(value: Any, replacements: Sequence[tuple[str, str]]) -> Any:
    """`redact_runtime_strings`, then `mask_known_third_party_text_in`
    (the message-shape catalogue below), then `normalize()` -- the one
    call every comparison (and the generator, before writing) makes from
    here on, so a case-directory/fakes-directory/mock-port leak, a
    recognised third-party message shape, and a volatile-field
    substitution are never applied out of order or only partially."""
    return normalize(mask_known_third_party_text_in(redact_runtime_strings(value, replacements)))


# ---- Message-shape catalogue (issue #778 step 6, `tests/conformance/README.md`'s
# "Message shapes" section) ----
#
# Each entry below recognises one place Circuitry's own error text embeds a
# third-party library's wording (cel-python/the `cel` crate, PyYAML, curl,
# Python's `re`, `jsonschema`'s own `SchemaError`/a Rust `jsonschema` crate) --
# matched by a fixed, literal marker that is Circuitry's own text, never the
# library's, so a library wording change can never make the match itself
# drift. Only the text *after* the marker (named group `tp`) is ever
# replaced, with `THIRD_PARTY_TEXT_PLACEHOLDER` -- the location, the effect
# name and every other Circuitry-authored part of the same string is left
# untouched and still has to compare exactly. A string that matches no
# marker here is unaffected and still compares byte for byte.

#: `case.json`'s own `third_party_text` key (issue #503's own harness
#: addition) -- a fixed placeholder, distinct from every other one in
#: this module, for a leaf whose exact text is curl's own (version-
#: dependent) wording rather than Circuitry's: a connection-refused or a
#: timeout failure's own `meta.error`, recorded only by *shape* (a regex
#: `third_party_text` names), never by exact value. Defined before every
#: catalogue pattern below, not after: several of them default a
#: `placeholder` parameter to this constant, which Python evaluates when
#: the `def` itself runs, not when the function is later called.
THIRD_PARTY_TEXT_PLACEHOLDER = "<THIRD_PARTY_TEXT>"

#: `core/cel_eval.py`'s own `f"{where}: expression does not parse ({exc}). "
#: f"Expression: {expr!r}"` (only the `Parse` failure shape has this
#: trailing `Expression: ` repeat of the document's own expression text --
#: electricity's `CelSyntaxError::is_parse_error` reconstructs the same
#: suffix only for its own `Parse` variant, never for `TooDeeplyNested`/
#: `Empty`/`TooLong`, whose text is entirely Circuitry's own and never
#: matches this). The parser's own diagnostic (the caret-pointing
#: multi-line text inside the parens) is third-party; the repeated
#: `Expression: '<expr>'` after it is just a repr of the document's own
#: text and stays exact. Greedy + `DOTALL` so a diagnostic that itself
#: contains `). Expression: ` is still split at the *last* occurrence.
_CEL_PARSE_ERROR_RE = re.compile(
    r"CEL expression at '[^']+' \(effect '[^']+'\): expression does not parse \("
    r"(?P<tp>.*)"
    r"\)\. Expression: ",
    re.DOTALL,
)

#: `core/cel_eval.py`'s own `_error_text`'s own fixed templates -- every
#: text that function constructs itself, rather than returning a wrapped
#: library exception's message unchanged, so this is Circuitry's own
#: wording, never cel-python's: `no such key`/`no such name`/`cannot
#: select field`/`... index out of range`/`cannot index ... with ...`
#: (`_StateEvaluator.member_dot`/`member_index`), `division by zero`/
#: `modulus by zero`/`return error for overflow` (the multiplication
#: node's own `ZeroDivisionError`/`ValueError`/`OverflowError` handling),
#: `cannot use ... as a key`/`duplicate key` (`mapinits`), `undeclared
#: reference to`/`cannot compare ... to ...`/`cannot apply ... to ...`
#: (`_error_text`'s own undeclared-reference and overload branches),
#: `no such overload` and the `min()`-of-empty-sequence text (the `all`/
#: `exists`/`min` macro handling). Checked against the `tp` group's own
#: text before it is ever masked -- a match here is left alone entirely,
#: the same carve-out `_SCHEMA_WELL_FORMEDNESS_KNOWN_RE` gives Circuitry's
#: own two schema well-formedness templates. Loose on the variable parts
#: (`.+`/`\w+`, never the exact repr) since the point is only to
#: recognise *which* fixed template produced the text, not to re-validate
#: its content -- any text actually produced by one of these call sites
#: can only ever have this shape.
_CEL_EVAL_OWN_TEXT_RE = re.compile(
    r"no such key: .+"
    r"|no such name '[^']*' in \[.*\]"
    r"|cannot select field '[^']*' on (?:a|an) \w+"
    r"|\w+ index out of range: .+"
    r"|cannot index (?:a|an) \w+ with (?:a|an) \w+"
    r"|division by zero"
    r"|modulus by zero"
    r"|return error for overflow"
    r"|cannot use (?:a|an) \w+ as a key"
    r"|duplicate key: .+"
    r"|undeclared reference to '[^']*'"
    r"|cannot compare (?:a|an) \w+ to (?:a|an) \w+"
    r"|cannot apply '[^']*' to .+"
    r"|no such overload"
    r"|Attempt to reduce an empty sequence or a sequence with a None value"
)

#: `core/cel_eval.py`'s own `f"CEL evaluation failed for {expr!r}: {exc}"` --
#: everything after the colon is the evaluator's own exception text, third
#: party only when it isn't one of `_CEL_EVAL_OWN_TEXT_RE`'s own fixed
#: templates (`division by zero`, `no such key: 'zz'`, the genuinely
#: third-party leftover, cel-python's own bare `overflow` for a relational
#: comparison whose operand doesn't fit CEL's 64-bit `int` -- #400 -- being
#: the one shape this module doesn't construct itself). Non-`DOTALL`, so
#: `.` never crosses a newline: a combined-failure summary
#: (`tree-failure-class-names-3`) embeds several of these, one per line,
#: and each must stop at its own line's end rather than swallowing the
#: rest of the summary. Lazy, stopping at the first `)`/`;` too -- a
#: `Conditional`/`Loop`/`Dynamic` warning line wraps this whole phrase in
#: parens and follows it with `; on_error=...` on the same line, and a
#: leftover (non-skip-listed) third-party tail must not swallow that.
_CEL_EVAL_ERROR_RE = re.compile(
    r"CEL evaluation failed for (?:'[^']*'|\"[^\"]*\"): (?P<tp>.*?)(?=\)|;|\n|$)"
)

#: `core/yaml_load.py::_UniqueKeyLoader.construct_mapping`'s own
#: `DuplicateKeyError` -- entirely Circuitry's own text (`f"duplicate key
#: {key!r} at {where} (first defined at {first_seen[key]}); YAML would
#: silently keep only the last one"`), never PyYAML's: `yaml.safe_load`
#: itself says nothing at all about a repeated key, which is exactly why
#: this loader exists. Reaches `_YAML_PARSE_ERROR_RE` below the same way
#: any other `yaml.YAMLError` does (`DuplicateKeyError` subclasses it), so
#: without this carve-out the catalogue would mask the one YAML-parse
#: shape that is never third-party at all -- its own location (`at line
#: L, column C`) and its own clarifying sentence included.
_YAML_DUPLICATE_KEY_KNOWN_RE = re.compile(
    r"duplicate key .+ at line \d+, column \d+ \(first defined at line \d+, "
    r"column \d+\); YAML would silently keep only the last one",
    re.DOTALL,
)

#: `plugins/validate_yaml.py`/`core/decompose.py`/`core/use.py`'s own
#: `f"YAML parse error: {exc}"`, and `core/reflector.py`'s own
#: `f"parse_failed: {e}"` (a generated plan that isn't valid YAML at all,
#: issue #778) -- both wrap PyYAML's own (multi-line, caret-pointing)
#: `YAMLError` text verbatim, *except* `DuplicateKeyError`
#: (`_YAML_DUPLICATE_KEY_KNOWN_RE`), which is Circuitry's own. `DOTALL`:
#: PyYAML's own message is always multi-line and can legitimately embed a
#: quoted token followed by a comma (`'<unicode string>', line 1`) or a
#: semicolon, so this is never bounded short of the end of the string the
#: way the curl/regex shapes below are -- every call site hands this
#: shape the whole rest of the exception text with nothing of Circuitry's
#: own after it (`validate_yaml.py`/`decompose.py`/`use.py` all return the
#: wrapped message as the *only*, immediately-returned error; `reflector.py`
#: assigns it to `rec["error"]` with nothing appended after).
_YAML_PARSE_ERROR_RE = re.compile(r"(?:YAML parse error|parse_failed): (?P<tp>.*)", re.DOTALL)

#: `curl_support.py::curl_failure_message`'s own `f"... url={url}: {detail}"`
#: -- `detail` is curl's own stderr text (`curl: (N) ...`) whenever the
#: request never got a complete reply; a trailing hint (`adapters/ollama.py`
#: `_raise_curl_failure`'s own three fixed templates, joined with a literal
#: leading space by `curl_failure_message` itself) is Circuitry's own and
#: stays exact -- the only adapter that ever passes one. Keep the leading
#: space in the lookahead (not in `tp`) so the placeholder reads cleanly as
#: `<THIRD_PARTY_TEXT> <hint>` rather than swallowing that space too.
_OLLAMA_HINT_RE = (
    r" Ollama at \S+ is not reachable\. Start it \(`ollama serve`\), or set "
    r"`runtime\.adapters\.\w+\.base_url` in your config\. Run `cof doctor` to verify connectivity\."
    r"| The model didn't finish within \d+s\. Raise `runtime\.adapters\.\w+\.timeout_seconds` "
    r"in your config \(a prompt's `timeout_ms` can only shorten it\), or use a smaller/faster model\."
    r"| Try `ollama pull [^`]+` first\."
)
#: Also stops at the closing quote of an embedding Python `repr()`:
#: `detail` can land inside `All adapter attempts failed: [{...}]`'s own
#: dict/list repr (`prompt.py`'s own fallback-exhaustion message), where a
#: bare `$` would otherwise run the capture past the closing quote of the
#: attempt's own `'error'` entry and swallow every sibling key
#: (`'tokens_sent'`, ...) after it. Every curl stderr string this runtime
#: has ever captured is a single plain-English sentence with no internal
#: quoting of its own, so this lookahead only ever fires at the repr's own
#: boundary, never inside curl's own text.
_CURL_DETAIL_RE = re.compile(
    r"curl: \(\d+\) (?P<tp>.*?)(?=" + _OLLAMA_HINT_RE + r'|["\x27][,}\]]|$)', re.DOTALL
)

#: `plugins/regex.py`'s own `f"regex: invalid pattern: {exc}"`, wrapping a
#: `re.error` verbatim -- its own class is already named by the fixed
#: `regex: invalid pattern: ` prefix, and `re.error`'s own trailing
#: `" at position N"` (optionally `" (line L, column C)"`) is kept exact,
#: since it's a location, not library wording; only the message between
#: the two is third-party. `electricity-pyre` imitates CPython's own `re`
#: error wording for this shape (DESIGN.md's own "regex parity gap",
#: "accept and document", not a settled guarantee the two engines must
#: always agree on it), so this is masked even though the two happen to
#: agree today.
_REGEX_INVALID_PATTERN_RE = re.compile(
    r"regex: invalid pattern: (?P<tp>.*?)(?= at position \d+(?: \(line \d+, column \d+\))?$|$)",
    re.DOTALL,
)

#: `core/schema_corners.py::schema_well_formedness_error`'s own
#: `jsonschema.SchemaError` fallback, wrapped by every caller
#: (`core/compiler.py`, `plugins/cel.py`, `plugins/agent.py`) as
#: `f"...: schema is not a valid JSON Schema: {malformed}"` -- third-party
#: (`electricity/DESIGN.md`'s own note that the `cel` tool's malformed-
#: schema compile error "uses the jsonschema Rust crate's own text, not
#: Python's"), *except* when `malformed` is one of this module's own two
#: fixed templates (`_multiple_of_well_formedness_error`'s own
#: `MULTIPLE_OF_ERROR`, `ref_chain_error`'s own message) -- both
#: Circuitry's own text, excluded by the negative lookahead so they stay
#: exact.
_SCHEMA_WELL_FORMEDNESS_KNOWN_RE = re.compile(
    r"\$ref chain is too long \(limit \d+ consecutive references\)$"
    r"|multipleOf must be a number greater than 0$"
)
_SCHEMA_WELL_FORMEDNESS_RE = re.compile(r"(?m)is not a valid JSON Schema: (?P<tp>.*)$")

#: `plugins/systemone.py`'s own `f"...cannot be read: {exc.strerror}"` --
#: `exc.strerror` is the OS's own short message (`"No such file or
#: directory"`), never Circuitry's. The only `OSError.strerror` text a
#: document effect can surface through `cof run` (the TUI/CLI call sites
#: that format the same way -- `tui/inspector.py`, `cli/last_run.py` --
#: are never reached by a run this suite drives).
_OS_STRERROR_RE = re.compile(r"(?m)cannot be read: (?P<tp>.*)$")

#: `core/prompt_files.py`/`core/prompt.py`'s own `f"... could not be read:
#: {exc}"`/`f"... could not be started (...): {exc}"`-style wrapping of a
#: whole `OSError`, never just its `strerror` (`_OS_STRERROR_RE`, above) --
#: Python's own `str(OSError(...))` is `"[Errno N] <strerror>"`, or
#: `"[Errno N] <strerror>: '<filename>'"` once a filename is set, which it
#: is for every open/stat/read failure here. `errno` is kept exact (a
#: numbered OS condition, not library wording -- the same reasoning
#: `_REGEX_INVALID_PATTERN_RE` already gives `re.error`'s own position);
#: so is the trailing `: '<filename>'`, the actual path involved, never
#: masked -- only `strerror` itself (`electricity/DESIGN.md`'s own
#: "`electricity-compiler`'s 'could not be read' OS error text" row:
#: `std::io::Error`'s own `Display` for the same errno is never the same
#: sentence as Python's). Bounded at `: '` (the filename's own opening
#: quote) rather than `_THIRD_PARTY_TAIL_BOUNDARY`, since `strerror`
#: itself never contains a colon on any platform this runtime supports.
_OS_ERRNO_RE = re.compile(r"\[Errno \d+\] (?P<tp>[^:]+?)(?=: '|$)")


def _mask_tp_group(
    text: str,
    pattern: re.Pattern[str],
    *,
    skip: re.Pattern[str] | None = None,
    placeholder: str = THIRD_PARTY_TEXT_PLACEHOLDER,
) -> str:
    """Every match of *pattern* (which must have a named group `tp`) in
    *text*, with that group's own span replaced by *placeholder* --
    everything else in the match (and in the rest of *text*) is untouched.
    *skip*, when given, is checked against the matched `tp` text with
    `fullmatch`; a match is left alone entirely (not even the surrounding
    marker is touched, since nothing needs to change) when it matches --
    `_SCHEMA_WELL_FORMEDNESS_RE`'s own carve-out for Circuitry's two fixed
    well-formedness messages, among others. *placeholder* defaults to
    `THIRD_PARTY_TEXT_PLACEHOLDER`; `scripts/generate-conformance-cases.py`
    passes a distinct sentinel instead, to check whether a case's own
    `third_party_text` pattern still matches once the masked span's real
    content is swapped out for something else entirely."""

    def _replace(match: re.Match[str]) -> str:
        tail = match.group("tp")
        if skip is not None and skip.fullmatch(tail):
            return match.group(0)
        whole = match.group(0)
        start = match.start("tp") - match.start(0)
        end = match.end("tp") - match.start(0)
        return whole[:start] + placeholder + whole[end:]

    return pattern.sub(_replace, text)


def _mask_bulleted_location_message_lines_after(
    text: str,
    head_end: int,
    *,
    placeholder: str,
    own_line: re.Pattern[str] | None = None,
) -> str:
    """Every consecutive `  - <location>: <message>` line in *text*
    starting at *head_end* (a header's own end position), with each
    line's own `<message>` -- everything after its own first `: ` --
    replaced by *placeholder*; stops at the first line with no `: ` in it
    at all (a free-form sentence with no `<location>` prefix never
    matches, and neither does anything after it). *own_line*, when
    given, is checked against the *whole* line first: a line matching it
    is left alone entirely (it has a colon somewhere, but isn't the
    `<location>: <jsonschema message>` shape at all) *without* ending the
    scan -- `structural_errors` can mix a genuine `schema_errors` line
    with one of `core/document_check.py`'s own hand-written near-miss/
    group/interface-type sentences in the same list, at any position.
    Shared by `_mask_profile_schema_validation_lines` (`own_line=None`,
    since every line under that header is a real `jsonschema.
    ValidationError`) and `_mask_document_validation_lines` below.
    """
    lines = text[head_end:].split("\n")
    changed = False
    for i, line in enumerate(lines):
        if own_line is not None and own_line.fullmatch(line):
            continue
        match = re.match(r"(  - [^\n:]+: )(.*)", line)
        if match is None:
            break
        lines[i] = match.group(1) + placeholder
        changed = True
    if not changed:
        return text
    return text[:head_end] + "\n".join(lines)


def _mask_profile_schema_validation_lines(
    text: str, *, placeholder: str = THIRD_PARTY_TEXT_PLACEHOLDER
) -> str:
    """`cli/profiles.py::_validate_profile_schema`'s own
    `f"Profile {name!r} at {path} failed schema validation:\\n"` followed
    by one `f"  - {location}: {e.message}"` line per violation -- a real
    `jsonschema` `ValidationError.message`, third-party here (unlike the
    orchestration/inline-document schema-error lines `_mask_document_
    validation_lines` masks below, which electricity's own schema engine
    now matches word for word, issue #528 -- that header reads "<X>
    validation failed:", never "failed schema validation:", so it is
    never touched here): profiles validate against a separate Rust
    `jsonschema` crate instead, which words the same
    violation differently.
    """
    marker = "failed schema validation:\n"
    idx = text.find(marker)
    if idx == -1:
        return text
    return _mask_bulleted_location_message_lines_after(
        text, idx + len(marker), placeholder=placeholder
    )


#: `core/use.py`'s own `"Inline orchestration validation failed:\n"` /
#: `f"Orchestration {resolved_path} validation failed:\n"` /
#: `f"Orchestration {label} validation failed:\n"`, and `cli/runtime_
#: shim.py`'s own `"Orchestration validation failed:\n"` (both the
#: structural/schema gate and the unknown-concurrency-group gate) --
#: `structural_errors` (`core/document_check.py`) joins up to five kinds
#: of line under this one header: a real `schema_errors` line (`f"
#: {location}: {e.message}"`, wrapping a genuine `jsonschema.
#: ValidationError.message` -- third-party), and four of the module's
#: own hand-written sentences (`unknown_key_errors`'s near-miss text,
#: `group_field_errors`'s `'group' is only allowed on ...` text,
#: `interface_unknown_type_errors`'s `... is not a recognized type ...`
#: text, `interface_default_type_errors`'s `declared type ... but ...`
#: text) that merely happen to contain a colon too and are never
#: `jsonschema`'s own wording -- `_DOCUMENT_VALIDATION_OWN_LINE_RE` names
#: all four, plus `core/compiler.py::unknown_concurrency_group_errors`'s
#: own `group '<name>' is not defined in runtime.concurrency_groups --
#: known groups: ...` line (`cli/runtime_shim.py`'s second gate, same
#: header, no `<location>: ` prefix at all). Reaches only `meta.error`/
#: stderr/`cof check`'s own report (never anything this runtime sends
#: onward to a model) -- electricity's own schema engine matches `core.
#: document_check.schema_errors`'s wording and order exactly (issue
#: #528), so the location and the order are Circuitry's own and stay
#: exact; only a real `schema_errors` line's own jsonschema message body
#: is third-party here.
_DOCUMENT_VALIDATION_HEADER_RE = re.compile(
    r"(?:Orchestration(?: [^\n:]+)? validation failed|Inline orchestration validation failed):\n"
)

_DOCUMENT_VALIDATION_OWN_LINE_RE = re.compile(
    r"  - .+: unknown key .+ on .+ — did you mean '[^']*'\? As written it is ignored\."
    r"|  - .+: 'group' is only allowed on tool/prompt effects \(it names a "
    r"concurrency-group slot a leaf effect holds at dispatch\) — found on a '[^']*' effect\."
    r"|  - interface\.inputs\.[^\n:]+\.type: .+ is not a recognized type — expected one of .+\."
    r"|  - interface\.inputs\.[^\n:]+\.default: declared type '[^']*' but .+\."
    r"|  - group '[^']*' is not defined in runtime\.concurrency_groups — known groups: .+"
)


def _mask_document_validation_lines(
    text: str, *, placeholder: str = THIRD_PARTY_TEXT_PLACEHOLDER
) -> str:
    match = _DOCUMENT_VALIDATION_HEADER_RE.search(text)
    if match is None:
        return text
    return _mask_bulleted_location_message_lines_after(
        text,
        match.end(),
        placeholder=placeholder,
        own_line=_DOCUMENT_VALIDATION_OWN_LINE_RE,
    )


#: Every `tp`-group-bearing catalogue pattern, with its own `skip`
#: (`None` when there isn't one) -- the single list `mask_known_third_
#: party_text` walks in sequence, and `mask_known_third_party_text_to_end`
#: searches for the *earliest* match of any one of, for a field a fixed
#: byte/character cap can truncate (see that function's own docstring).
#: Neither `_mask_profile_schema_validation_lines` nor `_mask_document_
#: validation_lines` is here: both mask whole lines by a header search,
#: not one `tp`-bearing regex match, so neither has a place in an
#: "earliest match" scan, and both are applied to `text` separately in
#: `mask_known_third_party_text` only -- no committed `--events` field is
#: ever profile- or document-validation text.
_TP_GROUP_SHAPES: tuple[tuple[re.Pattern[str], re.Pattern[str] | None], ...] = (
    (_CEL_PARSE_ERROR_RE, None),
    (_CEL_EVAL_ERROR_RE, _CEL_EVAL_OWN_TEXT_RE),
    (_YAML_PARSE_ERROR_RE, _YAML_DUPLICATE_KEY_KNOWN_RE),
    (_CURL_DETAIL_RE, None),
    (_REGEX_INVALID_PATTERN_RE, None),
    (_SCHEMA_WELL_FORMEDNESS_RE, _SCHEMA_WELL_FORMEDNESS_KNOWN_RE),
    (_OS_STRERROR_RE, None),
    (_OS_ERRNO_RE, None),
)


def mask_known_third_party_text(
    text: str, *, placeholder: str = THIRD_PARTY_TEXT_PLACEHOLDER
) -> str:
    """*text* with every message-shape catalogue entry's own third-party
    tail replaced by *placeholder* (`THIRD_PARTY_TEXT_PLACEHOLDER` by
    default), applied in sequence -- the entries match disjoint, fixed
    Circuitry-authored markers, so applying them one after another (rather
    than needing one combined pattern) never double-matches the same
    span. A string with none of these markers is returned unchanged.
    *placeholder* is a parameter, not always the module constant, so
    `scripts/generate-conformance-cases.py` can mask with a distinct
    sentinel to check a case's own `third_party_text` pattern isn't
    secretly pinned to the real masked text."""
    for pattern, skip in _TP_GROUP_SHAPES:
        text = _mask_tp_group(text, pattern, skip=skip, placeholder=placeholder)
    text = _mask_profile_schema_validation_lines(text, placeholder=placeholder)
    return _mask_document_validation_lines(text, placeholder=placeholder)


#: `cli/events.py::_ERROR_MAX_CHARS` / electricity's own `events.rs::
#: ERROR_MAX_CHARS` -- the one field either engine's own `--events`
#: writer truncates, by character count, before this harness ever sees
#: it. `mask_known_third_party_text_to_end`/`redact_events_text_pair`'s
#: own docstrings explain why a field at or over this length needs a
#: different mask than everywhere else.
_EVENTS_ERROR_TRUNCATION_LIMIT = 500


def mask_known_third_party_text_to_end(text: str) -> str:
    """*text* with everything from the *earliest* third-party tail any
    catalogue shape recognises onward replaced by one
    `THIRD_PARTY_TEXT_PLACEHOLDER` -- unlike `mask_known_third_party_text`,
    which stops each shape's own mask at that shape's own tail (keeping a
    curl hint, an `Expression: ...` repr, or anything else Circuitry's own
    after it exact), this is only correct for a field a fixed-length cap
    can truncate: once the field might have been cut off *inside* a
    third-party tail, the entire *rest* of the field -- Circuitry's own
    text included -- depends on exactly how long that tail happened to be
    on this particular run, and is third-party by inheritance. Returns
    *text* unchanged when no shape matches at all."""
    earliest: int | None = None
    for pattern, skip in _TP_GROUP_SHAPES:
        match = pattern.search(text)
        if match is None:
            continue
        tail = match.group("tp")
        if skip is not None and skip.fullmatch(tail):
            continue
        start = match.start("tp")
        if earliest is None or start < earliest:
            earliest = start
    if earliest is None:
        return text
    return text[:earliest] + THIRD_PARTY_TEXT_PLACEHOLDER


#: Key names `mask_known_third_party_text_in` ever masks a string leaf
#: under -- an *error-carrying* field, by cof's own naming convention,
#: regardless of nesting depth or which container it sits inside (a
#: `child_errors`/`attempts`/`fallback_attempts` list entry's own
#: `"error"` key matches here the same way a top-level `meta.error` does,
#: with no special-casing for the container). `"errors"`/`"stderr"` cover
#: a tool result's own plural error list and the text it's joined into
#: (`plugins/validate_yaml.py`'s own `value.errors`/`meta.raw.errors`/
#: `meta.stderr`). Everything else -- `prompt_sent`, `params_rendered`,
#: `value` (except its own `errors`, above), `raw` (except its own
#: `errors`), a recorded HTTP request's `body`, and every other key name
#: -- is content Circuitry sent or received, never its own error text,
#: and is never masked here even if it happens to contain a catalogue
#: marker's literal text.
_ERROR_CARRYING_KEYS = frozenset({"error", "errors", "stderr"})


def mask_known_third_party_text_in(value: Any, *, key: str | None = None) -> Any:
    """`mask_known_third_party_text` over every string leaf of *value*
    whose own dict key is in `_ERROR_CARRYING_KEYS` -- a state tree, an
    events line's own parsed object, or a bare string (`key=None`, so
    never masked unless a caller names one) alike. Unlike `normalize()`'s
    key-based rules, nesting depth and the parent container's own name
    are irrelevant -- only the leaf's own key."""
    if isinstance(value, dict):
        return {k: mask_known_third_party_text_in(v, key=k) for k, v in value.items()}
    if isinstance(value, list):
        return [mask_known_third_party_text_in(v, key=key) for v in value]
    if isinstance(value, str) and key in _ERROR_CARRYING_KEYS:
        return mask_known_third_party_text(value)
    return value


def _resolve_container(root: Any, segments: Sequence[str]) -> Any:
    node = root
    for segment in segments:
        node = node[int(segment)] if isinstance(node, list) else node[segment]
    return node


def apply_third_party_text(state: Any, specs: Sequence[dict[str, str]]) -> Any:
    """A deep copy of *state* with every `third_party_text` leaf
    (`case.json`'s own `[{"location": ..., "pattern": ...}]`, issue
    #503) replaced by `THIRD_PARTY_TEXT_PLACEHOLDER`, after checking it
    `re.fullmatch`es its own `pattern` first -- a location whose value
    doesn't match is a real divergence (or a stale fixture), never
    silently accepted. Call this on *both* the actual and the expected
    state, independently, before `assert_states_equal`: each side's own
    leaf has to satisfy the pattern on its own, and the placeholder then
    makes the two sides compare equal there regardless of curl's own
    (version-dependent) wording. Dotted `location`, same convention as
    `apply_known_divergence`: a segment that parses as a non-negative
    integer indexes a list, otherwise it's a dict key.
    """
    root = copy.deepcopy(state)
    for spec in specs:
        location = spec["location"]
        pattern = spec["pattern"]
        segments = location.split(".")
        node = _resolve_container(root, segments[:-1])
        last = segments[-1]
        if isinstance(node, list):
            index = int(last)
            value = node[index]
        else:
            if last not in node:
                raise KeyError(f"third_party_text location {location!r} does not exist in state")
            value = node[last]
        if not isinstance(value, str) or re.fullmatch(pattern, value) is None:
            raise AssertionError(
                f"third_party_text location {location!r} does not fullmatch pattern "
                f"{pattern!r}: {value!r}"
            )
        if isinstance(node, list):
            node[int(last)] = THIRD_PARTY_TEXT_PLACEHOLDER
        else:
            node[last] = THIRD_PARTY_TEXT_PLACEHOLDER
    return root


def prepare_for_comparison(
    value: Any,
    replacements: Sequence[tuple[str, str]],
    third_party_text: Sequence[dict[str, str]] = (),
) -> Any:
    """`normalize_for_comparison` then, when *third_party_text* is
    non-empty, `apply_third_party_text` -- the one call both pytest
    runners make wherever a case might carry a `third_party_text` spec,
    so the two steps are never applied out of order or only partially.
    """
    normalized = normalize_for_comparison(value, replacements)
    if not third_party_text:
        return normalized
    return apply_third_party_text(normalized, third_party_text)


def _redact_third_party_patterns(value: Any, patterns: Sequence[str]) -> Any:
    """Like `redact_runtime_strings`, but a regex *search*-and-replace
    (never a `fullmatch`) for each of *patterns*, applied to every string
    leaf -- an `--events` line's own `error` field (`cli/events.py`'s own
    500-character truncation, electricity/src/events.rs's own matching
    cap) can cut a `third_party_text` match off *after* the span a
    pattern actually covers while still containing that whole span
    intact, which a `fullmatch` (state's own rule, `apply_third_party_
    text`) would wrongly reject outright. A pattern that doesn't occur in
    a given string is simply a no-op there (`re.sub` with no match).
    """
    if isinstance(value, dict):
        return {k: _redact_third_party_patterns(v, patterns) for k, v in value.items()}
    if isinstance(value, list):
        return [_redact_third_party_patterns(v, patterns) for v in value]
    if isinstance(value, str):
        for pattern in patterns:
            value = re.sub(pattern, THIRD_PARTY_TEXT_PLACEHOLDER, value)
        return value
    return value


def redact_third_party_text_lines(
    lines: Sequence[str], third_party_text: Sequence[dict[str, str]]
) -> list[str]:
    """`_redact_third_party_patterns`, applied to each of *lines* --
    `expected.warnings.txt`'s own `WARNING:`/`ERROR:` lines can embed a
    third-party exception's own text (`core/cel_eval.py`'s own
    `logger.error` call for a failed `if`/`while` CEL condition, among
    others) the same way a tool's `meta.error` can, so this follows the
    same substring (never `fullmatch`) convention `redact_events_text`
    already uses for `--events` lines -- applied both before a case's
    `expected.warnings.txt` is ever written and at comparison time (both
    pytest runners), so the two can never drift.

    `core/cel_eval.py::evaluate_cel`'s own `logger.error` call spells a
    CEL evaluation failure "for expr %r", never "for %r" the way its
    own raise-site text does (`cel_eval.py:132/157/194/207`) -- the one
    place `expr` appears only on the log line, both engines' own
    `log::error!`/`logger.error` call alike. Normalized back to the
    raise-site's own wording first, so a `third_party_text` pattern
    crafted against `meta.error`'s own text (never "for expr") still
    matches this line's text too. `mask_known_third_party_text` (the
    message-shape catalogue) runs unconditionally, the same way
    `redact_events_text` runs it on every events line regardless of
    whether the case also declares a `third_party_text` spec.
    """
    normalized = [
        line.replace("CEL evaluation failed for expr ", "CEL evaluation failed for ")
        for line in lines
    ]
    normalized = [mask_known_third_party_text(line) for line in normalized]
    if not third_party_text:
        return normalized
    patterns = [spec["pattern"] for spec in third_party_text]
    return [_redact_third_party_patterns(line, patterns) for line in normalized]


def _redact_events_lines(
    events: list[dict[str, Any]],
    replacements: Sequence[tuple[str, str]],
    third_party_text: Sequence[dict[str, str]],
    truncatable_paths: frozenset[str],
) -> str:
    """`redact_events_text`/`redact_events_text_pair`'s shared body: each
    already-parsed event, `redact_runtime_strings`-redacted, then
    catalogue-masked -- with its own `error` field (if any) remasked with
    `mask_known_third_party_text_to_end` instead, mask-to-end rather than
    mask-to-tail, when its own `path` is in *truncatable_paths* -- and,
    when *third_party_text* is given, `_redact_third_party_patterns` on
    top, then re-serialized one JSON object per line in the exact wire
    format (`separators=(",", ":")`, `ensure_ascii=True`)
    `_assert_compact_wire_format` itself checks for."""
    patterns = [spec["pattern"] for spec in third_party_text]
    lines = []
    for event in events:
        redacted = redact_runtime_strings(event, replacements)
        raw_error = redacted.get("error")
        masked = mask_known_third_party_text_in(redacted)
        if redacted.get("path") in truncatable_paths and isinstance(raw_error, str):
            masked = {**masked, "error": mask_known_third_party_text_to_end(raw_error)}
        if patterns:
            masked = _redact_third_party_patterns(masked, patterns)
        lines.append(json.dumps(masked, separators=(",", ":"), ensure_ascii=True))
    return "".join(f"{line}\n" for line in lines)


def _parse_event_lines_raw(text: str) -> list[dict[str, Any]]:
    return [json.loads(line) for line in text.splitlines() if line.strip()]


def serialize_events_text(text: str, replacements: Sequence[tuple[str, str]]) -> str:
    """`redact_runtime_strings` applied to each `--events` JSONL line's own
    parsed object, then re-serialized in the exact wire format
    (`separators=(",", ":")`, `ensure_ascii=True`) `_assert_compact_wire_
    format` itself checks for -- no message-shape masking at all, so
    `scripts/generate-conformance-cases.py` commits a case's
    `expected.events.jsonl` holding the real, unmasked capture, the same
    convention `expected.json` already follows for state: masking only
    ever happens at comparison time (`redact_events_text_pair`), never
    baked into a committed fixture, so a later catalogue change (a new
    shape, a narrower or wider mask) never makes a committed file stale
    on its own.
    """
    lines = [
        json.dumps(redact_runtime_strings(event, replacements), separators=(",", ":"), ensure_ascii=True)
        for event in _parse_event_lines_raw(text)
    ]
    return "".join(f"{line}\n" for line in lines)


def redact_events_text(
    text: str,
    replacements: Sequence[tuple[str, str]],
    third_party_text: Sequence[dict[str, str]] = (),
) -> str:
    """`_redact_events_lines`, with no `--events` field treated as
    truncatable -- issue #503's own harness addition: a failed prompt
    dispatch's own `end` event embeds the full failure message (`cli/
    events.py`'s own event record), which, for a mock-HTTP-backed adapter
    case, carries the mock server's ephemeral port the same way a
    captured `base_url`/recorded-request host does, and can carry the
    same curl-version-dependent text a case's own `third_party_text` spec
    already masks in state. Comparing one stream in isolation (the only
    way to call this rather than `redact_events_text_pair`) can't tell
    whether its own `error` field was truncated *relative to the other
    side*, so this never applies the mask-to-end treatment -- call
    `redact_events_text_pair` instead whenever both an actual and an
    expected stream are being compared against each other.
    """
    return _redact_events_lines(
        _parse_event_lines_raw(text), replacements, third_party_text, frozenset()
    )


def redact_events_text_pair(
    actual_text: str,
    expected_text: str,
    replacements: Sequence[tuple[str, str]],
    third_party_text: Sequence[dict[str, str]] = (),
) -> tuple[str, str]:
    """`redact_events_text`'s own pair-aware twin for the one field cof's
    and electricity's own `--events` writer each truncate to a fixed
    length (`_EVENTS_ERROR_TRUNCATION_LIMIT`, an `end` event's own
    `error`) -- call this, never two separate `redact_events_text` calls,
    whenever both streams are being compared against each other, so a
    path whose raw `error` hit that cap on *either* side gets the same
    aggressive, mask-to-end-of-field treatment (`mask_known_third_party_
    text_to_end`) on both: once a recognised shape's own third-party tail
    might have been cut off partway through, everything after where that
    tail starts depends on the tail's own length on this particular run,
    so it's third-party by inheritance too -- not just the shape's own
    un-truncated extent `mask_known_third_party_text` would stop at
    (`tests/conformance/README.md`'s own "Message shapes" section). A
    path present on only one side, or whose raw `error` is under the cap
    on both sides, is unaffected -- `redact_events_text`'s own per-line
    masking still applies to everything else, on both sides alike.
    """
    actual_events = _parse_event_lines_raw(actual_text)
    expected_events = _parse_event_lines_raw(expected_text)
    truncatable_paths = frozenset(
        event["path"]
        for events in (actual_events, expected_events)
        for event in events
        if isinstance(event.get("path"), str)
        and isinstance(event.get("error"), str)
        and len(event["error"]) >= _EVENTS_ERROR_TRUNCATION_LIMIT
    )
    return (
        _redact_events_lines(actual_events, replacements, third_party_text, truncatable_paths),
        _redact_events_lines(expected_events, replacements, third_party_text, truncatable_paths),
    )


def normalize_recorded_http_requests(value: Any, replacements: Sequence[tuple[str, str]]) -> Any:
    """`redact_runtime_strings` then `normalize()` -- *never*
    `mask_known_third_party_text_in`, unlike `normalize_for_comparison`: a
    recorded HTTP request's own `body`/`headers`/`query` is content
    Circuitry sent *onward* (a prompt's rendered text, a tool's rendered
    params), not Circuitry's own error text, so none of it is third-party
    wording the message-shape catalogue should ever replace (issue #778
    step 6) -- a request body that happened to contain, say, the literal
    substring "CEL evaluation failed for" as ordinary content would
    otherwise have that content silently replaced with the placeholder
    before `assert_recorded_http_requests` ever compares it."""
    return normalize(redact_runtime_strings(value, replacements))


def canonicalize_http_headers(headers: dict[str, Any] | list[list[str]]) -> list[list[str]]:
    """A recorded HTTP request's own ``headers`` -- either a mapping (a
    freshly recorded request, `mock_http.RecordedRequest.to_json`) or a list
    of ``[name, value]`` pairs (a committed `expected.http_requests.json`,
    already in this function's own canonical shape) -- as a list of
    ``[lowercased name, value]`` pairs sorted by that same key. The header
    rule both the Python and electricity conformance runners compare under
    (``tests/conformance/README.md``'s "Mock HTTP server" section): header
    *names* are compared case-insensitively, the whole *set* is compared
    order-insensitively (two clients are free to send the same headers in a
    different order), but a *value* is still compared exactly. Sorting by
    ``(lowercased name, value)`` turns both of those into a single ordered
    structure plain list/dict equality already checks correctly, without a
    bespoke set-comparison here. ``User-Agent`` never reaches this function
    at all -- ``mock_http.MockHttpServer`` excludes it at record time,
    before either engine's own request even reaches a fixture."""
    pairs = headers.items() if isinstance(headers, dict) else headers
    return sorted([str(k).lower(), str(v)] for k, v in pairs)


def canonicalize_http_requests(
    requests: list[dict[str, Any]], *, sort_requests: bool
) -> list[dict[str, Any]]:
    """Every recorded HTTP request's own ``headers`` canonicalized
    (`canonicalize_http_headers`), then, only when *sort_requests* is set
    (``case.json``'s ``sort_http_requests`` -- a tree-flow case, whose
    concurrent branches may dispatch their own HTTP calls in a different
    wall-clock order on either engine), the whole list re-ordered by its own
    JSON representation into one fixed, engine-independent order. A case
    with no concurrent dispatch leaves *sort_requests* `False` and keeps
    arrival order, which is itself part of what's being checked there (a
    request issued out of the document's own order is a real divergence,
    not noise to normalize away)."""
    canonical = [{**r, "headers": canonicalize_http_headers(r["headers"])} for r in requests]
    if sort_requests:
        canonical.sort(key=lambda r: json.dumps(r, sort_keys=True))
    return canonical


def assert_states_equal(actual: Any, expected: Any, *, path: str = "$") -> None:
    """Order-sensitive structural equality after `normalize()`. Raises
    `AssertionError` naming the first differing path."""
    if isinstance(actual, dict) and isinstance(expected, dict):
        if list(actual.keys()) != list(expected.keys()):
            raise AssertionError(
                f"{path}: key order/set differs:\n"
                f"  actual:   {list(actual.keys())}\n"
                f"  expected: {list(expected.keys())}"
            )
        for k in actual:
            assert_states_equal(actual[k], expected[k], path=f"{path}.{k}")
        return
    if isinstance(actual, list) and isinstance(expected, list):
        if len(actual) != len(expected):
            raise AssertionError(
                f"{path}: length differs: {len(actual)} != {len(expected)}"
            )
        for i, (a, e) in enumerate(zip(actual, expected, strict=True)):
            assert_states_equal(a, e, path=f"{path}[{i}]")
        return
    # `NaN != NaN` under Python's own `==`, but a complexity score's
    # `signals.*.normalized`/`contribution` can legitimately be NaN on
    # both engines for the same non-finite input (issue #516 §3) --
    # two NaNs are the same divergence-free value here, not a
    # mismatch.
    if (
        isinstance(actual, float)
        and isinstance(expected, float)
        and math.isnan(actual)
        and math.isnan(expected)
    ):
        return
    if actual != expected:
        raise AssertionError(f"{path}: {actual!r} != {expected!r}")


def assert_values_equal(actual: Any, expected: Any, *, path: str = "$") -> None:
    """Structural equality after `normalize()`, ignoring dict key order —
    for asserting plain and `--pretty` carry the *same value*, independent
    of the layout difference `assert_states_equal` itself enforces."""
    if isinstance(actual, dict) and isinstance(expected, dict):
        if set(actual.keys()) != set(expected.keys()):
            raise AssertionError(
                f"{path}: key set differs:\n"
                f"  actual:   {sorted(actual.keys())}\n"
                f"  expected: {sorted(expected.keys())}"
            )
        for k in actual:
            assert_values_equal(actual[k], expected[k], path=f"{path}.{k}")
        return
    if isinstance(actual, list) and isinstance(expected, list):
        if len(actual) != len(expected):
            raise AssertionError(f"{path}: length differs: {len(actual)} != {len(expected)}")
        for i, (a, e) in enumerate(zip(actual, expected, strict=True)):
            assert_values_equal(a, e, path=f"{path}[{i}]")
        return
    if actual != expected:
        raise AssertionError(f"{path}: {actual!r} != {expected!r}")


def assert_errors_equal(
    actual: str,
    expected: str,
    *,
    byte_for_byte: bool,
    location_pattern: str | None = None,
) -> None:
    """Compare a failure's message per §12: Circuitry's own messages
    byte-for-byte (`byte_for_byte=True`, the raw text, never masked --
    pinning it exactly is only valid when it names no third-party shape
    at all, which `scripts/generate-conformance-cases.py` checks
    statically); a third-party library's message (`byte_for_byte=False`)
    first through `mask_known_third_party_text` (the message-shape
    catalogue): if that alone makes the two sides equal, Circuitry's own
    part (prefix, location, effect name) matched exactly and the
    third-party tail is masked out, so there's nothing left to check.
    Otherwise, only when *location_pattern* is given **and** masking
    recognised no shape in *either* side (neither text changed), fall
    back to the older, weaker check this replaces for a shape the
    catalogue doesn't recognise yet: "failed in the same place, with a
    non-empty message" -- never byte-for-byte. A shape *was* recognised
    somewhere but the two sides still disagree is never excused by a
    `location_pattern` -- that's a real divergence in the one part of
    the message (prefix, location, effect name) masking never touches,
    and the fallback's own weaker rule would let it through unnoticed."""
    if byte_for_byte:
        if actual != expected:
            raise AssertionError(
                f"error text differs:\n  actual:   {actual!r}\n  expected: {expected!r}"
            )
        return
    if not actual:
        raise AssertionError("actual error message is empty")
    if not expected:
        raise AssertionError("fixture's expected error message is empty")
    masked_actual = mask_known_third_party_text(actual)
    masked_expected = mask_known_third_party_text(expected)
    if masked_actual == masked_expected:
        return
    shape_recognised = masked_actual != actual or masked_expected != expected
    if location_pattern is None or shape_recognised:
        raise AssertionError(
            "error text differs even after masking every known third-party shape:\n"
            f"  actual:   {masked_actual!r}\n  expected: {masked_expected!r}"
        )
    actual_match = re.search(location_pattern, actual)
    expected_match = re.search(location_pattern, expected)
    if actual_match is None or expected_match is None:
        raise AssertionError(
            f"location pattern {location_pattern!r} not found in one of the two "
            f"error messages:\n  actual:   {actual!r}\n  expected: {expected!r}"
        )
    if actual_match.group(0) != expected_match.group(0):
        raise AssertionError(
            f"failed at different locations: "
            f"{actual_match.group(0)!r} != {expected_match.group(0)!r}"
        )


def assert_out_serialization(raw_text: str, *, pretty: bool) -> None:
    """Re-derive `--out`'s exact byte layout from its own parsed content and
    assert it round-trips (C23, §3.4.1): plain is insertion-order, no
    indent; `--pretty` is alphabetically sorted, 2-space indent. Both always
    `ensure_ascii=True` and end with exactly one trailing newline.

    `sort_keys=True` is safe here even for a case whose *live* state mixed
    dict key types (#778): `data` comes from `json.loads`, which only ever
    produces string keys, so the mixed-type dict that could make a plain
    `sort_keys=True` raise never survives the round trip to reach this
    sort -- it already turned into an all-string-keyed dict sorted the same
    way either rule would sort it."""
    data = json.loads(raw_text)
    if pretty:
        expected_text = json.dumps(data, indent=2, sort_keys=True, ensure_ascii=True) + "\n"
    else:
        expected_text = json.dumps(data, separators=(", ", ": "), ensure_ascii=True) + "\n"
    if expected_text != raw_text:
        raise AssertionError(
            f"--out{' --pretty' if pretty else ''} is not the expected serialization "
            f"(insertion order + no indent for plain; sorted keys + 2-space indent for "
            f"--pretty; both ensure_ascii) — see electricity/DESIGN.md §3.4.1"
        )


_EVENT_TS_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$")

EVENT_TS_PLACEHOLDER = "<EVENT_TS>"
EVENT_MS_PLACEHOLDER = "<MS>"
EVENT_PID_PLACEHOLDER = "<PID>"
EVENT_ENGINE_PLACEHOLDER = "<ENGINE>"

#: `--events` fields normalized like any other volatile field (runtime-
#: semantics §8.7, issue #431's events-conformance acceptance criterion):
#: shape-checked, then replaced with a fixed placeholder. `run_id` reuses
#: the same UUID check/placeholder `normalize()` uses for state's own
#: `run_id`/`_run_id`. `first_token_ms` (#674, a `progress` line) is the
#: same kind of wall-clock-derived value as `ms` and gets the same
#: placeholder.
_EVENT_FIELD_RULES = ("ts", "ms", "first_token_ms", "run_id", "pid", "engine")

#: `run_start`'s own `config` field is not a `_EVENT_FIELD_RULES` entry --
#: unlike `ts`/`ms`/`run_id`/`pid`/`engine`, it is not placeholdered the
#: same way on both sides. A case that names a `config` in `case.json`
#: gives both engines that same name -- except a case run against the mock
#: server, where `harness.materialize_config` hands both engines its own
#: `materialized-<config>` instead (the port it substitutes is different on
#: every run); either way, the two engines' own actual streams and the
#: committed fixture all carry the identical string, compared as-is. A case
#: that names none gives `cof` nothing (it writes `null`, discovery never
#: naming a single file) but still has to give electricity's required
#: config argument *something*: the conformance electricity runner's own
#: placeholder config (`test_electricity_runner.DEFAULT_CONFIG_PATH`, named
#: by this file only through its fixed basename below, never a path).
#: `_check_run_start_config` accepts either value on that side, rather than
#: ever placeholdering `config` itself the way `engine` is -- that would
#: leave this whole field untested.
_ELECTRICITY_RUNNER_DEFAULT_CONFIG_NAME = "default_config.json"

#: `run_start.config` is always present (never omitted) on both engines --
#: a sentinel, not a default of `None`, so a stream from before this field
#: existed (missing the key outright, distinct from carrying it as `null`)
#: is caught as a mismatch rather than silently treated as `null`.
_RUN_START_CONFIG_MISSING = object()


def _check_run_start_config(actual: Any, expected: Any) -> None:
    if actual is _RUN_START_CONFIG_MISSING or expected is _RUN_START_CONFIG_MISSING:
        raise AssertionError(
            "run_start.config is missing from one side -- a stream from before this "
            f"field existed:\n  actual:   {actual!r}\n  expected: {expected!r}"
        )
    if expected is not None:
        if actual != expected:
            raise AssertionError(
                f"run_start.config differs:\n  actual:   {actual!r}\n  expected: {expected!r}"
            )
        return
    if actual not in (None, _ELECTRICITY_RUNNER_DEFAULT_CONFIG_NAME):
        raise AssertionError(
            "run_start.config differs: expected null (no config named by the case) "
            f"or {_ELECTRICITY_RUNNER_DEFAULT_CONFIG_NAME!r} (the electricity runner's "
            f"own placeholder), got {actual!r}"
        )


def _normalize_event_field(key: str, value: Any) -> Any:
    if key == "ts":
        if not isinstance(value, str) or not _EVENT_TS_RE.match(value):
            raise NormalizationError(f"event {key!r}: not a millisecond UTC timestamp: {value!r}")
        return EVENT_TS_PLACEHOLDER
    if key in ("ms", "first_token_ms"):
        # `progress`'s own `ms`/`first_token_ms` (#674) can be `null` --
        # unlike `end`'s `ms`, which is omitted rather than nulled when
        # there's nothing to report (see `cli/events.py::on_complete`).
        if value is None:
            return None
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise NormalizationError(f"event {key!r}: expected a non-negative int, got {value!r}")
        return EVENT_MS_PLACEHOLDER
    if key == "run_id":
        if not isinstance(value, str) or not _UUID_RE.match(value):
            raise NormalizationError(f"event {key!r}: expected a UUID string, got {value!r}")
        return UUID_PLACEHOLDER
    if key == "pid":
        if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
            raise NormalizationError(f"event {key!r}: expected a positive int, got {value!r}")
        return EVENT_PID_PLACEHOLDER
    if key == "engine":
        if not isinstance(value, str) or not value:
            raise NormalizationError(f"event {key!r}: expected a non-empty string, got {value!r}")
        return EVENT_ENGINE_PLACEHOLDER
    raise AssertionError(f"unreachable event field {key!r}")  # pragma: no cover


def _comparable_event(event: dict[str, Any]) -> dict[str, Any]:
    """One event line, ready to compare across engines: `ts`/`ms`/
    `run_id`/`pid`/`engine` shape-checked and replaced with a fixed
    placeholder (`_normalize_event_field`); `seq` and the per-instance
    `id` dropped outright rather than normalized — both are assigned from
    one counter in *dispatch* order, and a tree dynamic's branches are
    free to start/finish in a different wall-clock order on the two
    engines (cof's real OS threads vs. electricity's single-threaded
    cooperative scheduler); comparing their absolute values would assert
    an implementation detail neither engine commits to. Every other key
    (`v`, `ev`, `path`, `ok`, `error`, `branches`, `concurrency`,
    `orchestration`, `config`, `signal`) is compared as-is -- `config`
    only here; `assert_events_equal` pops it back out of a `run_start`
    comparison for its own dedicated rule (`_check_run_start_config`),
    since unlike every other key here it is not always expected to
    compare equal across engines."""
    comparable = {k: v for k, v in event.items() if k not in ("seq", "id")}
    for key in _EVENT_FIELD_RULES:
        if key in comparable:
            comparable[key] = _normalize_event_field(key, comparable[key])
    return comparable


def parse_event_lines(text: str) -> list[dict[str, Any]]:
    """One JSON object per non-empty line, in file order (`--events`'
    JSONL format, runtime-semantics §8.7)."""
    return [json.loads(line) for line in text.splitlines() if line.strip()]


def _assert_compact_wire_format(text: str, *, label: str) -> None:
    """Every non-empty line of *text* must already be exactly
    `json.dumps(json.loads(line), separators=(",", ":"))` -- the wire
    format runtime-semantics §8.7 specifies (`cli/events.py::EventLog.
    _write_line`'s own `separators=(",", ":")`; `ensure_ascii` left at
    its default `True`, so this never passes `ensure_ascii=False` here
    either). Catches an engine whose events happen to parse the same but
    were written with different separators -- `assert_events_equal`'s
    own per-field comparison below parses every line first, so it would
    never notice that on its own."""
    for i, line in enumerate(text.splitlines()):
        if not line.strip():
            continue
        expected = json.dumps(json.loads(line), separators=(",", ":"))
        if line != expected:
            raise AssertionError(
                f"{label}: line {i} is not in the exact --events wire format:\n"
                f"  actual:   {line!r}\n  expected: {expected!r}"
            )


def _split_and_check_order(
    events: list[dict[str, Any]], *, label: str
) -> tuple[dict[str, Any], list[dict[str, Any]], dict[str, Any]]:
    """Checks the structural ordering rule runtime-semantics §8.7
    specifies (`run_start` first, `run_end` last, every container's own
    `start` before any child's `start` and every child's `end` before its
    container's `end`), then splits off the two bracket events. Applied to
    *one* engine's own stream at a time — this is an internal-consistency
    check on that stream, not a cross-engine comparison; `assert_events_equal`
    calls it once per side precisely so a stream that is internally
    consistent but differs from the other engine's is still caught by the
    per-path comparison below, not mistaken for an ordering bug."""
    if not events:
        raise AssertionError(f"{label}: event stream is empty")
    if events[0].get("ev") != "run_start":
        raise AssertionError(f"{label}: event stream's first line is not run_start: {events[0]!r}")
    if events[-1].get("ev") != "run_end":
        raise AssertionError(f"{label}: event stream's last line is not run_end: {events[-1]!r}")

    starts: dict[str, int] = {}
    ends: dict[str, int] = {}
    for i, event in enumerate(events):
        path = event.get("path")
        if path is None:
            continue
        if event.get("ev") == "start":
            starts.setdefault(path, i)
        elif event.get("ev") == "end":
            ends[path] = i

    for child_path, child_start in starts.items():
        for parent_path, parent_start in starts.items():
            if parent_path == child_path or not child_path.startswith(f"{parent_path}."):
                continue
            if not parent_start < child_start:
                raise AssertionError(
                    f"{label}: {parent_path!r}'s start does not precede its child "
                    f"{child_path!r}'s start"
                )
            parent_end = ends.get(parent_path)
            child_end = ends.get(child_path)
            if parent_end is not None and child_end is not None and not child_end < parent_end:
                raise AssertionError(
                    f"{label}: {child_path!r}'s end does not precede its container "
                    f"{parent_path!r}'s end"
                )

    return events[0], events[1:-1], events[-1]


def _group_events_by_path(events: list[dict[str, Any]]) -> dict[str, list[dict[str, Any]]]:
    """Every event that carries a `path`, in stream order, grouped by that
    path. A path's own events have a fixed internal order in M0-H --
    `start`, then (a tree container only) `dispatch`, then `end` -- so
    comparing each path's own list *as a list*, not a multiset, catches an
    end-before-start or a dispatch-after-end on the same path, which a
    `collections.Counter` comparison cannot: distinct `ev` values compare
    equal as a set regardless of which order they came in.

    A path that carries more than one `start` (several concurrent tree
    passes of an unnamed loop sharing one compiled body path -- a named
    loop's own `Frame::with_pass` resolution gives every pass a distinct
    path instead) is the one case where that ordered-list comparison is
    too strict: genuinely concurrent passes are free to interleave in a
    different order on either engine. `assert_events_equal` compares such
    a path's own events as a sorted multiset instead, via
    [`_raw_group_events_by_path`]'s own id-pairing check first."""
    groups: dict[str, list[dict[str, Any]]] = {}
    for event in events:
        path = event.get("path")
        if path is None:
            continue
        groups.setdefault(path, []).append(_comparable_event(event))
    return groups


def _raw_group_events_by_path(
    events: list[dict[str, Any]],
) -> dict[str, list[dict[str, Any]]]:
    """Like [`_group_events_by_path`], but keeps every event exactly as
    parsed -- `id` included -- for [`_check_instance_pairing`]'s own use.
    `id` values aren't comparable *across* engines (`_comparable_event`'s
    own doc comment: assigned from one counter in dispatch order, which a
    tree dynamic's branches don't commit to matching between engines), but
    pairing a `start` with its own `end` *within one engine's own stream*
    is exactly what `id` is for."""
    groups: dict[str, list[dict[str, Any]]] = {}
    for event in events:
        path = event.get("path")
        if path is None:
            continue
        groups.setdefault(path, []).append(event)
    return groups


def _check_instance_pairing(events: list[dict[str, Any]], *, label: str, path: str) -> None:
    """Within *path*'s own raw event list (one engine's own stream, `id`
    still attached): every `id` that starts must start before it ends, and
    every `id` that starts must eventually end. Only meaningful -- and only
    called -- for a path with more than one `start` (`_group_events_by_
    path`'s own doc comment); a single-start path already has its full
    order checked by the plain list comparison."""
    started_at: dict[Any, int] = {}
    ended: set[Any] = set()
    for i, event in enumerate(events):
        instance = event.get("id")
        ev = event.get("ev")
        if ev == "start":
            if instance in started_at:
                raise AssertionError(f"{label}: {path!r} id {instance!r} starts more than once")
            started_at[instance] = i
        elif ev == "end":
            if instance not in started_at:
                raise AssertionError(f"{label}: {path!r} id {instance!r} ends before it starts")
            ended.add(instance)
    missing = set(started_at) - ended
    if missing:
        raise AssertionError(f"{label}: {path!r} id(s) {sorted(missing)} never end")


def _chain_sibling_order(events: list[dict[str, Any]]) -> dict[str, list[str]]:
    """For every container path whose own stream carries no `dispatch`
    event (a chain, never a tree -- `fire_concurrent_dispatch` only ever
    fires for a tree-flow dynamic), the order its own *direct* children's
    `start` events occurred in, keyed by the container's path. A chain's
    children run strictly one after another, in document order, on both
    engines -- unlike a tree dynamic's branches, which are free to
    interleave with each other on either engine (cof's real OS threads vs.
    electricity's single-threaded cooperative scheduler) and are
    deliberately excluded here by the `dispatch`-event check."""
    dispatch_paths = {
        event["path"]
        for event in events
        if event.get("ev") == "dispatch" and event.get("path") is not None
    }
    order: dict[str, list[str]] = {}
    for event in events:
        if event.get("ev") != "start":
            continue
        path = event.get("path")
        if path is None or "." not in path:
            continue
        parent = path.rsplit(".", 1)[0]
        if parent in dispatch_paths:
            continue
        order.setdefault(parent, []).append(path)
    return order


def assert_events_equal(actual_text: str, expected_text: str) -> None:
    """Compare two `--events` JSONL streams (runtime-semantics §8.7,
    issue #431's events-conformance acceptance criterion): `run_start`
    and `run_end` are compared directly after normalization; both streams
    are independently checked against the start-before-child / child-end-
    before-container ordering rule (`_split_and_check_order`); every other
    event is grouped by its own `path` and compared *as an ordered list*
    (`_group_events_by_path`) -- a path's own events have exactly one
    valid order (`start`, optionally `dispatch`, then `end`), so this
    catches a same-path reordering a multiset comparison would miss, *except*
    for a path with more than one `start` (several concurrent tree passes of
    an unnamed loop sharing one compiled body path): that path's own events
    are compared as a sorted multiset instead, after first checking, within
    each stream separately, that every `id` starts before it ends and every
    started `id` ends (`_check_instance_pairing`) -- genuine concurrent
    interleaving is allowed to differ between engines, but a same-path
    ordering bug within *one* engine's own stream still isn't; and a
    chain container's own direct children are compared for the order their
    `start` events occurred in (`_chain_sibling_order`) -- a tree
    dynamic's branches are excluded from that check and may interleave
    freely with each other on either engine, since only the nesting order
    between a container and its descendants (not sibling interleaving) is
    asserted for one of those."""
    _assert_compact_wire_format(actual_text, label="actual")
    _assert_compact_wire_format(expected_text, label="expected")
    actual = parse_event_lines(actual_text)
    expected = parse_event_lines(expected_text)
    actual_start, actual_middle, actual_end = _split_and_check_order(actual, label="actual")
    expected_start, expected_middle, expected_end = _split_and_check_order(
        expected, label="expected"
    )

    actual_start_c = _comparable_event(actual_start)
    expected_start_c = _comparable_event(expected_start)
    actual_config = actual_start_c.pop("config", _RUN_START_CONFIG_MISSING)
    expected_config = expected_start_c.pop("config", _RUN_START_CONFIG_MISSING)
    if actual_start_c != expected_start_c:
        raise AssertionError(
            f"run_start differs:\n  actual:   {actual_start_c!r}\n"
            f"  expected: {expected_start_c!r}"
        )
    _check_run_start_config(actual_config, expected_config)
    actual_end_c = _comparable_event(actual_end)
    expected_end_c = _comparable_event(expected_end)
    if actual_end_c != expected_end_c:
        raise AssertionError(
            f"run_end differs:\n  actual:   {actual_end_c!r}\n  expected: {expected_end_c!r}"
        )

    actual_raw_groups = _raw_group_events_by_path(actual_middle)
    expected_raw_groups = _raw_group_events_by_path(expected_middle)
    for label, raw_groups in (("actual", actual_raw_groups), ("expected", expected_raw_groups)):
        for path, raw_events in raw_groups.items():
            if sum(1 for e in raw_events if e.get("ev") == "start") > 1:
                _check_instance_pairing(raw_events, label=label, path=path)

    actual_groups = _group_events_by_path(actual_middle)
    expected_groups = _group_events_by_path(expected_middle)
    if set(actual_groups) != set(expected_groups):
        raise AssertionError(
            f"event paths differ:\n  actual:   {sorted(actual_groups)}\n"
            f"  expected: {sorted(expected_groups)}"
        )
    for path in actual_groups:
        multi_start = (
            sum(1 for e in actual_raw_groups[path] if e.get("ev") == "start") > 1
            or sum(1 for e in expected_raw_groups[path] if e.get("ev") == "start") > 1
        )
        if multi_start:
            actual_counts = Counter(json.dumps(e, sort_keys=True) for e in actual_groups[path])
            expected_counts = Counter(json.dumps(e, sort_keys=True) for e in expected_groups[path])
            if actual_counts != expected_counts:
                raise AssertionError(
                    f"{path}: concurrent events differ as a multiset:\n"
                    f"  actual:   {actual_groups[path]!r}\n"
                    f"  expected: {expected_groups[path]!r}"
                )
        elif actual_groups[path] != expected_groups[path]:
            raise AssertionError(
                f"{path}: events differ:\n  actual:   {actual_groups[path]!r}\n"
                f"  expected: {expected_groups[path]!r}"
            )

    actual_siblings = _chain_sibling_order(actual)
    expected_siblings = _chain_sibling_order(expected)
    if actual_siblings != expected_siblings:
        raise AssertionError(
            f"chain sibling start order differs:\n  actual:   {actual_siblings!r}\n"
            f"  expected: {expected_siblings!r}"
        )
