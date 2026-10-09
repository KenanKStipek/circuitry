"""`on_error: continue` must null the value and clear stale result meta from
an earlier pass, in `prompt`, `tool` and `use` (issue #260 part 2).

Before this fix, only `on_error: skip` nulled the value; `continue` left
whatever was already on the node's `value` key, so a reused node — an
unnamed loop's later pass, most commonly — showed a previous pass's output
sitting right next to the current pass's `meta.error`. Retry/decomposition
meta had the same problem: it is only ever *set* on a path that reached it,
never reset at the top of a pass, so a value like `meta.retries_used` from a
pass that needed a retry survived onto a later pass that failed outright.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from circuitry.adapters._retry import AdapterCallError, RetryInfo
from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store

#: These fixtures simulate a transient failure a retry can recover from, so
#: they raise a classified-retryable error rather than a bare ``RuntimeError``
#: (which the retry loop now treats as not worth retrying — see #263).
_RETRYABLE = RetryInfo(retryable=True)


@dataclass
class FlakyThenFailAdapter:
    """Pass 0: fails once, then succeeds (uses one retry). Pass 1: fails
    outright on its own first attempt with a non-retryable error, so it
    never spends a retry of its own — keeping this test's point (no stale
    leakage from pass 0) separate from #421's own retries_used-on-failure
    behaviour, covered elsewhere."""

    name: str = "flaky"
    calls: int = 0

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls += 1
        if self.calls == 1:
            raise AdapterCallError("transient failure, pass 0 attempt 0", retry_info=_RETRYABLE)
        if self.calls == 2:
            return GenerateResult(text="OK", raw={})
        raise AdapterCallError(
            "permanent failure, pass 1", retry_info=RetryInfo(retryable=False)
        )


def _unnamed_each_loop_orch(*, on_error: str) -> dict:
    return {
        "effects": [
            {
                "type": "loop",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {
                        "type": "prompt",
                        "name": "step",
                        "template": "{{item}}",
                        "retries": {"max_attempts": 2, "backoff_ms": 0},
                        "on_error": on_error,
                    }
                ],
            }
        ]
    }


def test_prompt_continue_nulls_value_and_clears_retries_used_from_prior_pass() -> None:
    orch = _unnamed_each_loop_orch(on_error="continue")
    root = compile_orchestration(orch=orch, root_name="prime")
    state = {"input": {"items": ["a", "b"]}}
    adapter = FlakyThenFailAdapter()

    DynamicRuntime(root, adapter=adapter, model="unit-test").execute(
        store=Store(state)
    )

    node = state["prime"]["step"]
    # Pass 0 succeeded after one retry.
    assert adapter.calls == 3
    # Pass 1 failed outright, spending none of its own retries: the node is
    # reused (unnamed loop), and must not carry pass 0's value or
    # retries_used next to pass 1's error.
    assert node["value"] is None
    assert node["meta"]["error"] is not None
    assert "retries_used" not in node["meta"]


def test_prompt_continue_records_its_own_passs_retries_used_not_the_priors() -> None:
    """#421: a reused node's own retries_used, on a pass that failed after
    using its own retry, must be that pass's own count — not stale from the
    prior pass, and not simply absent because the prior pass's value was
    cleared (#260). Pass 0 succeeds after one retry (retries_used would
    read 1 right after it); pass 1 then also retries once before
    exhausting its own budget and failing — it must show its own 1, not a
    leftover 1 from pass 0 read as though nothing had been cleared."""

    @dataclass
    class FlakyBothPassesAdapter:
        name: str = "flaky"
        calls: int = 0

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            self.calls += 1
            if self.calls == 1:
                raise AdapterCallError(
                    "transient failure, pass 0 attempt 0", retry_info=_RETRYABLE
                )
            if self.calls == 2:
                return GenerateResult(text="OK", raw={})
            # Pass 1: fails every attempt, retryable each time, exhausting
            # its own 2-attempt budget (one retry spent).
            raise AdapterCallError("permanent failure, pass 1", retry_info=_RETRYABLE)

    orch = _unnamed_each_loop_orch(on_error="continue")
    root = compile_orchestration(orch=orch, root_name="prime")
    state = {"input": {"items": ["a", "b"]}}

    DynamicRuntime(root, adapter=FlakyBothPassesAdapter(), model="unit-test").execute(
        store=Store(state)
    )

    node = state["prime"]["step"]
    assert node["value"] is None
    assert node["meta"]["error"] is not None
    assert node["meta"]["retries_used"] == 1


@dataclass
class RecordingAdapter:
    name: str = "rec"
    prompts: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.prompts.append(prompt)
        return GenerateResult(text=prompt, raw={"model": model})


def test_prompt_continue_clears_both_retry_meta_and_generation_option_meta() -> None:
    """Pass 0 needs a retry (sets ``meta.retries_used``, #260) and gets a
    truncated reply (sets ``meta.finish_reason``/``meta.warnings``, #250);
    pass 1 fails outright. Both groups of pre-dispatch resets must survive
    being merged together — pass 1's error must not sit next to any of
    pass 0's leftovers."""

    @dataclass
    class FlakyTruncatedThenFailAdapter:
        name: str = "flaky"
        calls: int = 0

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            self.calls += 1
            if self.calls == 1:
                raise AdapterCallError(
                    "transient failure, pass 0 attempt 0", retry_info=_RETRYABLE
                )
            if self.calls == 2:
                return GenerateResult(text="OK", raw={}, finish_reason="length")
            raise AdapterCallError(
                "permanent failure, pass 1", retry_info=RetryInfo(retryable=False)
            )

    orch = _unnamed_each_loop_orch(on_error="continue")
    root = compile_orchestration(orch=orch, root_name="prime")
    state = {"input": {"items": ["a", "b"]}}

    DynamicRuntime(root, adapter=FlakyTruncatedThenFailAdapter(), model="unit-test").execute(
        store=Store(state)
    )

    node = state["prime"]["step"]
    assert node["value"] is None
    assert node["meta"]["error"] is not None
    assert "retries_used" not in node["meta"]
    assert "finish_reason" not in node["meta"]
    assert "warnings" not in node["meta"]


def test_prompt_skip_and_continue_both_null_the_value() -> None:
    """The guidebook documents `skip` and `continue` as the same leaf-effect
    degradation (05-errors.md) \u2014 pin that `continue` actually does it too."""

    @dataclass
    class FailOnceAdapter:
        name: str = "fail-once"
        calls: int = 0

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            self.calls += 1
            raise RuntimeError("boom")

    for on_error in ("skip", "continue"):
        orch = {
            "effects": [
                {
                    "type": "prompt",
                    "name": "step",
                    "template": "hi",
                    "on_error": on_error,
                }
            ]
        }
        root = compile_orchestration(orch=orch, root_name="prime")
        state: dict = {"input": {}}
        DynamicRuntime(root, adapter=FailOnceAdapter(), model="unit-test").execute(
            store=Store(state)
        )
        assert state["prime"]["step"]["value"] is None
        assert state["prime"]["step"]["meta"]["error"] is not None


def test_tool_continue_nulls_value_on_a_reused_node() -> None:
    """The exact #260 repro: an unnamed each-loop reusing one tool node, the
    second pass's input fails to parse."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {
                        "type": "tool",
                        "name": "parse_item",
                        "provider": "json",
                        "on_error": "continue",
                        "params": {"input": "{{item}}"},
                    }
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    state = {"input": {"items": ["1", "oops"]}}

    DynamicRuntime(root, adapter=RecordingAdapter(), model="unit-test").execute(
        store=Store(state)
    )

    node = state["prime"]["parse_item"]
    assert node["value"] is None
    assert node["meta"]["error"] is not None


def test_tool_continue_clears_stdout_and_exit_code_from_a_prior_successful_pass() -> None:
    """Uses ``shell`` rather than ``json``: the ``json`` plugin reports
    ``stdout``/``exit_code`` as ``None`` even on success, so a test built on
    it would pass whether or not the reset actually happened. ``shell echo``
    sets real values on pass 0, so clearing them on pass 1's failure is
    actually exercised."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {
                        "type": "tool",
                        "name": "run",
                        "provider": "shell",
                        "on_error": "continue",
                        "params": {"command": "{{item.command}}", "args": ["{{item.arg}}"]},
                    }
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    # Pass 0: `echo hi` succeeds with real stdout/exit_code on the node.
    # Pass 1: `cat` of a file that doesn't exist exits nonzero and raises,
    # reusing the same node (unnamed loop).
    state = {
        "input": {
            "items": [
                {"command": "echo", "arg": "hi"},
                {"command": "cat", "arg": "/no/such/file-297"},
            ]
        }
    }

    DynamicRuntime(root, adapter=RecordingAdapter(), model="unit-test").execute(
        store=Store(state)
    )

    node = state["prime"]["run"]
    assert node["value"] is None
    assert node["meta"]["error"] is not None
    assert node["meta"]["stdout"] is None
    assert node["meta"]["exit_code"] is None


def test_use_continue_nulls_value_on_a_reused_node() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {
                        "type": "use",
                        "name": "child",
                        "on_error": "continue",
                        "inline": (
                            "effects:\n"
                            "  - type: tool\n"
                            "    name: parsed\n"
                            "    provider: json\n"
                            "    params: {input: \"{{item}}\"}\n"
                        ),
                        "outputs": {"parsed": "prime.parsed.value"},
                    }
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    state = {"input": {"items": ["1", "oops"]}}

    DynamicRuntime(root, adapter=RecordingAdapter(), model="unit-test").execute(
        store=Store(state)
    )

    node = state["prime"]["child"]
    assert node["value"] is None
    assert node["meta"]["error"] is not None
