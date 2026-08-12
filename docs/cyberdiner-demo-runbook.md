# CyberDiner Demo Runbook

End-to-end script for driving a Circuitry orchestration across a live
CyberDiner network: **submit → a cook serves the job → the completion lands
in orchestration state**.

Every command below is copy-pasteable. Nothing here runs in CI — the live
integration tests are marked `integration` and CI runs
`pytest -m 'not integration'`, so the offline guarantee is untouched.

> **Pre-stability API.** The adapter talks to expo's `/beta` job routes
> (`POST /beta/jobs`, `GET /beta/jobs/{job_id}`). Those routes are
> pre-stability and may change shape without a CyberDiner major bump.

---

## 0. What you need

| Requirement | Check |
| --- | --- |
| A reachable **expo** deployment (the job broker) | `curl -sS -o /dev/null -w '%{http_code}\n' "$CYBERDINER_EXPO_URL/beta/jobs"` — any HTTP status means the host answered |
| At least one **cook** serving the tier you ask for | otherwise jobs sit in `pending` until the adapter's timeout fires |
| A CyberDiner **API key** (`ck_…`), minted via expo's `api_keys` routes or the web app | |
| Circuitry installed | `pip install -e ".[tools]"` from a checkout, or `pip install circuitry-cof` |

Tiers are the adapter's model names: `tier-1`, `tier-2`, `tier-3`, `tier-4`.
Anything else is rejected before a request leaves the process.

---

## 1. Environment

```sh
export CYBERDINER_EXPO_URL=https://expo.example.com   # expo root URL, no trailing path
export CYBERDINER_TOKEN=ck_...                        # never commit this
export CYBERDINER_TIER=tier-1
```

These names are also what the live integration tests read (step 5).

---

## 2. Config file

The adapter is configured under `runtime.adapters.cyberdiner`. Generate the
config from the environment so the token only ever exists in your shell and in
a local, git-ignored file — **never in orchestration YAML**, which is meant to
be shared and committed:

```sh
cat > circuitry.config.json <<EOF
{
  "default_adapter": "cyberdiner",
  "default_model": "${CYBERDINER_TIER:-tier-1}",
  "enabled_adapters": ["cyberdiner"],
  "enabled_tools": [],
  "enabled_plugins": [],
  "runtime": {
    "adapters": {
      "cyberdiner": {
        "expo_url": "${CYBERDINER_EXPO_URL}",
        "token": "${CYBERDINER_TOKEN}",
        "default_tier": "${CYBERDINER_TIER:-tier-1}",
        "poll_interval_ms": 500,
        "timeout_seconds": 180
      }
    }
  }
}
EOF
chmod 600 circuitry.config.json
```

Notes:

- `timeout_seconds` bounds the whole submit-and-poll sequence for one prompt.
  Raise it when the cook fleet is cold; a job that never gets picked up fails
  with `timed out … waiting for job <id> to complete (last status='pending')`.
- `enabled_tools: []` / `enabled_plugins: []` lock the demo down to the adapter
  alone, which also keeps `cof doctor` focused (see the next step). Drop them if
  your orchestration uses tool effects.
- The token is redacted (`***REDACTED***`) everywhere state is serialized —
  `runtime.effective_settings`, `--out`, `--json`, `--live-state`, and
  `~/.config/circuitry/last-run.json`. Live adapter calls still use the real
  value.

---

## 3. Preflight — `cof doctor`

```sh
cof doctor -c circuitry.config.json
```

Expected: effective adapter/model resolved from config, and a green
`cyberdiner` row. The adapter's `check()` counts *any* HTTP response from
`{expo_url}/beta/jobs` as reachable — only connection-level failures are
reported as missing.

```
                               Circuitry · Doctor
┏━━━━━━━━━━━━━━━━━━━┳━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓
┃ Check             ┃ Result                               ┃
┡━━━━━━━━━━━━━━━━━━━╇━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┩
│ Config path       │ circuitry.config.json                │
│ Effective adapter │ cyberdiner (source: config)          │
│ Effective model   │ tier-1 (source: config)              │
└───────────────────┴──────────────────────────────────────┘
          Adapters (allowlisted)
┏━━━━━━━━━━━━┳━━━━━━━━┳━━━━━━━━━━━━━━━━━━━┓
┃ Name       ┃ Status ┃ Missing / message ┃
┡━━━━━━━━━━━━╇━━━━━━━━╇━━━━━━━━━━━━━━━━━━━┩
│ cyberdiner │ ok     │ —                 │
└────────────┴────────┴───────────────────┘
```

`doctor` exits non-zero if any checked extension is red. A red
`['host:https://expo.example.com']` means expo is unreachable — fix that before
running anything else. The unrelated "NOT FOUND" rows in the backend-detection
table (ollama, comfyui, ffmpeg …) are informational and do not affect the exit
code.

Optional smoke of a single completion, before any orchestration is involved:

```sh
cof doctor -c circuitry.config.json --generate
```

---

## 4. The run

`adapter` is chosen by the orchestration's `adapter:` key, or — as configured
above — by `default_adapter`. Any bundled prompt orchestration therefore runs
on CyberDiner unchanged; that portability is the point of the demo:

```sh
cof run learn/hello -c circuitry.config.json -e name=CyberDiner --print --pretty
```

Once the bundled CyberDiner example lands (issue #6) it is the more explicit
demo target, since it pins `adapter: cyberdiner` and `model: tier-1` in YAML:

```sh
cof run learn/cyberdiner_hello -c circuitry.config.json --print --pretty
```

### Where the completion shows up

`--print --pretty` dumps the final state. The interesting paths:

```jsonc
{
  "prime": {
    "greet": {
      "value": "Greetings, CyberDiner!",        // ← the completion
      "meta": {
        "adapter": "cyberdiner",
        "model": "tier-1",
        "prompt_sent": "Say hello to CyberDiner in a creative way.",
        "completed_at": "2026-08-12T21:44:08.815609+00:00",
        "error": null
      }
    },
    "value": true                                // ← orchestration completed
  },
  "runtime": {
    "effective_settings": {
      "adapter": "cyberdiner",
      "runtime": {
        "adapters": {
          "cyberdiner": {
            "expo_url": "https://expo.example.com",
            "token": "***REDACTED***"            // ← redacted, as promised
          }
        }
      }
    }
  }
}
```

Other ways to watch the same run:

- `--tail` — stream effects as they finish, instead of one dump at the end.
- `--out state.json` — write the final state to a file.
- `--live-state live.json` — atomic incremental writes after every effect, for
  external watchers.
- `cof run --last` — re-run the previous invocation; the previous run's state is
  kept at `~/.config/circuitry/last-run.json`.

From Python, the same run is `circuitry.run_orchestration(...)`, and
`result.state` holds exactly the structure above.

---

## 5. Live integration tests

`tests/integration/test_cyberdiner_live.py` is the automated version of steps
3–4: one test drives `CyberdinerAdapter.generate()` directly, one runs a full
orchestration through `run_orchestration` and asserts `ok=True` plus a redacted
token in `runtime.effective_settings`.

```sh
# With the env from step 1 exported:
pytest tests/integration/test_cyberdiner_live.py -q
```

Without `CYBERDINER_EXPO_URL` and `CYBERDINER_TOKEN` both set, the tests skip
with a reason instead of failing — so this command is safe on any machine, and
the CI-facing `pytest -q -m 'not integration'` never selects them at all.

Optional knobs: `CYBERDINER_TIER` (default `tier-1`) and
`CYBERDINER_TIMEOUT_SECONDS` (default 180).

---

## 6. Troubleshooting

| Symptom | Cause / fix |
| --- | --- |
| `doctor` shows `['host:…']` for cyberdiner | expo unreachable — check the URL (root URL, no `/beta` suffix), VPN, TLS |
| `cyberdiner: HTTP 401 …` | bad or revoked API key; mint a fresh `ck_…` |
| `cyberdiner: timed out … (last status='pending')` | no cook is serving that tier, or the fleet is cold — start a cook, try another tier, or raise `timeout_seconds` |
| `cyberdiner: job <id> failed: …` | the cook reported failure; the message is expo's `error_message` verbatim |
| `unknown tier 'gpt-4o'` | tiers are the model names — use `tier-1` … `tier-4` |
| `Preflight failed: …` | fix the reported item, or bypass with `cof run --skip-preflight` |
| Tests skipped when you expected them to run | one of `CYBERDINER_EXPO_URL` / `CYBERDINER_TOKEN` is unset or empty in *that* shell |

---

## Related

- [Adapter Conformance](./adapter-conformance.md) — the `Adapter` contract the
  cyberdiner adapter implements
- [Threat Model](./threat-model.md) — credential handling and redaction policy
- `src/circuitry/adapters/cyberdiner.py` — submit/poll client, tier mapping,
  preflight `check()`
