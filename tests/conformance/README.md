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
  case.json            # metadata (see below)
  orchestration.yml     # the document (the default filename; "orchestration" in case.json can point elsewhere)
  config.json            # optional — passed as --config if case.json sets "config"
  fakes/                  # optional — prepended to PATH for process-backed tool fakes
  replies.json            # optional — reserved for the scripted model adapter (#362); unused today
  expected.json            # the captured --out state (success), or {"error": "..."} (failure)
  expected.pretty.json      # only when case.json sets "also_pretty": true
  expected.out.json        # failure cases only -- the --out `cof run` still writes on a load/check failure
  expected.events.jsonl     # success cases only -- the --events stream `cof run` wrote for this case
  expected.warnings.txt    # always written (empty when there are none) -- cof run's own stderr WARNING:/ERROR: lines (issue #442)
```

`case.json`:

| key | meaning |
|---|---|
| `spec_case` | the `electricity/docs/spec/*` case number this exercises, e.g. `"C6"` |
| `description` | one or two sentences, cite the spec section |
| `engines` | `["python"]`, `["electricity"]`, or both — which runner(s) run this case |
| `expect` | `"success"` or `"failure"` |
| `cli_args` | extra argv appended after `--out <path>`, e.g. `["-e", "x=5"]` |
| `config` | filename (relative to the case dir) to pass as `--config`, or omitted |
| `also_pretty` | success cases only — also capture `--out --pretty` (used by C23) |
| `error_compare` | failure cases only — `"exact"` (Circuitry's own message, byte-for-byte) or `"location"` (a third-party library's message — CEL, YAML/JSON parse errors, JSON Schema — compared only for failing at the same place with a non-empty message, per §12) |
| `location_pattern` | failure cases with `error_compare: "location"` — a regex both messages must match, with the same captured text, e.g. `"effects\\[\\d+\\]\\.name"` |
| `replies_file` | reserved slot for the scripted model adapter's per-case reply list (#362's `scripted` adapter, not yet implemented) — always `null` today |
| `known_divergence` | a deliberate, temporary engine disagreement, documented here instead of silently skipping |
| `sort_warnings` | a case whose document dispatches a tree `dynamic` — `expected.warnings.txt` is written, and compared, as a sorted set rather than `cof run`'s own real order, since two branches' own warnings can land in either relative order between runs |

`harness.load_case` rejects an unknown `case.json` key and an `expect`/
`engines`/`error_compare` value outside the ones listed above (a typo such
as `"Exact"` or `"engine"` fails loudly instead of silently falling back
to a default). `config`, if omitted, gets the Python engine's own built-in
defaults (no `--config` passed — `cof run`'s sandboxed-HOME, no-project-
config case); the electricity runner has a different fallback for the same
"omitted" case, see "Running it" below.

A case document never calls a real model or network: cases accepted so far
use only the pure-Python `json` tool (no subprocess, no network). `config.json`
is used by a case that needs one (e.g. `config-deep-merge-adapters`); `fakes/`
and `replies.json` remain unused — those two slots exist for cases that will
need a process-backed tool fake or (once #362 lands) a scripted model reply.

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

Not implemented yet, because no case needs it today: cross-field timestamp
ordering (`started_at` <= `completed_at`), a plausibility bound on a
duration beyond `>= 0`, and §12's `{path}` rule for a profile's
schema-failure message. Add the rule in `normalize.py` when the first case
that needs it lands, rather than widening these ahead of a real case.

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
of `quality.yml`'s pytest jobs builds this crate once). All of M0-H
(issues #408, #431) is on `main`: electricity runs `tool`/`dynamic`/`if`/
`finally:` documents end to end, so a case that lists `electricity` in its
own `engines` and expects success now actually runs and is diffed against
the same `expected.json`/`expected.events.jsonl` the Python runner uses —
not skipped. A *failure* case may still legitimately skip for electricity
if its document uses content M0-H's own preview genuinely doesn't support
yet (`prompt`/`loop`/`use`/`reflector`/`yield`/...): the binary refuses
those up front with a message containing `is a preview and cannot run
orchestrations yet` (`electricity/crates/electricity-cli`), and the runner
treats that refusal as an expected skip rather than a failure. The same
refusal on a case that expects *success* is never skipped — it fails the
test, since a success case electricity wrongly refuses is a real
regression.

electricity is invoked with the case's own `fakes/` first on `PATH` and no
credential/`CIRCUITRY_*` env vars, same as the Python runner
(`harness._sandboxed_env`), and with a required positional config path that
has no equivalent to `cof run`'s "no config given" yet — a case without an
explicit `case.json` `"config"` gets `default_config.json` (`{}`) as a
placeholder baseline for the electricity runner only (see that file's
comment in `test_electricity_runner.py`).

## Adding a case

1. Pick the next unused name under `cases/`, named after the spec case it
   exercises, e.g. `cases/c9-tree-loop-four-items/`.
2. Write `orchestration.yml` and `case.json`. Keep the document the
   smallest one that exercises the property — no model/network calls
   unless the case is specifically about one (then it needs a tool fake or,
   once #362 lands, a `replies.json`).
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
