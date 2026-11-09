# Fixture runs for the design frames

The runs the frames in `../frames/` were drawn from, recorded with the scripted adapter from `demo.yml`,
`config.json` and `replies.yaml` (see the design document's Appendix B):

- `run2/`: the good run the single-run frames show, frozen 3.62 s after its `run_start`.
- `run/`: the failed run (`tape--failed`).
- `run3/`: another good run.
- `hist/`: three history runs (`run-h1..3`, with their configs and replies) whose per-step medians are the
  frames' "usual"; `history.json` is the builders' own summary of those medians.

Absolute paths inside the recorded state were replaced with neutral ones; nothing else was edited.
