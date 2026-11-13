"""Unit tests for the ``surrealdb`` SDK 2.x/3.x compatibility shim."""

from __future__ import annotations

import sys
import types
from typing import Any

import pytest

from circuitry.surrealdb_sdk import (
    is_record_range,
    record_target,
    run,
    run_delete_range,
    run_query,
    run_query_raw,
    run_select,
    run_select_range,
    run_write,
)


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


def test_run_select_leaves_a_list_alone() -> None:
    assert run_select([{"name": "ada"}, {"name": "grace"}]) == [
        {"name": "ada"},
        {"name": "grace"},
    ]


def test_run_select_wraps_a_bare_record_in_a_list() -> None:
    builder = _Builder(result={"name": "ada"})
    assert run_select(builder) == [{"name": "ada"}]


def test_run_select_turns_none_into_an_empty_list() -> None:
    builder = _Builder(result=None)
    assert run_select(builder) == []


def test_run_write_leaves_a_record_or_list_alone() -> None:
    assert run_write({"name": "ada"}) == {"name": "ada"}
    assert run_write([{"name": "ada"}]) == [{"name": "ada"}]
    assert run_write([]) == []


def test_run_write_turns_none_into_an_empty_list() -> None:
    builder = _Builder(result=None)
    assert run_write(builder) == []


def test_run_query_raw_returns_the_first_statements_result() -> None:
    response = {
        "result": [
            {"status": "OK", "result": [{"name": "ada"}]},
            {"status": "OK", "result": [{"name": "grace"}]},
        ]
    }
    assert run_query_raw(response) == [{"name": "ada"}]


def test_run_query_raw_returns_none_for_no_statements() -> None:
    assert run_query_raw({"result": []}) is None


def test_run_query_raw_raises_on_the_first_failing_statement() -> None:
    response = {
        "result": [
            {"status": "OK", "result": [{"name": "ada"}]},
            {"status": "ERR", "result": "Found NONE for field score"},
        ]
    }
    with pytest.raises(RuntimeError, match="SurrealQL error: Found NONE"):
        run_query_raw(response, mode="query")


def test_run_query_raw_raises_on_an_early_failing_statement_even_with_a_later_ok() -> None:
    response = {
        "result": [
            {"status": "ERR", "result": "Parse error"},
            {"status": "OK", "result": [{"name": "ada"}]},
        ]
    }
    with pytest.raises(RuntimeError, match="SurrealQL error: Parse error"):
        run_query_raw(response, mode="query")


def test_run_query_raw_rejects_an_unexpected_response_shape() -> None:
    with pytest.raises(RuntimeError, match="unexpected response shape"):
        run_query_raw({"no_result_key": True})


def test_is_record_range() -> None:
    assert is_record_range("person:1..10") is True
    assert is_record_range("person:1..=10") is True
    assert is_record_range("person:1") is False
    assert is_record_range("person") is False


class _FakeQueryRawDb:
    def __init__(self, response: Any) -> None:
        self._response = response
        self.queries: list[str] = []

    def query_raw(self, query: str) -> Any:
        self.queries.append(query)
        return self._response


def test_run_select_range_sends_a_select_statement_and_always_returns_a_list() -> None:
    db = _FakeQueryRawDb({"result": [{"status": "OK", "result": [{"name": "ada"}]}]})
    assert run_select_range(db, "person:1..10") == [{"name": "ada"}]
    assert db.queries == ["SELECT * FROM person:1..10"]


def test_run_select_range_turns_none_into_an_empty_list() -> None:
    db = _FakeQueryRawDb({"result": [{"status": "OK", "result": None}]})
    assert run_select_range(db, "person:1..10") == []


def test_run_delete_range_sends_a_delete_statement_and_turns_none_into_an_empty_list() -> None:
    db = _FakeQueryRawDb({"result": [{"status": "OK", "result": None}]})
    assert run_delete_range(db, "person:1..10") == []
    assert db.queries == ["DELETE person:1..10 RETURN BEFORE"]


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
