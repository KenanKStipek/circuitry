from __future__ import annotations

import pytest

from circuitry.core.store._identifiers import (
    quote_sqlite_identifier,
    validate_table_name,
)


class TestValidateTableName:
    def test_accepts_simple_name(self) -> None:
        assert validate_table_name("circuitry_runs") == "circuitry_runs"

    def test_accepts_underscore_prefix(self) -> None:
        assert validate_table_name("_private") == "_private"

    def test_accepts_max_length(self) -> None:
        name = "a" * 63
        assert validate_table_name(name) == name

    def test_rejects_over_length(self) -> None:
        with pytest.raises(ValueError):
            validate_table_name("a" * 64)

    def test_rejects_hyphen(self) -> None:
        with pytest.raises(ValueError):
            validate_table_name("bad-name")

    def test_rejects_leading_digit(self) -> None:
        with pytest.raises(ValueError):
            validate_table_name("1table")

    def test_rejects_empty(self) -> None:
        with pytest.raises(ValueError):
            validate_table_name("")

    def test_rejects_semicolon(self) -> None:
        with pytest.raises(ValueError):
            validate_table_name("runs; DROP TABLE users --")

    def test_rejects_space(self) -> None:
        with pytest.raises(ValueError):
            validate_table_name("my table")

    def test_rejects_dot(self) -> None:
        with pytest.raises(ValueError):
            validate_table_name("schema.table")


class TestQuoteSqliteIdentifier:
    def test_clean_name(self) -> None:
        assert quote_sqlite_identifier("runs") == '"runs"'

    def test_embedded_double_quote(self) -> None:
        assert quote_sqlite_identifier('my"table') == '"my""table"'

    def test_already_safe_name(self) -> None:
        assert quote_sqlite_identifier("circuitry_runs") == '"circuitry_runs"'
