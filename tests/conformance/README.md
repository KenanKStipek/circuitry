# Conformance suite

Checks that the Python engine (`cof run`) and electricity produce the same
final state for the same document, config, and inputs — the parity contract
`electricity/DESIGN.md` §12 commits to. Python `cof` is the reference
implementation: a case's expected state is always *captured* by running
`cof run` itself, never hand-written.

## Layout

One directory per case under `cases/<name>/`:

```
cases/<name>/
  case.json              # metadata (see below)
  orchestration.yml       # the document (the default filename; "orchestration" in case.json can point elsewhere)
  config.json              # optional — passed as --config if case.json sets "config" (materialized
                            # fresh per run when the case also sets "mock_http_fixture", see below)
  fakes/                    # optional — prepended to PATH for process-backed tool fakes
  profiles/<name>.yml       # optional — a --profile document, named by case.json's own "profile" key
  <name>.yaml               # optional — a scripted-adapter replies file, named by config.json's own
                            # runtime.adapters.scripted.replies_file (electricity/docs/spec/scripted-replies.md)
  <name>.yaml               # optional — a mock-HTTP-server fixture, named by case.json's "mock_http_fixture"
  expected.json             # the captured --out state (success), or {"error": "..."} (failure)
  expected.pretty.json      # only when case.json sets "also_pretty": true
  expected.out.json         # failure cases only -- the --out `cof run` still writes on a load/check failure
  expected.events.jsonl     # success cases only -- the --events stream `cof run` wrote for this case
  expected.warnings.txt    # always written (empty when there are none) -- cof run's own stderr WARNING:/ERROR: lines (issue #442)
  expected.run_warnings.txt # only when case.json sets "compare_run_warnings": true -- cof run's own stderr Warning: ... lines
  expected.http_requests.json # only when case.json sets "mock_http_fixture" -- every request the mock
                            # server actually received, in arrival order
```

`case.json`:

| key | meaning |
|---|---|
| `spec_case` | the `electricity/docs/spec/*` case number this exercises, e.g. `"C6"` |
| `description` | one or two sentences, cite the spec section |
| `engines` | `["python"]`, `["electricity"]`, or both — which runner(s) run this case |
| `expect` | `"success"` or `"failure"` |
| `cli_args` | extra argv appended after `--out <path>`, e.g. `["-e", "x=5"]` -- a `__MOCK_HTTP_PORT__` substring in any element is replaced with the mock server's real port, see "Mock HTTP server" below |
| `config` | filename (relative to the case dir) to pass as `--config`, or omitted |
| `also_pretty` | success cases only — also capture `--out --pretty` (used by C23) |
| `error_compare` | failure cases only — `"exact"` (Circuitry's own message, byte-for-byte) or `"location"` (a third-party library's message — CEL, YAML/JSON parse errors, JSON Schema — compared only for failing at the same place with a non-empty message, per §12) |
| `location_pattern` | failure cases with `error_compare: "location"` — a regex both messages must match, with the same captured text, e.g. `"effects\\[\\d+\\]\\.name"` |
| `mock_http_fixture` | filename (relative to the case dir) of a mock-HTTP-server fixture; enables the mock server for this case, see "Mock HTTP server" below |
| `sort_http_requests` | bool, default `false` -- a tree-flow case whose concurrent branches may dispatch their own HTTP calls in a different wall-clock order on either engine; compares the recorded requests re-ordered by their own canonical JSON form instead of arrival order, see "Mock HTTP server" below |
| `known_divergence` | a deliberate, temporary engine disagreement — `{"location": "<dotted state path>", "electricity_value": <value>}`; see "Known divergence" below |
| `electricity_preview_ok` | bool, default `false` — a *success* case whose document uses content the electricity preview genuinely doesn't support yet (`shell`/`http`/a real model adapter/...); see "Running it" below |
| `sort_warnings` | a case whose document dispatches a tree `dynamic` — `expected.warnings.txt` is written, and compared, as a sorted set rather than `cof run`'s own real order, since two branches' own warnings can land in either relative order between runs |
| `env` | a mapping of extra environment variables set for both engines' own subprocess (applied after the credential/`CIRCUITRY_*` filter, so it can set a name the filter would otherwise drop) — for a case that needs a controlled variable neither engine's own process otherwise sees, e.g. `env_vars`'s own `list` mode |
| `profile` | a path relative to the case directory, of the form `profiles/<name>.yml` or `profiles/<name>.yaml` — exactly one of the two suffixes must exist. `cof run` gets `--profile <name>` (discovery finds it under the orchestration directory, the case directory); electricity gets `--profile <case_dir>/<profile>` (the absolute path) |
| `compare_run_warnings` | bool, default `false` — also compares `expected.run_warnings.txt` against `harness.run_warning_lines(stderr)` (`cli/app.py::_print_run_warnings`'s own `Warning: ...` lines, e.g. preflight's soft warnings — distinct from `WARNING:`/`ERROR:` `logging`-module lines, which `expected.warnings.txt` already covers unconditionally) |
| `fake_credentials` | a mapping from a `harness.CREDENTIAL_ENV_VARS` name to a synthetic value starting with `circuitry-test-fake-` (`harness.FAKE_CREDENTIAL_MARKER`) — set, for both engines' own subprocess, *after* the credential-stripping filter, so a case can give a real adapter (`openai`, `groq`, ...) a key to send without ever risking a live one; `env` still rejects any of these names |
| `third_party_text` | a list of `{"location": "<dotted state path>", "pattern": "<regex>"}` — each listed leaf must `re.fullmatch` its own pattern in *both* the actual and the expected state (`expected.out.json` included), and is then replaced by a fixed placeholder before the exact comparison; for a case whose `meta.error` carries curl's own version-dependent stderr text (a connection-refused, or a timeout with no retry) that can't be pinned byte-for-byte |
| `timeout_seconds` | overrides `harness.DEFAULT_TIMEOUT_SECONDS` (30) for both engines' own subprocess — a case whose document deliberately runs long (e.g. `c520-claude-code-timeout`'s own agent-CLI timeout) needs more than the default budget before the harness itself gives up and fails the case with a `subprocess.TimeoutExpired` |

`harness.load_case` rejects an unknown `case.json` key and an `expect`/
`engines`/`error_compare`/`known_divergence`/`electricity_preview_ok` value
outside the ones listed above (a typo such as `"Exact"` or `"engine"` fails
loudly instead of silently falling back to a default). `config`, if
omitted, gets the Python engine's own built-in defaults (no `--config`
passed — `cof run`'s sandboxed-HOME, no-project-config case); the
electricity runner has a different fallback for the same "omitted" case,
see "Running it" below. It also rejects three key combinations that parse
fine but don't work: `also_pretty` together with `mock_http_fixture` (the
`--pretty` re-run gets neither a fresh mock server nor a fresh
leftover-replies path, and runs after the server has already stopped);
`known_divergence` together with `also_pretty` (the `--pretty` comparison
never applies the override); and `electricity_preview_ok: true` on
anything but a *success* case that lists `electricity` in `engines` (it
names a case electricity is expected to refuse, which is meaningless
otherwise).

A case document calls no real model or network — a case that needs one
uses the `scripted` adapter (a replies file) or the harness's own mock
HTTP server, never a live credential or endpoint. `config.json` is used by
a case that needs one (e.g. `config-deep-merge-adapters`, or any case
configuring an adapter's `replies_file`/`base_url`); `fakes/` is used by a
case that needs a process-backed tool fake (e.g. `c-shell-path-fake`).

## How a case runs

`tests/conformance/harness.py` is the one place that knows how to invoke
`cof run` for a case: `python -m circuitry.cli.app run <orchestration> --out
<path> --quiet [--config ...] [cli_args...]`, with a temporary `HOME`,
every credential env var and `CIRCUITRY_*` variable removed, the case's
own `fakes/` first on `PATH` if present, and `cwd` set to the case
directory. Both `scripts/generate-conformance-cases.py` and
`test_python_runner.py` call it, so generation and verification can never
silently diverge on *how* a case is run.

`test_electricity_runner.py` parses electricity's own stdout the same way
`harness.parse_cli_error` parses `cof run`'s, per the non-TTY CLI output
contract both engines commit to byte for byte (electricity/DESIGN.md §6.9,
"CLI output"): a run failure's error is in the `{"ok": false, ...}` stdout
payload, never on stderr; only a config error (never a case this suite
exercises, since every case's `config.json`, if any, is valid) prints a
bare `Error: <text>` line on stderr instead.

Both runners also compare `harness.warning_lines(result.stderr)` against
the committed `expected.warnings.txt` (issue #442's "Warning lines on
stderr" item) — Circuitry's own `logging`-module `WARNING:`/`ERROR:` lines
(`core/dynamic.py`/`core/conditional.py`'s on_error/`finally:` degradation
warnings, `cli/config.py`'s "Unknown environment" warning, and
`cli/live_state.py`/`cli/events.py`'s mid-run write-failure warnings), not
the unrelated `Warning:`/`Error:` lines above. A chain's own warnings keep
`cof run`'s real order; `sort_warnings` compares them as a sorted set
instead, for a tree `dynamic` case where real order can vary run to run.

## Scripted model replies

A case selects the `scripted` adapter (#362) the same way any other
document selects an adapter — a document-level `adapter: scripted` (and
`model: <anything>`, since the adapter ignores it) — and points
`config.json`'s `runtime.adapters.scripted.replies_file` at a replies file
in the case directory (`electricity/docs/spec/scripted-replies.md` has the
full file format). Both engines run with the case directory as `cwd`
("How a case runs" above), so a relative `replies_file` resolves the same
way either engine's own config-merge logic reads it — no harness-specific
handling needed; `case.json` has no separate `replies_file` key (an earlier
draft reserved one before #362 existed; `config.json` alone is enough, and
keeping both would be two ways to say the same thing). See
`c-scripted-prompt` for a minimal example.

A replies file that leaves an entry unused fails the case on either
engine, even though it isn't itself a run failure (over-provisioning a
script is not a bug in the orchestration it drives — spec §6): both
`test_python_runner.py` and `test_electricity_runner.py` set the test-only
`CIRCUITRY_TEST_SCRIPTED_LEFTOVER_REPLIES_FILE` environment variable
(`harness.run_case`'s `leftover_replies_path`) for every run, and assert the
exported counts are empty. The export mechanism itself — variable name,
file format, when it is written, failure behaviour — is specified in
`electricity/docs/spec/scripted-replies.md` §7, so electricity's own
`scripted` adapter (lane F1, #448) implements the identical one and this
same harness check works unmodified against either engine's subprocess.
`tests/conformance/test_harness.py::test_unused_scripted_reply_fails_the_case`
is the harness-level proof.

## Mock HTTP server

`tests/conformance/mock_http.py`'s `MockHttpServer` fakes any HTTP-family
tool (`http`) or model adapter (an OpenAI-compatible family member's
`base_url`) without a real network call. A case enables it by setting
`case.json`'s `mock_http_fixture` to a YAML (or JSON) fixture filename in
the case directory — a list of `request`/`response` entries, loaded
through Circuitry's own loader, consumed strictly in list order (never
matched by lookup):

```yaml
- request:
    method: GET          # or POST/PUT/DELETE/PATCH
    path: /data
  response:
    status: 200
    reason: OK            # optional, default ""
    headers:              # optional -- never Date/Server; the server never adds either itself
      Content-Type: application/json
    body: '{"greeting": "hi"}'
```

`harness.mock_http_server(case_dir, metadata)` is a context manager both
runners and the generator use identically: it starts the server (owned
entirely by the harness, on `127.0.0.1` with an ephemeral port, serving
from one background thread) when the case sets `mock_http_fixture`, and
always stops it in a `finally`, even on failure. The chosen port has to
reach both an adapter's `base_url` (`config.json`) and a document's own
input (`cli_args`' `-e`, for an `http` tool URL): the harness materializes
both with the real port right before each run, substituting it for the
literal placeholder `__MOCK_HTTP_PORT__` wherever that string appears
(`harness.materialize_config`/`materialize_cli_args`) — `config.json` and
`cli_args` as committed always carry the placeholder, never a real port
number. Every request the server actually receives (method, path, query,
headers apart from `User-Agent`, body) is recorded, in arrival order, and
committed as `expected.http_requests.json` so both engines' own runs can be
diffed against it through `harness.assert_recorded_http_requests`, the one
place both pytest runners make this comparison, run unconditionally in
both runners against both engines. See `c-http-mock-server` (the `http`
tool) and `c-openai-compatible-mock-server` (the `lmstudio` adapter, no
credential needed) for complete examples.

Header comparison follows one rule for both engines: a header *name* is
compared case-insensitively and the whole header *set* order-insensitively
(`normalize.canonicalize_http_headers` sorts every header to
`[lowercased name, value]`, so two requests whose headers differ only in
name case or arrival order still compare equal) — but a header *value* is
always compared exactly. `User-Agent` is excluded before either engine's
request even reaches a fixture (`mock_http.MockHttpServer`'s own record
step), never compared at all. Requests themselves are compared in arrival
order, since that order is itself part of what a chain-flow case checks —
unless `case.json` sets `"sort_http_requests": true`, for a tree-flow case
whose concurrent branches may dispatch their own HTTP calls in a different
wall-clock order on either engine (cof's real OS threads vs. electricity's
single-threaded cooperative scheduler); that re-orders the whole list by
its own canonical JSON representation before comparing, on both sides,
so neither engine's own dispatch order is asserted.

A client that reaches the mock server has to send the exact headers the
committed fixture recorded, including ones it adds on its own rather than
ones the document configured — `expected.http_requests.json` pins
whatever the *reference* client actually sent, not just the request a
tool/adapter's own config describes. Python's `urllib` (the Python engine's
own HTTP client) sends `Accept-Encoding: identity` and `Connection: close`
on every request, neither of which any case's own fixture or document sets
explicitly; electricity's own client (`reqwest`, lane E, issue #485) sends
the same headers the committed fixture already has, not merely the ones
the orchestration document itself names.

## Fake binaries

`fakes/fake` fakes a process-backed tool (`shell`, `awk`, `imagemagick`,
`ffmpeg`) with no real binary installed, parameterized entirely through
the `shell` tool's own `params.args`. Each case keeps its own copy of the
file (the suite's `fakes/` contract is per-case, not a shared library —
`harness._sandboxed_env` only ever puts one case's own directory on
`PATH`), but it isn't one script "copied verbatim" everywhere: most cases
carry the original seven-flag version (issue #450, item 4, plus `--cat`/
`--print-env` for a `stdin`/`runtime.plugins.<name>.env` check);
`c-shell-background-survives-normal-exit` carries a second, extended copy
(issue #519) that adds the pidfile flags below for a backgrounded-process
case.

| flag | behaviour |
|---|---|
| `--stdout TEXT` | write `TEXT` (plus a trailing newline if it has none) |
| `--stderr TEXT` | write `TEXT` to stderr the same way |
| `--exit N` | exit with code `N` (default `0`) |
| `--sleep` | ignore `SIGTERM` and sleep until killed (`SIGKILL`) — for a subprocess-timeout/cancellation case |
| `--crlf` | ignore every other flag; write two CRLF-terminated stdout lines |
| `--invalid-utf8` | ignore every other flag; write a fixed, deliberately invalid UTF-8 byte sequence to stdout |
| `--cat` | ignore every other flag; copy stdin to stdout verbatim — for a case that checks `params.stdin` actually reaches the child |
| `--print-env NAME` | ignore every other flag; write the named environment variable's own value (or `"<unset>"`) — for a case that checks a configured `runtime.plugins.<name>.env` override actually reaches the child |
| `--spawn-background --pidfile PATH` | (the extended copy only) start a detached grandchild that just sleeps, in this process's own process group, and write its pid to PATH — the `nohup ... &` pattern a real shell step uses to start a background server |
| `--check-pidfile PATH` | (the extended copy only) print `"alive"`/`"gone"` for the pid PATH names |
| `--kill-pidfile PATH` | (the extended copy only) `SIGKILL` the pid PATH names, remove PATH, print `"done"` |

`c-shell-path-fake` uses `--stdout`/`--exit`; `c-shell-crlf` and
`c-shell-invalid-utf8` use `--crlf`/`--invalid-utf8`; `c-shell-timeout`
uses `--sleep`; `c-shell-background-survives-normal-exit` uses the
pidfile flags. `tests/conformance/test_fakes.py` covers every flag
directly too, including `--cat`/`--print-env`.

## Known divergence

A `known_divergence` case documents a deliberate, pinned difference
between the two engines — both succeed, but electricity is documented to
produce a different value at one stated location (e.g. a regex or clock
edge case, #400/#401) — rather than silently skipping or marking the case
`known_divergence` ad hoc. `case.json` sets:

```json
"known_divergence": {"location": "prime.pattern.value", "electricity_value": "..."}
```

`location` is a dotted path from the state root (a segment that parses as
a non-negative integer indexes a list; otherwise it's a dict key) into the
Python-engine-captured `expected.json` — the same path convention
`electricity/docs/spec/scripted-replies.md` §3 uses for a replies-file key.
`harness.load_case` requires `expect: "success"` and both engines listed
(there is nothing to diverge from with only one running), the same way it
validates every other key. `test_electricity_runner.py` applies
`normalize.apply_known_divergence` to the expected state before comparing
— `electricity_value` at `location`, and only there, is allowed to differ;
everywhere else must still match exactly. No case sets it today: every
committed case's two engines either both succeed with an identical value,
or (`electricity_preview_ok`) one hasn't implemented the content at all
yet — `test_normalize.py` covers the override function directly, and
`test_harness.py`'s `known_divergence` tests cover the schema.

## Normalization

`tests/conformance/normalize.py` implements §12's rules once, shared by
both runners: timestamps (`created_at`/`completed_at`/`started_at`, and the
root `_timestamp` in `%Y%m%d_%H%M%S`), run ids/UUIDs, and durations
(`wall_time_s`/`eta_s`/`elapsed_s`) are checked for shape and replaced with
a fixed placeholder before comparing — never compared for their actual
value, matched by key name regardless of where in the tree they appear.

Two more fields are normalized for the same underlying reason
(test-environment, not document-content, dependence), but matched by their
**exact dotted location** from the state root instead of by key name —
`effective_settings.out` and `effective_settings.runtime._orchestration_dir`
are always absolute paths into wherever this suite happens to be checked
out and whatever temp directory generated the run, which differ between a
contributor's machine and CI; they're replaced with the stable placeholders
`<out>` and `<case>` respectively, both at comparison time and in the
committed `expected.json` itself (`scripts/generate-conformance-cases.py`
redacts them before writing — see "Regenerating" below). A name match alone
would also catch `effective_settings.sources.out` (a provenance tag like
`"cli"`, not a path) at the wrong location, so that field is deliberately
exact-location, not key-name. `runtime.last_run.orchestration_path`
(compared by basename) is already a relative, portable value by the time a
case runs it (cwd is always the case directory) and needs no redaction on
write — only the shape check at comparison time.

A third category, `redact_runtime_strings`, is matched by neither key name
nor exact location: a literal substring — the case directory's own
absolute path, its `fakes/` subdirectory, a mock HTTP server's ephemeral
port — replaced wherever it appears inside *any* string leaf, because it
can land at a genuinely unpredictable key (a subprocess timeout naming the
resolved binary it ran, e.g. `c-shell-path-fake`'s own `raw.binary`; a
configured `base_url` `effective_settings.runtime` echoes back verbatim,
e.g. `c-openai-compatible-mock-server`'s `lmstudio.base_url`). Both
runners build the replacement list with `harness.case_redaction_replacements`
(longest/most-specific path first, so `fakes/` is replaced before its own
parent case directory) and apply it with `normalize.normalize_for_comparison`
— `redact_runtime_strings` then `normalize()` — everywhere either runner
previously called `normalize()` alone, including a failure case's own error
text. The generator redacts the same way, with the same replacement list,
before writing any committed file (`_redact_and_reserialize`).

Not implemented yet, because no case needs it today: cross-field timestamp
ordering (`started_at` <= `completed_at`) and a plausibility bound on a
duration beyond `>= 0`. §12's `{path}` rule for a profile's schema-failure
message (`profile-schema-invalid`) needs no special handling either, but
for a different reason: `cof run --profile <name>`'s own discovered path
and electricity's own `--profile <absolute path>` argument both resolve
to the case directory, so `case_redaction_replacements`' ordinary
`<CASE_DIR>` substitution already lines the two up with no path-specific
rule at all -- `error_compare: "location"` only needs the two messages to
fail at the same place, so the small wording difference either engine's
own `jsonschema` library leaves around that path goes unchecked anyway.
Add the two timestamp/duration rules above to `normalize.py` when the
first case that needs either lands, rather than widening these ahead of a
real case.

Circuitry's own error messages (everything the reference implementation
itself raises) are compared byte-for-byte, always — `error_compare: "exact"`
in `case.json`. A third-party library's message (CEL, YAML/JSON parse
errors, JSON Schema) is compared only for "failed at the same place, with a
non-empty message" — `error_compare: "location"`, with a `location_pattern`
regex both messages must match identically.

Dict key *order* is part of what's compared, not just key set — `--out`'s
insertion order vs. `--out --pretty`'s alphabetical order is itself the
property C23 exists to check (`assert_out_serialization` round-trips a
file's own content through the exact `json.dumps` call that should have
produced it and asserts byte equality).

`cof run` writes `--out` on any failure too, not only on success
(run-wiring step 20, issue #431) — a load/check failure before any effect
ever dispatches gets a minimal seeded state plus whatever of
`runtime`/`effective_settings` had already resolved by then; a run failure
(a tool exhausting its retries, a `dynamic`'s `stop_on_error`, a failed
`finally:`) gets the full state as it stood when the run gave up. Either
way it varies by which step failed. Every failure case therefore also
commits `expected.out.json`, captured and redacted the same way a success
case's `expected.json` is, and both runners compare their own `--out`
against it (under `normalize()`, like any other state) in addition to the
error text. `also_pretty` is success-only — `harness.load_case` rejects it
on a failure case.

## Events and live state

Every success case also commits `expected.events.jsonl`, `cof run --events`'s
own output for that case (runtime-semantics.md §8.7). Both runners re-run
the case with `--events`/`--live-state` set to a path under `tmp_path` and
compare: electricity's own stream against the committed fixture; the Python
runner's fresh stream against the same fixture, the same determinism check
`expected.json` already gets. `normalize.assert_events_equal` does the
comparison: `ts`/`ms`/`run_id`/`pid`/`engine` are shape-checked and replaced
with a placeholder, like any other volatile state field; the per-instance
`seq`/`id` are dropped outright rather than normalized, because a tree
dynamic's branches are free to start and finish in a different wall-clock
order on the two engines (cof's real OS threads vs. electricity's
single-threaded cooperative scheduler) — comparing their absolute values
would assert an implementation detail neither engine commits to. What *is*
checked: both streams open with `run_start` and close with `run_end`
(compared directly); every path's own `start`/`dispatch`/`end` appear in a
container-before-child, child-before-container order; each path's own
list of events (grouped by `path`) is compared *in stream order*, not as a
multiset — a path's own events have exactly one valid order in M0-H
(`start`, then a tree container's own `dispatch`, then `end`), so an
end-before-start or a dispatch-after-end on the same path is caught, which
a `collections.Counter` comparison alone couldn't tell from the correct
order; and a chain container's (one with no `dispatch` event of its own)
direct children are compared for the relative order their own `start`
events occurred in across the two streams — a chain's children run
strictly one after another in document order on both engines, unlike a
tree dynamic's branches, which may freely interleave with each other on
either engine and are excluded from that last check by the `dispatch`
event check alone.

`--live-state`'s final write is asserted byte-identical to `--out` for
every success case, in both engines (electricity/DESIGN.md §10.5) — no
separate fixture needed, since it's compared against the same run's own
`--out` rather than a committed file.

## Running it

```sh
pytest tests/conformance/                      # both runners
pytest tests/conformance/test_python_runner.py  # python engine only
pytest tests/conformance/test_electricity_runner.py  # electricity engine only
```

The electricity runner (`test_electricity_runner.py`) builds the
`electricity` binary from `electricity/` with `cargo` (once per test
session) and skips every case it applies to — with a reason naming why —
only when `cargo` isn't on `PATH` (a contributor machine that genuinely has
no Rust toolchain; the ordinary `pytest -q -m 'not integration'` gate
itself *does* have one — CI's `ubuntu-latest` image ships `cargo`, so each
of `quality.yml`'s pytest jobs builds this crate once). electricity runs
a real document end to end (`electricity/README.md`'s preview section is
the single source of truth for what it runs), so a case that lists
`electricity` in its own `engines` and expects success now actually runs
and is diffed against the same `expected.json`/`expected.events.jsonl`
the Python runner uses — not skipped. A *failure* case may still
legitimately skip for electricity if
its document uses content this preview genuinely doesn't support yet: the
binary refuses those up front with a message containing the preview marker
(`is a preview and can't run this orchestration`,
`electricity_bytecode::PREVIEW_MARKER`), and the runner treats that refusal as an expected skip
rather than a failure. The same refusal on a case that expects *success*
is never skipped — it fails the test, since a success case electricity
wrongly refuses is a real regression.

electricity is invoked with the case's own `fakes/` first on `PATH` and no
credential/`CIRCUITRY_*` env vars, same as the Python runner
(`harness._sandboxed_env`), and with a required positional config path that
has no equivalent to `cof run`'s "no config given" yet — a case without an
explicit `case.json` `"config"` gets `default_config.json` (`{}`) as a
placeholder baseline for the electricity runner only (see that file's
comment in `test_electricity_runner.py`).

`case.json`'s `electricity_preview_ok: true` is the one declared exception
to "a success case electricity wrongly refuses is a real regression": a
sample case (#450) whose document uses content the electricity preview
genuinely doesn't support yet (`shell`/`http`/a real model adapter/...)
still passes on the Python engine, and is deliberately still refused by
electricity until the lane that implements that capability flips it — at
which point the flag is simply removed, and the case's own success
assertion starts applying to electricity too, with no other change. Never
use `known_divergence` for this: that key means *both* engines succeeded,
with one documented, stated difference — not applicable when one engine
never ran the document at all. No case sets it today: `c-shell-path-fake`
ran on both engines once `shell` landed, `c-scripted-prompt` once lane G
(issue #498) landed the `prompt` effect through the `scripted` adapter,
and `c-http-mock-server`/`c-openai-compatible-mock-server` once lane E's
`http` tool and lane F2's (issue #503) real model adapters landed. The
flag stays declared and validated for the next lane that needs it.

## M1 acceptance map

Every DESIGN.md §15 M1 acceptance item maps to one or more case directories
under `cases/`, or to a named Rust test outside this suite. `spec_case` in
a case's own `case.json` names the same item (set to the plain label
below, e.g. `"C19"`, `"§7 case 1"`, even where an existing case's own
name predates the item it turned out to cover).

| DESIGN §15 item | Case(s) / test |
|---|---|
| C1 | `c1-interface-inputs`, `c1-interface-inputs-required-missing`; `e-coercion-null`, `e-coercion-string-null`, `e-coercion-string-numeric`; `profile-inputs-interface-types` |
| C4 | `c4-prompt-template-semantics` |
| C5 | `c5-params-json-vs-template-list` |
| C7 (`while` half) | `c7-while-heterogeneous-equality` |
| C8 | `c8-each-chain-prev-and-collect`, `profile-disabled-loop-body-collect` |
| C9–C11, C13, C14, C16–C18, C24, C28–C33 | the `cNN-*` case of the same number |
| C12 | (a) `c-shell-path-fake`; (b) `c-shell-templated-allowed-commands`; (c) `c12-allowed-commands-from`; (d) `c12-allowed-commands-params-json` |
| C15 | `c15-expect-cel-missing-key` |
| C19 | `tree-stop-on-error`. The running-branch variant (a tree `stop_on_error` that must let an already-dispatched, still-running branch finish rather than cancelling it) was not added: on this machine the two branches' relative timing never reliably favors the slower one actually starting before the fast one fails, so the case cannot be made deterministic without changing what it exercises (making the "fast" branch artificially slow defeats the point). Left out rather than committed flaky. |
| C20 | `concurrency-group`, `prompt-tree-group`; `c20-group-in-tree-loop` |
| C25 | `electricity-cli/tests/signals.rs`, `signals_loop.rs` and `process_tool_signals.rs` (lane M part B adds the prompt re-run and `signals_prompt.rs`) |
| C26 | `complexity-scoring-off`, `complexity-scoring-on-routing-off`; `c26-scoring-absent`, `c26-scoring-enabled` |
| §7 case 1 | `c-shell-pin-denial` |
| §7 case 2 | `c-shell-templated-allowed-commands` |
| §7 case 3 | `c23-out-plain-vs-pretty`; `c-json-stringify-indent` |
| §7 case 4 | `fs-delete-branches` |
| §7 case 5 | `c-ffmpeg-filter-complex-allows-pipe-and-semicolon`, `c-ffmpeg-flags-unsafe-pipe-and-semicolon` |
| §7 case 6 | `c-http-error-excerpt-shapes` |
| §7 case 7 (the `http` half; the `mcp` half is M2) | `c-http-fail-on-error` |
| §7 case 9 | `uuid-v5-and-errors` (v5, bad version, bad namespace); `c-uuid-v1-shape` (v1 shape only -- monotonic ordering stays a Rust unit test, `electricity-tools/src/uuid.rs`) |
| §7 case 12 | `env-vars-get-and-list` |
| §7 case 14 | `anthropic-messages-mock`, `openai-compatible-roles-images-params-mock` |
| §7 case 15 | `openai-compatible-429-retry-after-mock`, `openai-compatible-401-mock`, `adapter-connection-refused-retried`, `c-http-retry-classification` |
| §7 case 17 | `use-host-claude-document-adapter` |
| §7 case 22 (the redaction blanket) | `c-http-redaction-blanket`, plus the post-check both runners now make after *every* case (`harness.assert_no_leaked_credentials`, `tests/conformance/test_no_leaked_credentials.py`'s static twin, `test_harness.py`'s own planted-credential test) |

Out of M1's own acceptance, tracked here only because an earlier pass
over DESIGN §15 already placed them: §7 case 8 (`regex-shapes`) and §7
case 16 (the `cyberdiner-*` cases) are filed under M2 there; C21/C22 stay
in M2-G with persistence; §7 cases 7's `mcp` half, 10, 11, 18, 19 are M2;
§7 cases 20, 21 are M3.

## Adding a case

1. Pick the next unused name under `cases/`, named after the spec case it
   exercises, e.g. `cases/c9-tree-loop-four-items/`.
2. Write `orchestration.yml` and `case.json`. Keep the document the
   smallest one that exercises the property — no model/network calls
   unless the case is specifically about one (then it needs a tool fake, a
   `scripted`-adapter replies file, or the mock HTTP server — see those
   sections above).
3. Generate the expected state: `python scripts/generate-conformance-cases.py
   <case-name>` (omit the name to regenerate every case).
4. Run `pytest tests/conformance/test_python_runner.py -k <case-name>` and
   read the captured `expected.json`/`expected.pretty.json` before
   committing it — it's the ground truth from here on, so it's worth
   reading once.

## Regenerating

A case's expected state goes stale when the document changes or when the
reference implementation's own behavior for that document legitimately
changes. Regenerate with `python scripts/generate-conformance-cases.py`
(or name one case to limit it), review the diff, and commit it alongside
the change that caused it. `python scripts/generate-conformance-cases.py
--check` is part of the verification gate (`CLAUDE.md`) and fails — naming
every stale file — when a committed expected state no longer matches a
fresh run, compared under the same normalizer the test runners use (a
fresh run's own run id/timestamps never match the committed file's byte
for byte; `--check` compares the *normalized* value, not raw bytes).
Regeneration only rewrites a case's file when its *normalized* content
actually changed, so running the generator over every case doesn't turn
into a 15-file diff of nothing but fresh run ids.

Before writing anything, the generator redacts the two absolute-path
fields (`<out>`, `<case>` — see "Normalization" above) out of the run it
just captured, so the committed file never contains the machine's own
checkout path or OS temp directory — `test_no_leaked_paths.py` guards
against a regression here, failing if any committed file under
`tests/conformance/cases/` matches an absolute-path pattern
(`/Users/`, `/home/`, `/var/folders/`, `/private/`, `/tmp/`, `.pi/`, a
Windows drive path, or the current username embedded in one of those
path forms).
