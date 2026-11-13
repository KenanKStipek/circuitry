# Fixture runs for the design frames

The runs the frames in `../frames/` were drawn from, recorded with the scripted adapter from `demo.yml`,
`config.json` and `replies.yaml` (see the design document's Appendix B):

- `run2/`: the good run the single-run frames show, frozen 3.62 s after its `run_start`.
- `run/`: the failed run (`tape--failed`).
- `run3/`: another good run.
- `hist/`: three history runs (`run-h1..3`, with their configs and replies) whose per-step medians are the
  frames' "usual"; `history.json` is the builders' own summary of those medians.

Absolute paths inside the recorded state were replaced with neutral ones; nothing else was edited.

## `multi/` — six orchestrations at once (the Select and All frames)

`multi/snap6/` is the moment the `select--*` and `all--*` frames show: six orchestrations (`triage.yml` twice with two
configs, `digest.yml`, `review.yml`, `summarize.yml`, `classify.yml`) launched together by `multi/launch6.sh`, four on
cof and two on electricity, snapshotted 2.8 s after launch. `snap6/manifest.json` is what the launcher knew at launch
(document, config, engine, run directory, pid) and stands in for the run registry; `snap6/taken-at` is the snapshot
instant (Unix seconds); each `snap6/<key>/` holds that run's `events.jsonl` and `state.live.json` as of the snapshot,
plus the engine's `stdout.txt`/`stderr.txt`. The documents, their six configs and the scripted `replies-*.yaml` the
runs used sit beside them. Absolute paths inside the live states, configs and the launcher were rewritten to a neutral
`/tmp/osi-demo/...` prefix; pids, run ids, timestamps and token figures are the recorded ones.
