### The scripted-replies file format

Source of truth: Circuitry's `scripted` adapter,
`src/circuitry/adapters/scripted.py`. This document specifies the file format so
electricity's own test-only `scripted` adapter (§12 of `electricity/DESIGN.md`) can read
the identical file — one case directory's fixture configures both engines.

---

## 1. Purpose

Both engines' conformance suite needs model replies that are deterministic and
identical, with no real network call. A **replies file** is a YAML or JSON document that
answers every model call an orchestration run makes — a `prompt` effect, a `tool`/`use`
effect's `expect: {mode: model}`, and an `if`/`while` effect's own `mode: model`
condition — keyed by the calling effect's own state path — never by the order calls
happen to arrive in, which a `flow: tree` loop or dynamic makes nondeterministic (its
branches dispatch in whatever order their worker threads happen to run).

## 2. File shape

A mapping from path (string) to a non-empty list of replies, consumed in order:

```yaml
prime.review:
  - text: "looks good"
    tokens_sent: 42
    tokens_received: 3
prime.shots.iter_2.describe:
  - error:
      kind: server_error
  - text: "a cat sitting on a mat"
    tokens_sent: 58
    tokens_received: 11
```

`.yaml`/`.yml` parses as YAML; `.json` parses as JSON. Every other extension is treated as
YAML (a superset of JSON for this purpose). A key present with an empty list, or any value
that is not a list, is a load-time error — an orchestration author who configured zero
replies for a path meant to remove the key, not leave an empty one that this adapter also
can't fill.

## 3. The path key

The key is the effect's **absolute dotted state path** — the same path a run's own
observability (OTel-style spans, runtime plugins' `effect_path`) already reports, and the
same path a reader would use to locate the effect's `value`/`meta` in the run's final
`--out` state: `prime.review`, or `prime.shots.iter_2.describe` for the third pass
(`iter_2`, zero-indexed) of a named `flow: tree`/`each` loop called `shots`, dispatching
its `describe` body effect.

A `use` child's or a decomposition's generated plan's effects are namespaced the same way
the run's own state nests them: a prompt named `answer` inside a `use` effect named
`first` is keyed `prime.first.answer` — the child orchestration's own implicit `prime`
root is stripped, exactly as it is for the run's own observability (OTel-style spans,
runtime plugins' `effect_path`) and the final `--out` state, so a replies-file key is
always the same string a reader would use to locate that same effect's value there.

An `if`/`while` effect's own `mode: model` decision is not a separately named
sub-effect — a *named* one (`name: review`) is keyed at its own node, `prime.review`; an
unnamed (transparent) one is keyed at the path of the container it sits in directly (its
enclosing node — the loop or conditional writes no node of its own to nest under). A
`while` loop's own passes always run one after another, so an unnamed `while`'s repeated
use of its container's path is still deterministic — each pass consumes the next reply in
list order. Two cases genuinely share one path across *concurrent* calls, and are
therefore matched in arrival order, not pass/sibling order: every body call of an unnamed
`flow: tree` **`each`** loop (its isolated per-pass stores carry no `iter_N` segment
without a loop name, and `flow: tree` dispatches every pass's body at once); and several
unnamed model-mode `if`s run as sibling branches of a `flow: tree` dynamic (each branch's
isolated store shares the dynamic's own path). Give either path identical replies across
its concurrent callers, or name the loop/conditional so each gets its own path.

Every reply queued for one path is consumed strictly in the order it appears in the
list — covering a prompt effect's retries (the same effect dispatches again, at the same
path, on the same pass) and a `tool`/`use` effect's `expect: {mode: model}` re-ask (a
failed expectation retries the whole effect, which re-asks at the same path). A path is
never matched by substring or prefix; it must match exactly.

## 4. One reply

A reply is a mapping, and is exactly one of a text reply or an error reply — never both,
and never neither. Every key not listed under the shape that applies (4.1 or 4.2) is a
load-time error, the same as a misspelled one (`token_sent` for `tokens_sent`) silently
dropping what it meant to set. A boolean is never accepted where an int is expected
(`status: true`, `tokens_sent: false`) — Python's `bool` is a subtype of `int`, so this is
checked explicitly, not left to `isinstance(x, int)` alone.

### 4.1 A text reply

```yaml
- text: "the model's answer"
  tokens_sent: 10     # optional, int >= 0
  tokens_received: 4  # optional, int >= 0
  finish_reason: stop # optional, string — the provider's own stop reason
```

`text` is required and must be a string (it is exactly what the effect's own
`prompt_type`/`schema` decode step then parses, same as a real adapter's reply). The two
token fields, when present, feed the run's own `tokens_sent`/`tokens_received` and
`*_total` accounting, so a replies file that cares about token totals in its expected
output sets them explicitly — they default to absent (`null`), same as a real adapter
reply that reports none.

### 4.2 An error reply

```yaml
- error:
    kind: rate_limited   # required
    status: 429          # optional; each kind has its own default (below)
    message: "slow down"  # optional; default is a generated message naming the path
```

`kind` is one of a fixed set, each mapping onto the same retryable/not-retryable
classification a real adapter's own failure carries, so the effect's existing retry and
fallback-chain logic treats a scripted failure identically to a real one:

| kind | retryable | default `status` | mirrors |
| --- | --- | --- | --- |
| `timeout` | yes | — (none) | a request that timed out before any response |
| `connection` | yes | — (none) | a dropped/refused connection, no response at all |
| `rate_limited` | yes | `429` | a provider's rate limit |
| `server_error` | yes | `500` | a provider's own `5xx` |
| `invalid_request` | no | `400` | a malformed request the provider rejected |
| `unauthorized` | no | `401` | a missing/bad credential |
| `not_found` | no | `404` | an unknown model/endpoint |
| `http` | per `status` | — (`status` required) | any other HTTP status; `429`/`408`/`5xx` are retryable, everything else is not |

`status` overrides the kind's default (`http` requires it, since it has none). Overriding
`rate_limited`'s or `server_error`'s status to something outside their usual range is
legal — the retryable/not-retryable classification still follows `status`, not the kind
name, for every kind except `timeout`/`connection`, which carry no status and are always
retryable (they model a failure that never got an HTTP response to classify in the first
place). An explicit `status: null` is only legal for `timeout`/`connection`; every other
kind needs one (its own default, or an override), checked at load time rather than left
to fail the call itself with no path named.

The generated default `message` (when a reply omits one) is the literal string
`"scripted adapter: <kind> at '<path>'"` — this, like every other error's message, ends
up verbatim in the run's own `meta.error`/`fallback_attempts[].error` state, which a
conformance case compares byte-for-byte (`electricity/DESIGN.md`'s normalization rules),
so an explicit `message` is pinned text; omit it only when the case doesn't assert on it.

## 5. What is not in scope here

This file only ever answers a *model* call — `prompt` effects, a `tool`/`use` effect's
`expect: {mode: model}`, and an `if`/`while` effect's own `mode: model` condition. Tool
fakes (a process-backed tool's recorded stdout/exit code, an HTTP-family tool's mock
server) are a separate mechanism (§12 of `electricity/DESIGN.md`), not part of this file.

The adapter also adds one warning to `meta.warnings` for any prompt effect that sets
`params`, `messages`, or `images` — `scripted` has no model behind it to honour them, the
same as any other adapter asked for an option it does not support
(`ignored_options_warning`). When `runtime.adapters.scripted.replies_file` is unset, it
defaults to `scripted-replies.yaml`, resolved the same way an explicit relative path is.

## 6. Unmatched calls and leftover replies

A model call whose path has no entry, or whose entry's list is already exhausted, is a
hard failure for that call, naming the path — the literal string
`"scripted adapter: no reply configured for path '<path>'"`, never the replies-file's own
path (which a harness typically generates fresh per run, so it wouldn't compare
byte-for-byte) — and the effect's own retry/fallback/`on_error` handling then applies to
that failure exactly as it would to a real adapter's. A path with
replies still unused at the end of a run is **not** itself a run failure (over-provisioning
a script is not a bug in the orchestration the script is driving) — Circuitry's own adapter
exposes the unused counts via `ScriptedAdapter.leftover_replies()` for a conformance
harness, which built the instance itself, to call after the run and fail the *case* on
anything left over.

## 7. The leftover-replies export (for a subprocess harness)

A conformance harness that runs `cof run` as a subprocess (`tests/conformance/harness.py`)
has no handle to the adapter instance it built — only the subprocess's exit code,
stdout/stderr, and `--out` state — so it cannot call `leftover_replies()` directly
after the run the way an in-process test (`tests/adapters/test_scripted.py`) can.
Both engines export leftover counts the same way:

- **Variable**: `CIRCUITRY_TEST_SCRIPTED_LEFTOVER_REPLIES_FILE`, naming a filesystem
  path. Never read when unset — an ordinary run (including every real use of this
  adapter outside a conformance harness) pays nothing for this, and nothing is
  written.
- **When it is written**: once, at process exit, regardless of whether the run
  itself succeeded or failed. Every scripted-adapter instance registers itself
  the moment it *loads* its replies file — `_ensure_loaded`, called by both
  `check()` (a preflight-only instance) and the first `generate()` dispatch,
  under the instance's own lock so two threads racing to build and dispatch
  the same fresh instance can never register it twice. A fallback chain, or
  more than one `runtime.adapters.*` block naming `scripted`, can build more
  than one instance; instances never share a queue — each loads its own,
  independent copy of every path's replies from the same file.

  At exit, every registered instance's own remaining count at each path
  (`0` for a path it has fully consumed, not omitted) is merged by taking the
  **minimum remaining count across every instance that holds that path** —
  never summed. For a path configured with `n` replies, the number actually
  used by the run is the most any *single* instance consumed from it, so the
  unused count is `n` minus that, which is exactly the lowest remaining count
  left on any one instance's own queue. Summing instead double-counts: two
  instances that each consumed a different half of a path's replies would
  wrongly report the whole path as unused, since each instance's own leftover
  count (what *it* didn't touch) gets added to the other's. Taking the
  minimum also means a preflight-only instance — whose queue is always full,
  since it never consumes anything — can never pull a path's reported count
  back up: its own vote is always at least as high as any instance that
  actually dispatched, so it never dominates the minimum, and a path nothing
  ever consumed still correctly reports its full count as leftover with no
  consuming instance in the picture at all.
- **File format**: a single JSON object, `{"<path>": <count>, ...}`, holding
  only paths with a nonzero merged count — the same mapping
  `leftover_replies()` itself returns, as plain JSON (no YAML fallback; this
  file is only ever machine-written and machine-read, never hand-authored).
  Written as `{}` when every registered instance's queues were fully
  consumed. An **absent** file means no instance ever registered at all —
  either nothing in the run ever actually loaded this replies file (a
  document whose only reference to the `scripted` adapter sits under a
  branch preflight doesn't statically see, or a replies file configured but
  never referenced by anything in the document), or the engine running it
  doesn't implement this export. Either way, a harness must not treat an
  absent file as "nothing left over": it means nothing was *consumed*, so
  every reply the case's own replies file configures is left over — a
  harness computes that total by parsing the replies file itself, with the
  identical loader this adapter uses (`total_replies_by_path` in
  `circuitry.adapters.scripted`), never by defaulting to an empty object.
- **Failure behaviour**: the write itself is best-effort — if it fails (an
  unwritable path, a vanished parent directory), that failure is never allowed to
  mask the run's own outcome; it does not raise past the process's own exit code.
  A harness that relies on this file checks for its existence/validity itself
  rather than trusting a non-zero exit code to imply it was skipped.

Circuitry's own implementation (`circuitry.adapters.scripted`) hangs this off
`atexit`, scoped to the `ScriptedAdapter` class alone (nothing else in the
process is affected) — electricity's own test-only `scripted` adapter (lane F1,
§448) implements an equivalent exit-time hook reading the identical variable
name, registering at load time, and merging by minimum remaining count the
same way, so one harness-side check (`tests/conformance/harness.py`) works
unmodified against either engine's subprocess.

## 8. Example

```yaml
prime.left:
  - text: "left reply"
    tokens_sent: 3
    tokens_received: 1
prime.right:
  - text: "right reply"
    tokens_sent: 5
    tokens_received: 2
prime.flaky:
  - error:
      kind: server_error
  - text: "recovered"
    tokens_sent: 4
    tokens_received: 6
```
