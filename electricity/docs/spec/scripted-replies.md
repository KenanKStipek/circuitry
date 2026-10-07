### The scripted-replies file format

Source of truth: Circuitry's `scripted` adapter,
`src/circuitry/adapters/scripted.py`. This document specifies the file format so
electricity's own test-only `scripted` adapter (§12 of `electricity/DESIGN.md`) can read
the identical file — one case directory's fixture configures both engines.

---

## 1. Purpose

Both engines' conformance suite needs model replies that are deterministic and
identical, with no real network call. A **replies file** is a YAML or JSON document that
answers every `prompt`/`expect: {mode: model}` call an orchestration run makes, keyed by
the calling effect's own state path — never by the order calls happen to arrive in, which
a `flow: tree` loop or dynamic makes nondeterministic (its branches dispatch in whatever
order their worker threads happen to run).

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
the run's own state nests them — including the child orchestration's own implicit `prime`
root, so a prompt named `answer` inside a `use` effect named `first` is keyed
`prime.first.prime.answer`, not `prime.first.answer`.

Every reply queued for one path is consumed strictly in the order it appears in the
list — covering a prompt effect's retries (the same effect dispatches again, at the same
path, on the same pass) and a `tool`/`use` effect's `expect: {mode: model}` re-ask (a
failed expectation retries the whole effect, which re-asks at the same path). A path is
never matched by substring or prefix; it must match exactly.

## 4. One reply

A reply is a mapping, and is exactly one of:

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
place).

## 5. What is not in scope here

This file only ever answers a *model* call — `prompt` effects and a `tool`/`use` effect's
`expect: {mode: model}`. Tool fakes (a process-backed tool's recorded stdout/exit code, an
HTTP-family tool's mock server) are a separate mechanism (§12 of `electricity/DESIGN.md`),
not part of this file.

## 6. Unmatched calls and leftover replies

A model call whose path has no entry, or whose entry's list is already exhausted, is a
hard failure for that call, naming the path — the effect's own retry/fallback/`on_error`
handling then applies to that failure exactly as it would to a real adapter's. A path with
replies still unused at the end of a run is **not** itself a run failure (over-provisioning
a script is not a bug in the orchestration the script is driving) — Circuitry's own adapter
exposes the unused counts via `ScriptedAdapter.leftover_replies()` for a conformance
harness, which built the instance itself, to call after the run and fail the *case* on
anything left over.

## 7. Example

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
