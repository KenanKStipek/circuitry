# Adapter Conformance

This document defines the normalized adapter contract and how to validate new providers.

## Contract

All adapters must implement:
- `generate(model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult`

`GenerateResult` requirements:
- `text`: `str`
- `raw`: `dict`
- `tokens_sent`: `int | None` and non-negative when present
- `tokens_received`: `int | None` and non-negative when present

Error behavior requirements:
- Transport/provider failures must raise `RuntimeError` with actionable context.
- Error messages should include enough detail to diagnose likely configuration/provider issues.

## Optional hooks

Two hooks are optional. Adapters written before they existed keep working,
so neither belongs to the `Adapter` Protocol and neither is called
directly — always go through its shim.

| hook | shim | absent means |
| --- | --- | --- |
| `check() -> CheckResult` | `circuitry.preflight.call_check` | ready (`ok=True`) |
| `list_models() -> list[str]` | `circuitry.adapters.models.call_list_models` | "I don't know" (`[]`) |

### `list_models()`

Names the models a user can pick, filling the TUI run launcher's model
dropdown and `cof list --models <adapter>`. It is a convenience, so it
must degrade rather than fail: `call_list_models` swallows exceptions,
rejects non-list returns, strips and de-duplicates entries, and caps the
result at `MAX_MODELS`. Implement it where the answer is cheap and
local-network only:

- `ollama` — `GET {base_url}/api/tags` over stdlib `urllib` with a short
  timeout; an unreachable daemon returns `[]`.
- `cyberdiner` — the configured `valid_tiers`, else the seed tier names
  (expo's `tier_service` is the authority).
- `anthropic` — current model aliases, static; no API key, no round trip.

An enumeration is never authoritative: the model picker keeps a
`custom…` free-text option and `cof run --model <anything>` still takes
any string.

Callers that have a config rather than an instance use
`circuitry.adapters.models.list_adapter_models(adapter_name=..., runtime=...)`,
which builds the adapter and applies the same forgiving contract.

## Conformance Suite

Run:

```bash
.venv/bin/pytest -q tests/adapters/test_conformance.py
```

Coverage includes:
- LiteLLM adapter contract checks (mocked `litellm` module)
- Direct provider adapters (OpenAI + Anthropic) with mocked curl transport
- Ollama adapter contract checks with mocked curl transport
- Optional `list_models()` hook and its shim (`tests/adapters/test_model_listing.py`)
- Contract mismatch diagnostics from the conformance validator
- Error-path diagnostics for provider transport failures

## Onboarding New Adapters

1. Add adapter implementation under `src/circuitry/adapters/`.
2. Wire adapter in `src/circuitry/adapters/factory.py`.
3. Add conformance test cases in `tests/adapters/test_conformance.py` using mocks/stubs only.
4. Ensure diagnostics are actionable for both contract mismatch and runtime/provider failures.
5. Run quality gates:
   - `.venv/bin/pytest -q`
   - `.venv/bin/ruff check .`
   - `.venv/bin/mypy src`
