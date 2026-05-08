# Plan 07: Harden Test Suite -- Deeper Mocking and Coverage

## Problem

Tests rely on shallow mocks that bypass real failure modes. Adapters mock `subprocess.run` globally. Plugin tests mock `build_plugin`. CLI tests patch `run()`. No coverage threshold in CI.

## Phase 1: Adapter Hardening (Highest Priority)

1. **Missing API key tests** -- `OpenAIAdapter.generate()` and `AnthropicAdapter.generate()` raise `RuntimeError` when env var is unset
2. **Malformed response tests** -- empty `choices`, missing `message.content`, missing `usage`, empty `content` array
3. **Non-JSON response test** -- returncode 0 with HTML body -> `RuntimeError`
4. **API key masking test** -- fake key does not appear in exception message
5. **curl-not-installed tests** -- `FileNotFoundError` -> actionable `RuntimeError`
6. **LiteLLM import error test** -- actionable message when package missing
7. **Ollama `list_models()` test** -- success and failure paths

## Phase 2: Factory and Plugin Coverage

8. **`build_adapter()` unit tests** in `tests/adapters/test_factory.py`:
   - Unknown name raises `ValueError`
   - Each supported name returns correct type
   - Config passthrough

9. **`build_plugin()` unit tests** in `tests/plugins/test_factory.py`:
   - Unknown name raises `ValueError`
   - Config passthrough to ComfyUI

10. **ComfyUI plugin tests** in `tests/plugins/test_comfyui.py`:
    - Missing params, curl failures, polling timeout, no image output, successful execution, image upload

## Phase 3: Coverage Infrastructure

11. **Add `pytest-cov>=5.0`** to `requirements-dev.txt`

12. **Add to `pyproject.toml`**:
    ```toml
    [tool.pytest.ini_options]
    addopts = "--cov=circuitry --cov-report=term-missing --cov-fail-under=70"
    ```

13. **Update `quality.yml`**:
    ```yaml
    - name: Run tests with coverage
      run: pytest -q --cov=circuitry --cov-report=term-missing --cov-fail-under=70
    ```

14. **Ratcheting**: Increase threshold by 5% after each phase. Target: 70% -> 75% -> 80%.

## Phase 4: End-to-End Error Propagation

15. **`run()` adapter-error propagation test** -- non-dry-run with failing adapter
16. **REST trigger edge cases** -- non-JSON body, GET on POST-only endpoint

## Files to Change

| File | Change |
|------|--------|
| `tests/adapters/test_conformance.py` | Add malformed response, auth, non-JSON tests |
| `tests/adapters/test_factory.py` | **Create** -- adapter factory tests |
| `tests/plugins/test_factory.py` | **Create** -- plugin factory tests |
| `tests/plugins/test_comfyui.py` | **Create** -- ComfyUI plugin tests |
| `requirements-dev.txt` | Add `pytest-cov>=5.0` |
| `pyproject.toml` | Add pytest/coverage config |
| `.github/workflows/quality.yml` | Add coverage flags |
