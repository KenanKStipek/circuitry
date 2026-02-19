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

## Conformance Suite

Run:

```bash
.venv/bin/pytest -q tests/adapters/test_conformance.py
```

Coverage includes:
- LiteLLM adapter contract checks (mocked `litellm` module)
- Direct provider adapters (OpenAI + Anthropic) with mocked curl transport
- Ollama adapter contract checks with mocked curl transport
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
