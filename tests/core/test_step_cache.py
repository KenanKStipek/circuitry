"""`core.step_cache` — the storage/TTL/key primitives `cache:` on a
prompt/tool effect builds on (#270). Effect-level hit/miss/failure-never-
cached behavior lives in `tests/core/test_step_cache_effects.py`.
"""

from __future__ import annotations

import stat
from pathlib import Path

import pytest

from circuitry.core.step_cache import (
    CacheDef,
    StepCache,
    cache_dir,
    compute_cache_key,
    parse_ttl,
)

# ---------------------------------------------------------------------------
# parse_ttl
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "raw,expected_seconds",
    [
        ("45s", 45),
        ("30m", 30 * 60),
        ("12h", 12 * 3600),
        ("7d", 7 * 86400),
        ("1D", 86400),
        (90, 90),
        (90.5, 90.5),
    ],
)
def test_parse_ttl_accepts_durations_and_numbers(raw: object, expected_seconds: float) -> None:
    assert parse_ttl(raw, label="x") == expected_seconds


@pytest.mark.parametrize("raw", [True, False, -1, 0, "0d", "7x", "seven days", "", None, [7]])
def test_parse_ttl_rejects_everything_else(raw: object) -> None:
    with pytest.raises(ValueError, match="x"):
        parse_ttl(raw, label="x")


# ---------------------------------------------------------------------------
# compute_cache_key
# ---------------------------------------------------------------------------


def test_compute_cache_key_is_stable_for_the_same_material() -> None:
    material = {"a": 1, "b": [1, 2, 3]}
    key1 = compute_cache_key(effect_type="prompt", material=material, salt=None)
    key2 = compute_cache_key(effect_type="prompt", material=dict(material), salt=None)
    assert key1 == key2


def test_compute_cache_key_ignores_dict_key_order() -> None:
    key1 = compute_cache_key(effect_type="prompt", material={"a": 1, "b": 2}, salt=None)
    key2 = compute_cache_key(effect_type="prompt", material={"b": 2, "a": 1}, salt=None)
    assert key1 == key2


def test_compute_cache_key_changes_with_material() -> None:
    key1 = compute_cache_key(effect_type="prompt", material={"a": 1}, salt=None)
    key2 = compute_cache_key(effect_type="prompt", material={"a": 2}, salt=None)
    assert key1 != key2


def test_compute_cache_key_changes_with_effect_type() -> None:
    key1 = compute_cache_key(effect_type="prompt", material={"a": 1}, salt=None)
    key2 = compute_cache_key(effect_type="tool", material={"a": 1}, salt=None)
    assert key1 != key2


def test_compute_cache_key_changes_with_salt() -> None:
    key1 = compute_cache_key(effect_type="prompt", material={"a": 1}, salt=None)
    key2 = compute_cache_key(effect_type="prompt", material={"a": 1}, salt="v2")
    key3 = compute_cache_key(effect_type="prompt", material={"a": 1}, salt="v3")
    assert len({key1, key2, key3}) == 3


# ---------------------------------------------------------------------------
# cache_dir
# ---------------------------------------------------------------------------


def test_cache_dir_respects_env_override(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    override = tmp_path / "somewhere"
    monkeypatch.setenv("CIRCUITRY_CACHE_DIR", str(override))
    assert cache_dir() == override
    assert override.is_dir()


def test_cache_dir_created_at_0700(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    override = tmp_path / "steps"
    monkeypatch.setenv("CIRCUITRY_CACHE_DIR", str(override))
    path = cache_dir()
    assert stat.S_IMODE(path.stat().st_mode) == 0o700


# ---------------------------------------------------------------------------
# StepCache
# ---------------------------------------------------------------------------


def test_step_cache_round_trip(tmp_path: Path) -> None:
    cache = StepCache(root=tmp_path)
    cache.put("k1", {"nested": [1, 2]}, created_at="2026-01-01T00:00:00+00:00")
    lookup = cache.get("k1", ttl_seconds=None)
    assert lookup.hit is True
    assert lookup.value == {"nested": [1, 2]}
    assert lookup.created_at == "2026-01-01T00:00:00+00:00"


def test_step_cache_miss_when_absent(tmp_path: Path) -> None:
    cache = StepCache(root=tmp_path)
    lookup = cache.get("missing", ttl_seconds=None)
    assert lookup.hit is False
    assert lookup.value is None


def test_step_cache_miss_on_corrupt_file(tmp_path: Path) -> None:
    cache = StepCache(root=tmp_path)
    (tmp_path / "bad.json").write_text("{not json", encoding="utf-8")
    lookup = cache.get("bad", ttl_seconds=None)
    assert lookup.hit is False


def test_step_cache_file_written_atomically_at_0600(tmp_path: Path) -> None:
    cache = StepCache(root=tmp_path)
    cache.put("k1", "value", created_at="2026-01-01T00:00:00+00:00")
    entry = tmp_path / "k1.json"
    assert entry.exists()
    assert stat.S_IMODE(entry.stat().st_mode) == 0o600
    # No leftover temp files from the atomic write.
    assert list(tmp_path.glob(".tmp-*")) == []


def test_step_cache_ttl_expiry(tmp_path: Path) -> None:
    from datetime import datetime, timedelta, timezone

    cache = StepCache(root=tmp_path)
    stale = (datetime.now(timezone.utc) - timedelta(seconds=120)).isoformat()
    cache.put("k1", "value", created_at=stale)

    assert cache.get("k1", ttl_seconds=600).hit is True
    assert cache.get("k1", ttl_seconds=60).hit is False
    # No TTL at all: always valid regardless of age.
    assert cache.get("k1", ttl_seconds=None).hit is True


# Real entry names are a sha256 hex digest (what `compute_cache_key` always
# returns) — `clear`/`stats` only ever match that shape (see below), so these
# use digest-shaped keys rather than the short "k1"/"k2" the other tests in
# this file use for readability.
_KEY_A = "a" * 64
_KEY_B = "b" * 64


def test_step_cache_clear_removes_every_entry(tmp_path: Path) -> None:
    cache = StepCache(root=tmp_path)
    cache.put(_KEY_A, "v1", created_at="2026-01-01T00:00:00+00:00")
    cache.put(_KEY_B, "v2", created_at="2026-01-01T00:00:00+00:00")
    assert cache.clear() == 2
    assert cache.get(_KEY_A, ttl_seconds=None).hit is False
    assert cache.get(_KEY_B, ttl_seconds=None).hit is False
    # Idempotent: clearing an already-empty store removes nothing.
    assert cache.clear() == 0


def test_step_cache_stats(tmp_path: Path) -> None:
    cache = StepCache(root=tmp_path)
    assert cache.stats() == {"entries": 0, "bytes": 0, "path": str(tmp_path)}
    cache.put(_KEY_A, "v1", created_at="2026-01-01T00:00:00+00:00")
    cache.put(_KEY_B, "v2", created_at="2026-01-01T00:00:00+00:00")
    stats = cache.stats()
    assert stats["entries"] == 2
    assert stats["bytes"] > 0
    assert stats["path"] == str(tmp_path)


def test_step_cache_clear_ignores_in_flight_tmp_files(tmp_path: Path) -> None:
    # An atomic `put` in progress elsewhere leaves a `.tmp-*.json` file in
    # the same directory until its `os.replace` lands — `clear`/`stats` must
    # never touch it (#270 review finding 5): deleting it out from under a
    # concurrent `put` would make that `os.replace` raise.
    cache = StepCache(root=tmp_path)
    cache.put(_KEY_A, "v1", created_at="2026-01-01T00:00:00+00:00")
    in_flight = tmp_path / ".tmp-abc123.json"
    in_flight.write_text("{}", encoding="utf-8")
    assert cache.stats()["entries"] == 1
    assert cache.clear() == 1
    assert in_flight.exists()


def test_step_cache_clear_ignores_unrelated_json(tmp_path: Path) -> None:
    cache = StepCache(root=tmp_path)
    cache.put(_KEY_A, "v1", created_at="2026-01-01T00:00:00+00:00")
    unrelated = tmp_path / "notes.json"
    unrelated.write_text("{}", encoding="utf-8")
    assert cache.stats()["entries"] == 1
    assert cache.clear() == 1
    assert unrelated.exists()


# ---------------------------------------------------------------------------
# CacheDef
# ---------------------------------------------------------------------------


def test_cache_def_defaults_to_no_ttl_no_salt() -> None:
    defn = CacheDef()
    assert defn.ttl_seconds is None
    assert defn.key_salt is None
