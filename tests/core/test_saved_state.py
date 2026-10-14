"""Saved state writes each loop's final pass once (#220).

In memory ``prime.<loop>.last`` is an alias of the final completed
``iter_<N>``; saved state writes it as ``{"$ref": "iter_<N>"}`` instead of a
second full copy, and every path that reads saved state back relinks it so
``last`` is the same alias again. State saved before the reference form
existed (``last`` as a full copy) keeps loading.
"""

from __future__ import annotations

import datetime
import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest

import circuitry.cli.runtime_shim as runtime_shim
from circuitry.adapters.base import GenerateResult
from circuitry.cli.app import _write_state_json
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.saved_state import (
    compact_last_aliases,
    dumps_saved_state,
    link_last_refs,
)
from circuitry.core.store import Store
from circuitry.tui.inspector import load_state_file


@dataclass
class EchoAdapter:
    """Echoes every rendered prompt back, so a step's value IS its template."""

    name: str = "echo"
    prompts: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.prompts.append(prompt)
        return GenerateResult(text=prompt, raw={"model": model})


#: Outer loop over two items, each pass running an inner loop over two more.
NESTED_LOOPS: dict[str, Any] = {
    "effects": [
        {
            "type": "loop",
            "name": "outer",
            "each": {"in": "input.outer_items", "as": "o"},
            "body": [
                {
                    "type": "loop",
                    "name": "inner",
                    "each": {"in": "input.inner_items", "as": "i"},
                    "body": [
                        {"type": "prompt", "name": "step", "template": "PASS {{o}}-{{i}}"}
                    ],
                }
            ],
        }
    ]
}

NESTED_INPUT = {"outer_items": ["a", "b"], "inner_items": ["x", "y"]}

#: A second run that only reads the first run's loops through `last`.
READER: dict[str, Any] = {
    "effects": [
        {
            "type": "prompt",
            "name": "reader",
            "template": "read=[{{prime.outer.last.inner.last.step.value}}]",
        }
    ]
}


def _run_nested() -> dict[str, Any]:
    state: dict[str, Any] = {"input": dict(NESTED_INPUT)}
    root = compile_orchestration(orch=NESTED_LOOPS, root_name="prime")
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
        store=Store(state)
    )
    return state


def _write_orch(path: Path, orch: dict[str, Any]) -> Path:
    path.write_text(json.dumps(orch), encoding="utf-8")
    return path


def _run_reader(tmp_path: Path, **source: Any) -> tuple[EchoAdapter, Any]:
    adapter = EchoAdapter()
    result = run(
        RunRequest(
            orchestration_path=_write_orch(tmp_path / "reader.json", READER),
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(),
            adapter=adapter,
            skip_preflight=True,
            **source,
        )
    )
    assert result.ok, result.error
    return adapter, result


def test_saved_state_writes_each_loops_final_pass_once() -> None:
    state = _run_nested()
    saved = json.loads(dumps_saved_state(state))

    outer = saved["prime"]["outer"]
    assert outer["last"] == {"$ref": "iter_1"}
    assert outer["iter_1"]["inner"]["last"] == {"$ref": "iter_1"}
    assert outer["iter_0"]["inner"]["last"] == {"$ref": "iter_1"}
    # The final pass of the nested pair is written exactly once; a plain
    # json.dumps of the aliased state writes it four times (inner iter_1 and
    # inner `last`, both again inside the outer `last`).
    final_value = '"value": "PASS b-y"'
    assert json.dumps(saved).count(final_value) == 1
    assert json.dumps(state).count(final_value) == 4


def test_saving_leaves_the_in_memory_alias_untouched() -> None:
    state = _run_nested()
    dumps_saved_state(state, pretty=True)
    outer = state["prime"]["outer"]
    assert outer["last"] is outer["iter_1"]
    assert outer["iter_1"]["inner"]["last"] is outer["iter_1"]["inner"]["iter_1"]


def test_state_without_aliases_is_returned_as_is() -> None:
    state = {"input": {"items": [1, 2]}, "prime": {"greet": {"value": "hi"}}}
    assert compact_last_aliases(state) is state


def test_a_last_that_is_not_an_alias_is_saved_verbatim() -> None:
    """A step named `last` in a plain sequence is data, not a loop alias."""
    node = {"iter_0": {"value": 1}, "last": {"value": 1}}
    assert compact_last_aliases(node) is node


def test_link_last_refs_restores_the_alias() -> None:
    state = _run_nested()
    loaded = link_last_refs(json.loads(dumps_saved_state(state)))

    # Same content as the in-memory state, and `last` is an alias again.
    assert loaded == json.loads(json.dumps(state))
    outer = loaded["prime"]["outer"]
    assert outer["last"] is outer["iter_1"]
    assert outer["iter_0"]["inner"]["last"] is outer["iter_0"]["inner"]["iter_1"]


def test_link_last_refs_leaves_unresolvable_and_full_copy_forms_alone() -> None:
    dangling = {"iter_0": {"value": 1}, "last": {"$ref": "iter_7"}}
    not_iter = {"meta": {"value": 1}, "last": {"$ref": "meta"}}
    full_copy = {"iter_0": {"value": 1}, "last": {"value": 1}}
    link_last_refs(dangling)
    link_last_refs(not_iter)
    link_last_refs(full_copy)
    assert dangling["last"] == {"$ref": "iter_7"}
    assert not_iter["last"] == {"$ref": "meta"}
    assert full_copy["last"] == {"value": 1}
    assert full_copy["last"] is not full_copy["iter_0"]


def test_out_file_holds_references(tmp_path: Path) -> None:
    out = tmp_path / "out.json"
    _write_state_json(out=out, state=_run_nested(), pretty=False)
    saved = json.loads(out.read_text(encoding="utf-8"))
    assert saved["prime"]["outer"]["last"] == {"$ref": "iter_1"}


def test_a_previous_runs_state_file_resolves_last(tmp_path: Path) -> None:
    """`--state` with a saved run: `prime.<loop>.last` reads through the ref."""
    previous = tmp_path / "previous.json"
    _write_state_json(out=previous, state=_run_nested(), pretty=True)

    adapter, result = _run_reader(tmp_path, state_path=previous)

    assert adapter.prompts == ["read=[PASS b-y]"]
    outer = result.state["prime"]["outer"]
    assert outer["last"] is outer["iter_1"]


def test_a_previous_runs_state_as_initial_state_resolves_last(tmp_path: Path) -> None:
    """`--state` merged with `-e`, a TUI replay, REST: all pass initial_state."""
    saved = json.loads(dumps_saved_state(_run_nested()))
    adapter, _ = _run_reader(tmp_path, state_path=None, initial_state=saved)
    assert adapter.prompts == ["read=[PASS b-y]"]


def test_old_state_with_full_last_copies_keeps_loading(tmp_path: Path) -> None:
    previous = tmp_path / "previous.json"
    previous.write_text(json.dumps(_run_nested()), encoding="utf-8")

    adapter, _ = _run_reader(tmp_path, state_path=previous)

    assert adapter.prompts == ["read=[PASS b-y]"]


def test_tui_state_file_relinks_last(tmp_path: Path) -> None:
    """The Runs view browses a saved file exactly like the live run."""
    path = tmp_path / "out.json"
    _write_state_json(out=path, state=_run_nested(), pretty=False)

    loaded = load_state_file(path)

    assert loaded.ok
    assert loaded.state is not None
    outer = loaded.state["prime"]["outer"]
    assert outer["last"] is outer["iter_1"]


@dataclass
class _StubPersistenceBackend:
    """``load_latest_state`` hands back a ref-form snapshot, as a real
    backend would if the run that saved it wrote references (#220).

    ``save_run_snapshot`` records every call's kwargs in ``saved``, so a
    test can inspect the ``state`` a run actually hands the backend (#236).
    """

    snapshot: dict[str, Any] | None
    saved: list[dict[str, Any]] = field(default_factory=list)

    def describe(self) -> dict[str, Any]:
        return {"backend": "stub"}

    def load_latest_state(self, *, orchestration_path: str) -> dict[str, Any] | None:
        del orchestration_path
        if self.snapshot is None:
            return None
        return json.loads(json.dumps(self.snapshot))

    def save_run_snapshot(self, **kwargs: Any) -> None:
        self.saved.append(kwargs)


def test_resuming_from_a_persisted_snapshot_relinks_last(
    tmp_path: Path, monkeypatch: Any
) -> None:
    """A ref-form snapshot loaded via persistence.load_latest_state (not
    --state) is relinked the same way (runtime_shim._load_state's sibling
    path for a persisted snapshot, around line 254)."""
    snapshot = json.loads(dumps_saved_state(_run_nested()))
    backend = _StubPersistenceBackend(snapshot=snapshot)
    monkeypatch.setattr(
        runtime_shim, "build_persistence_backend", lambda runtime: backend
    )

    adapter = EchoAdapter()
    result = run(
        RunRequest(
            orchestration_path=_write_orch(tmp_path / "reader.json", READER),
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(runtime={"persistence": {"enabled": True}}),
            adapter=adapter,
            skip_preflight=True,
        )
    )

    assert result.ok, result.error
    assert adapter.prompts == ["read=[PASS b-y]"]
    outer = result.state["prime"]["outer"]
    assert outer["last"] is outer["iter_1"]


def test_persistence_save_run_snapshot_writes_ref_form_last(
    tmp_path: Path, monkeypatch: Any
) -> None:
    """The persistence backend's write side gets the same ref-form state as
    ``--out`` -- a loop's ``last`` is a sibling reference, not a full copy of
    its final pass (#236)."""
    backend = _StubPersistenceBackend(snapshot=None)
    monkeypatch.setattr(
        runtime_shim, "build_persistence_backend", lambda runtime: backend
    )

    adapter = EchoAdapter()
    result = run(
        RunRequest(
            orchestration_path=_write_orch(tmp_path / "nested.json", NESTED_LOOPS),
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={"input": dict(NESTED_INPUT)},
            config=CircuitryConfig(runtime={"persistence": {"enabled": True}}),
            adapter=adapter,
            skip_preflight=True,
        )
    )

    assert result.ok, result.error
    assert len(backend.saved) == 1
    saved_state = backend.saved[0]["state"]
    outer = saved_state["prime"]["outer"]
    assert outer["last"] == {"$ref": "iter_1"}
    assert outer["iter_1"]["inner"]["last"] == {"$ref": "iter_1"}
    # The live state the run returns is untouched -- still real aliases.
    live_outer = result.state["prime"]["outer"]
    assert live_outer["last"] is live_outer["iter_1"]


# ----------------------------------------------------------------------
# Issue #512: an unquoted YAML 1.1 date/timestamp or ``!!binary`` reaching
# state must not crash serialization the way a plain ``json.dumps`` does.
# ----------------------------------------------------------------------


def test_dumps_saved_state_stringifies_a_date() -> None:
    assert dumps_saved_state({"d": datetime.date(2024, 1, 2)}) == '{"d": "2024-01-02"}'


def test_dumps_saved_state_stringifies_a_naive_datetime_with_microseconds() -> None:
    value = datetime.datetime(2024, 1, 2, 3, 4, 5, 123456)  # noqa: DTZ001
    assert dumps_saved_state({"t": value}) == '{"t": "2024-01-02 03:04:05.123456"}'


def test_dumps_saved_state_stringifies_a_datetime_with_a_utc_offset() -> None:
    value = datetime.datetime(2024, 1, 2, 3, 4, 5, tzinfo=datetime.timezone.utc)
    assert dumps_saved_state({"t": value}) == '{"t": "2024-01-02 03:04:05+00:00"}'


def test_dumps_saved_state_stringifies_bytes_as_its_python_repr() -> None:
    assert dumps_saved_state({"b": b"hi"}) == '{"b": "b\'hi\'"}'


def test_dumps_saved_state_still_raises_for_anything_else_unencodable() -> None:
    """Only ``date``/``datetime``/``bytes`` get the fallback -- anything else
    ``json.dumps`` can't encode remains an error, same as a plain
    ``json.dumps`` with no ``default=`` at all."""
    with pytest.raises(TypeError, match="not JSON serializable"):
        dumps_saved_state({"s": {1, 2, 3}})


def test_write_state_json_no_longer_crashes_on_a_date(tmp_path: Path) -> None:
    """``cli/app.py::_write_state_json`` used to crash with ``TypeError:
    Object of type date is not JSON serializable`` and write no ``--out``
    file at all (#512) -- it now writes the stringified value."""
    out_path = tmp_path / "out.json"
    params_rendered = {"d": datetime.date(2024, 1, 2)}
    state = {"prime": {"a": {"value": None, "meta": {"params_rendered": params_rendered}}}}

    _write_state_json(out=out_path, state=state, pretty=False)

    loaded = load_state_file(out_path)
    assert loaded.ok
    assert loaded.state is not None
    assert loaded.state["prime"]["a"]["meta"]["params_rendered"] == {"d": "2024-01-02"}


def test_resume_from_a_written_out_with_a_date_param_reuses_the_completed_node(
    tmp_path: Path,
) -> None:
    """End to end (#512): a run whose only effect has a YAML date param
    completes and writes ``--out`` without crashing; ``--resume`` from that
    file succeeds, skips the already-completed node (its ``completed_at``
    is unchanged -- never re-dispatched), and the date param it reads back
    is the string ``--out`` wrote, not the original ``date``."""
    orch = tmp_path / "orch.yml"
    orch.write_text(
        """
effects:
  - type: tool
    name: a
    provider: json
    params:
      mode: stringify
      input: {d: 2024-01-02}
""".lstrip(),
        encoding="utf-8",
    )
    out_path = tmp_path / "out.json"

    first = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=out_path,
            dry_run=False,
            validate_only=False,
        )
    )
    assert first.ok, first.error
    _write_state_json(out=out_path, state=first.state, pretty=False)
    first_completed_at = first.state["prime"]["a"]["meta"]["completed_at"]

    loaded = load_state_file(out_path)
    assert loaded.ok
    assert loaded.state is not None

    second = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state=loaded.state,
            resume=True,
        )
    )
    assert second.ok, second.error
    node = second.state["prime"]["a"]
    assert node["meta"]["completed_at"] == first_completed_at
    assert node["meta"]["params_rendered"]["input"]["d"] == "2024-01-02"
