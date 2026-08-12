from __future__ import annotations

import pytest

pytest.importorskip("typer")

from circuitry.cli.app import _find_last_effect_value, _parse_env_vars

# ---------------------------------------------------------------------------
# _parse_env_vars
# ---------------------------------------------------------------------------


def test_parse_env_vars_empty():
    assert _parse_env_vars(None) == {}
    assert _parse_env_vars([]) == {}


def test_parse_env_vars_single():
    result = _parse_env_vars(["name=World"])
    assert result == {"name": "World"}


def test_parse_env_vars_multiple():
    result = _parse_env_vars(["name=World", "count=3"])
    assert result == {"name": "World", "count": 3}  # "3" is valid JSON -> int


def test_parse_env_vars_json_value():
    result = _parse_env_vars(['data={"key": "val"}'])
    assert result == {"data": {"key": "val"}}


def test_parse_env_vars_json_array():
    result = _parse_env_vars(["items=[1,2,3]"])
    assert result == {"items": [1, 2, 3]}


def test_parse_env_vars_preserves_string_when_not_json():
    result = _parse_env_vars(["greeting=hello world"])
    assert result == {"greeting": "hello world"}


def test_parse_env_vars_equals_in_value():
    result = _parse_env_vars(["equation=a=b+c"])
    assert result == {"equation": "a=b+c"}


def test_parse_env_vars_rejects_missing_equals():
    import typer
    with pytest.raises(typer.BadParameter, match="Invalid -e format"):
        _parse_env_vars(["no_equals_here"])


def test_parse_env_vars_empty_value():
    result = _parse_env_vars(["key="])
    assert result == {"key": ""}


def test_parse_env_vars_boolean_json():
    result = _parse_env_vars(["flag=true"])
    assert result == {"flag": True}


# ---------------------------------------------------------------------------
# _find_last_effect_value
# ---------------------------------------------------------------------------


def test_find_last_effect_value_single():
    state = {"prime": {"greet": {"value": "hello"}}}
    assert _find_last_effect_value(state) == "hello"


def test_find_last_effect_value_multiple():
    state = {
        "prime": {
            "first": {"value": "one"},
            "second": {"value": "two"},
        }
    }
    # Should return the last one iterated (dict ordering = insertion order)
    assert _find_last_effect_value(state) == "two"


def test_find_last_effect_value_none_when_no_prime():
    assert _find_last_effect_value({}) is None
    assert _find_last_effect_value({"other": "stuff"}) is None


def test_find_last_effect_value_skips_meta():
    state = {
        "prime": {
            "greet": {"value": "hi"},
            "meta": {"value": "should_skip"},
        }
    }
    assert _find_last_effect_value(state) == "hi"


def test_find_last_effect_value_skips_non_dict_children():
    state = {
        "prime": {
            "simple_key": "not a dict",
            "greet": {"value": "found"},
        }
    }
    assert _find_last_effect_value(state) == "found"
