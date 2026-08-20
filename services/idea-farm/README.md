# idea-farm

Runs the `idea_generator` orchestration against the CyberDiner network on a
loop until `TARGET_IDEAS` unique orchestration ideas have accumulated.

Sharded dedupe keeps prompts bounded forever: each run focuses one domain
from a rotating list and only sees that domain's prior ideas; duplicates are
caught globally on write. State is a single `all_ideas.jsonl` under
`DATA_DIR` — restarts resume from the file.

## Harvest hygiene

Two checks run on every harvested line, both pure-python (no extra deps):

- **Focus adherence.** Runs used to drift into generic business-process
  templates — "world-building and fiction series" producing "Retirement
  Savings Forecast" — which also defeats sharded dedupe, since the same
  generic idea then lands under several foci. The orchestration's prompts
  demand the focus's own artifacts and actors first; this is the backstop.

  The backstop targets *that regression*, not topical purity. Downstream
  intake treats `focus` as advisory provenance and reclassifies domain
  itself, so an on-topic idea phrased in words the focus phrase happens not
  to contain ("Pantry Gap Shopper" under "cooking and meal planning") is a
  real idea, not drift — rejecting on bare token overlap costs those. So a
  line is dropped only when it is **both** off-focus **and** a match for one
  of the known cross-domain office templates in `GENERIC_TEMPLATES`
  (contract review, resume screening, expense approval, invoice chasing,
  loan underwriting, retirement forecasting, performance review, risk
  registers) that is *not* native to the run's focus. Rejections are logged
  as `[focus-reject:template:<name>]` and counted per run as
  `focus_rejects`; lines that are merely off-focus are banked and counted
  separately as `off_focus`, so adherence stays measurable without costing
  ideas.

  | `FOCUS_CHECK` | behaviour |
  | --- | --- |
  | `template` (default, also `1`) | reject foreign office templates only |
  | `strict` | reject every off-focus line — measured to drop legitimate on-focus ideas, use only when purity matters more than volume |
  | `off` (also `0`) | bank everything |

- **Near-duplicate detection.** The exact normalized-title key remains as a
  fast path, backed by a global Jaccard similarity check (`DUP_THRESHOLD`,
  default `0.7`) run over **two** windows, better score wins:
  - the **title** — everything before the em-dash, en-dash, bar, spaced
    hyphen, or a substantial colon head. This is the key that "Drafting and
    tracking team meeting summaries" and "Drafting and Tracking Meeting
    Summaries" now share.
  - the **lead window** — the first `DUP_LEAD_WORDS` (default `8`) content
    words of the full line. Corpus QA found the dominant survivor was a pair
    differing only in its trailing clause; that is harmless when the line
    carries a separator (the title key already folds those together) and
    fatal when the model wrote none, because the whole line then becomes the
    key and the differing tail drags similarity to ~0.5. Set
    `DUP_LEAD_WORDS=0` to disable.

  Comparisons go through an inverted token index over both windows, so a new
  idea is only scored against ideas sharing a content word.

## Corpus QA

`qa_corpus.py` reports total vs effective-unique ideas, per-focus adherence,
generic-template regressions, per-shape distribution **with within-shape
effective uniqueness**, the count of near-dup clusters spanning two or more
shapes (the shape hints re-skinning rather than generating), and the top
near-duplicate clusters:

```
python qa_corpus.py --data-dir /data            # report only, read-only
python qa_corpus.py --data-dir /data --dedupe   # + all_ideas.cleaned.jsonl
```

Records banked before the `shape` field existed count as `default`. It never
writes to `all_ideas.jsonl`; `--dedupe` writes a cleaned *copy* beside it.
Set `QA_ON_BOOT=1` to print the report at farm startup.

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
   | `DUP_LEAD_WORDS` | `8` (leading content words of the tail-proof key; `0` disables) |
   | `FOCUS_CHECK` | `template` (`strict` / `off`) |
   | `QA_ON_BOOT` | `0` (`1` prints a corpus QA report at startup) |
4. Deploy. Progress lines look like:
   `[farm] run 41 (insurance, fanout): +12 new, focus_rejects=2, off_focus=1, near_dups=3, 613/10000 total, 94s`

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
