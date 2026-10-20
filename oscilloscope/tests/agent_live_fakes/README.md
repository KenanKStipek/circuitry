# Reference fake CLIs for the agent tool's live progress (issue #637)

`fake_pi.py` and `fake_claude.py` stand in for `pi --mode json` and
`claude --output-format stream-json`, streaming one session with real
`time.sleep` calls so a mid-run poll genuinely catches `meta.live` and
the transcript partway through. Marker strings (`MARKER_*`) in their
output exist so a privacy test can assert none of them ever reach
`meta.live`, `meta.raw` or `--events` -- `value` (the agent's own
final reply text, by the agent tool's own documented contract) is
the one place `MARKER_REPLY` is expected to land.

Point `runtime.plugins.agent.pi.binary` / `claude_code.binary` at
either file to use it; see `scripts/record_fixtures.py`'s
`AGENT_LIVE_CASES`, which recorded `tests/fixtures/agent_live_pi/` and
`tests/fixtures/agent_live_claude_code/` from them.

These files mirror the stream shape Circuitry's own
`tests/agent_cli_test_support.py` and `tests/fixtures/agent_live/*.jsonl`
exercise against the agent tool itself, kept here unedited so osi's
own recorded fixtures exercise the same shape end to end.
