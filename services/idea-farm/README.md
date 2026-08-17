# idea-farm

Runs the `idea_generator` orchestration against the CyberDiner network on a
loop until `TARGET_IDEAS` unique orchestration ideas have accumulated.

Sharded dedupe keeps prompts bounded forever: each run focuses one domain
from a rotating list and only sees that domain's prior ideas; duplicates are
caught globally on write. State is a single `all_ideas.jsonl` under
`DATA_DIR` — restarts resume from the file.

## Harvest hygiene

Two checks run on every harvested line, both pure-python (no extra deps):

- **Focus adherence.** An idea line sharing no content word with the run's
  focus phrase is dropped, and the count is logged per run as
  `focus_rejects`. Runs used to drift into generic business-process
  templates — "world-building and fiction series" producing "Retirement
  Savings Forecast" — which also defeats sharded dedupe, since the same
  generic idea then lands under several foci. The orchestration's prompts
  demand the focus's own artifacts and actors first; this is the backstop.
  Set `FOCUS_CHECK=0` to bank everything.
- **Near-duplicate detection.** The exact normalized-title key remains as a
  fast path, backed by a global Jaccard similarity check (`DUP_THRESHOLD`,
  default `0.7`) over stemmed title tokens — the key that "Drafting and
  tracking team meeting summaries" and "Drafting and Tracking Meeting
  Summaries" now share. Comparisons go through an inverted token index, so
  a new idea is only scored against ideas sharing a content word.

## Corpus QA

`qa_corpus.py` reports total vs effective-unique ideas, per-focus adherence,
per-shape distribution and the top near-duplicate clusters:

```
python qa_corpus.py --data-dir /data            # report only, read-only
python qa_corpus.py --data-dir /data --dedupe   # + all_ideas.cleaned.jsonl
```

It never writes to `all_ideas.jsonl`; `--dedupe` writes a cleaned *copy*
beside it. Set `QA_ON_BOOT=1` to print the report at farm startup.

## Northflank setup (one-time, ~10 minutes)

1. **Service**: project `cyberdiner` → Add service → *Combined* →
   repo `KenanKStipek/circuitry`, branch `main`,
   Dockerfile `/services/idea-farm/Dockerfile`, context `/`.
   Smallest compute plan is plenty (the cooks do the work).
2. **Volume**: attach a volume (1 GB is overkill) mounted at `/data`.
3. **Environment**:
   | var | value |
   | --- | --- |
   | `CYBERDINER_EXPO_URL` | `http://expo:3000` (project-internal — no TLS, no egress) |
   | `CYBERDINER_TOKEN` | *(secret)* your `ck_…` key |
   | `TARGET_IDEAS` | `10000` |
   | `TIER` | `cheap` |
   | `IDEAS_PER_RUN` | `15` |
   | `SLEEP_BETWEEN_RUNS` | `20` |
   | `JOB_TIMEOUT_SECONDS` | `600` |
   | `DUP_THRESHOLD` | `0.7` (Jaccard similarity counting as a duplicate) |
   | `FOCUS_CHECK` | `1` (`0` disables the focus-adherence check) |
   | `QA_ON_BOOT` | `0` (`1` prints a corpus QA report at startup) |
4. Deploy. Progress lines look like:
   `[farm] run 41 (insurance, fanout): +12 new, focus_rejects=2, 613/10000 total, 94s`

No ports needed — it serves nothing. On completion it logs `DONE` and idles.

## Ops

- Progress: service logs (each run is one line), or shell in and
  `wc -l /data/all_ideas.jsonl`.
- Pause/resume the service freely — state is derived from the file.
- Throughput math: ~700 runs × (target 15/run); at 20s spacing plus run time,
  expect the farm to take a few days at `cheap`-tier fleet speed. Raise
  `IDEAS_PER_RUN`, lower `SLEEP_BETWEEN_RUNS`, or run a second replica-style
  service with a different `DATA_DIR` volume if impatient (merge later).
- Retrieve the harvest: `kubectl`-style shell → `cat /data/all_ideas.jsonl`,
  or attach the volume browser in the Northflank UI.
