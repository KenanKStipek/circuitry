"""Unit tests for the ``surrealdb`` SDK 2.x/3.x compatibility shim."""

from __future__ import annotations

import sys
import types
from typing import Any

import pytest

from circuitry.surrealdb_sdk import record_target, run, run_query


class _FakeStringRecordId:
    def __init__(self, value: str) -> None:
        self.value = value

    def __eq__(self, other: object) -> bool:
        return isinstance(other, _FakeStringRecordId) and other.value == self.value

    def __hash__(self) -> int:
        return hash(self.value)


@pytest.fixture
def no_string_record_id(monkeypatch: pytest.MonkeyPatch) -> None:
    """A fake ``surrealdb`` module shaped like SDK 2.x: no ``StringRecordId``."""
    monkeypatch.setitem(sys.modules, "surrealdb", types.ModuleType("surrealdb"))


@pytest.fixture
def fake_string_record_id(monkeypatch: pytest.MonkeyPatch) -> type[_FakeStringRecordId]:
    """A fake ``surrealdb`` module shaped like SDK 3.x: exports ``StringRecordId``."""
    module = types.ModuleType("surrealdb")
    module.StringRecordId = _FakeStringRecordId  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "surrealdb", module)
    return _FakeStringRecordId


class _Builder:
    def __init__(self, result: Any = None, error: BaseException | None = None) -> None:
        self._result = result
        self._error = error
        self.executed = False

    def execute(self) -> Any:
        self.executed = True
        if self._error is not None:
            raise self._error
        return self._result


def test_run_executes_a_builder() -> None:
    builder = _Builder(result=[{"name": "ada"}])
    assert run(builder) == [{"name": "ada"}]
    assert builder.executed is True


def test_run_passes_through_a_plain_result() -> None:
    assert run([{"name": "ada"}]) == [{"name": "ada"}]
    assert run({"name": "ada"}) == {"name": "ada"}
    assert run(None) is None


def test_run_propagates_a_builder_execute_error() -> None:
    builder = _Builder(error=RuntimeError("boom"))
    with pytest.raises(RuntimeError, match="boom"):
        run(builder)


def test_run_query_unwraps_the_single_statement_result() -> None:
    builder = _Builder(result=[[{"name": "ada"}]])
    assert run_query(builder) == [{"name": "ada"}]
    assert builder.executed is True


def test_run_query_returns_none_for_no_statements() -> None:
    builder = _Builder(result=[])
    assert run_query(builder) is None


def test_run_query_passes_through_a_plain_result() -> None:
    assert run_query([{"name": "ada"}]) == [{"name": "ada"}]


def test_run_query_propagates_a_statement_error() -> None:
    builder = _Builder(error=RuntimeError("Parse error: boom"))
    with pytest.raises(RuntimeError, match="Parse error"):
        run_query(builder)


def test_record_target_wraps_a_table_colon_id_string(
    fake_string_record_id: type[_FakeStringRecordId],
) -> None:
    assert record_target("person:1") == fake_string_record_id("person:1")


def test_record_target_leaves_a_bare_table_name_alone(
    fake_string_record_id: type[_FakeStringRecordId],
) -> None:
    assert record_target("person") == "person"


def test_record_target_leaves_a_range_alone(
    fake_string_record_id: type[_FakeStringRecordId],
) -> None:
    assert record_target("person:1..10") == "person:1..10"


def test_record_target_passes_through_without_string_record_id(
    no_string_record_id: None,
) -> None:
    assert record_target("person:1") == "person:1"
