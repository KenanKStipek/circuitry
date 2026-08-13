# idea-farm

Runs the `idea_generator` orchestration against the CyberDiner network on a
loop until `TARGET_IDEAS` unique orchestration ideas have accumulated.

Sharded dedupe keeps prompts bounded forever: each run focuses one domain
from a rotating list of 40 and only sees that domain's prior ideas; a
normalized-title check dedupes globally on write. State is a single
`all_ideas.jsonl` under `DATA_DIR` — restarts resume from the file.

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
4. Deploy. Progress lines look like:
   `[farm] run 41 (insurance): +12 new, 613/10000 total, 94s`

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
