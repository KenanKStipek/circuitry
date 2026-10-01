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
- `finish_reason`: `str | None` — the provider's own stop reason (`stop`,
  `length`, `end_turn`, `max_tokens`, ...) when it reports one. The runtime
  records it as `meta.finish_reason` and warns on `length`/`max_tokens`.
- `warnings`: `tuple[str, ...]`, default empty — what the adapter could not
  honour, recorded on `meta.warnings`.

### Generation options

`generate()` may also take `options: GenerateOptions | None = None`
(`circuitry.adapters.base`). The runtime calls every adapter through
`call_generate`, which passes `options` only to a `generate()` that has an
`options` (or `**kwargs`) parameter. Older adapters keep working unchanged,
and their result carries one warning naming what they never received.

`GenerateOptions` carries:
- `temperature`, `max_tokens`, `stop` — portable knobs; map them to the
  provider's names (ollama `num_predict`, anthropic `stop_sequences`, ...).
- `params` — the rest of the effect's `params:`, for the provider as written.
- `deterministic` — add a fixed seed (`DETERMINISTIC_SEED`) where the provider
  takes one and `params` names none. Temperature 0 is already set.
- `messages` — role-tagged `ChatMessage` turns; when present, send them
  instead of the flattened `prompt`, with `system` in the provider's system slot.
- `images` — `ImageInput`s (bytes plus `media_type`, or a `url`) for the last
  user turn.

An adapter that accepts `options` but cannot use some of them returns
`ignored_options_warning(self.name, options, used=...)` as its `warnings`. An
adapter that can send images declares `accepts_images: ClassVar[bool] = True`;
`cof check` and run preflight warn when an effect's image assets go to an
adapter without it.

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

### `check()` and preflight's hard/soft classification

`check()` reports one adapter instance's own readiness; whether a failing
`check()` blocks a run is decided one layer up, by
`circuitry.cli.runtime_shim.preflight` walking the orchestration that
references the adapter. By default every `adapter:`/`provider:` an
orchestration declares is a **hard** dependency — a failing `check()` fails
`cof check`/`cof run` for the whole file. An orchestration author opts an
adapter into being a **soft** dependency by setting `on_error: skip` (or
`continue`) on every `prompt` effect that uses it: preflight then downgrades
a failing `check()` to a warning naming the effects that will skip, instead
of hard-failing the run. This is orchestration-file authoring policy, not
something an adapter implementation controls — see the `prompt` effect's
`on_error` field in `docs/orchestration-reference.md` for the full rule.

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
