from __future__ import annotations

import json
from pathlib import Path

from circuitry.cli.live_state import make_live_state_callback, write_live_state


def test_write_live_state_creates_valid_json(tmp_path: Path):
    target = tmp_path / "state.json"
    state = {"prime": {"greet": {"value": "hello"}}}
    write_live_state(target, state)

    assert target.exists()
    parsed = json.loads(target.read_text(encoding="utf-8"))
    assert parsed == state


def test_write_live_state_creates_parent_dirs(tmp_path: Path):
    target = tmp_path / "deep" / "nested" / "state.json"
    write_live_state(target, {"ok": True})

    assert target.exists()
    assert json.loads(target.read_text(encoding="utf-8")) == {"ok": True}


def test_write_live_state_no_tmp_file_remains(tmp_path: Path):
    target = tmp_path / "state.json"
    write_live_state(target, {"a": 1})

    tmp_file = target.with_suffix(".tmp")
    assert not tmp_file.exists(), ".tmp file should not remain after atomic rename"


def test_write_live_state_overwrites_existing(tmp_path: Path):
    target = tmp_path / "state.json"
    write_live_state(target, {"version": 1})
    write_live_state(target, {"version": 2})

    parsed = json.loads(target.read_text(encoding="utf-8"))
    assert parsed["version"] == 2


def test_make_live_state_callback_returns_callable(tmp_path: Path):
    target = tmp_path / "cb.json"
    cb = make_live_state_callback(target)
    assert callable(cb)


def test_make_live_state_callback_writes_on_call(tmp_path: Path):
    target = tmp_path / "cb.json"
    cb = make_live_state_callback(target)

    state = {"runtime": {"run_id": "abc"}}
    cb(state)

    assert target.exists()
    parsed = json.loads(target.read_text(encoding="utf-8"))
    assert parsed == state


def test_callback_updates_file_on_each_call(tmp_path: Path):
    target = tmp_path / "cb.json"
    cb = make_live_state_callback(target)

    cb({"step": 1})
    assert json.loads(target.read_text(encoding="utf-8"))["step"] == 1

    cb({"step": 2})
    assert json.loads(target.read_text(encoding="utf-8"))["step"] == 2

    cb({"step": 3})
    assert json.loads(target.read_text(encoding="utf-8"))["step"] == 3
