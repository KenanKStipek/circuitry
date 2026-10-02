"""Tests for the tool-plugin capability tags (#275)."""

from __future__ import annotations

from circuitry.plugins.capabilities import (
    CAPABILITIES,
    FS_WRITE,
    NETWORK,
    PLUGIN_CAPABILITIES,
    PYTHON_EVAL,
    SHELL,
    capabilities_of,
)
from circuitry.plugins.factory import PLUGIN_REGISTRY


def test_every_tagged_plugin_is_a_real_registry_name() -> None:
    """The tag table can only drift by naming something that doesn't exist."""
    assert set(PLUGIN_CAPABILITIES) <= set(PLUGIN_REGISTRY)


def test_every_tag_is_one_of_the_four_capabilities() -> None:
    for name, caps in PLUGIN_CAPABILITIES.items():
        assert caps, f"{name}: tagged with an empty set (omit it instead)"
        assert caps <= set(CAPABILITIES), f"{name}: unknown capability in {caps}"


def test_shell_and_python_eval_plugins_are_tagged() -> None:
    assert capabilities_of("shell") == {SHELL}
    assert capabilities_of("python_eval") == {PYTHON_EVAL}


def test_fs_plugin_is_tagged_fs_write() -> None:
    assert capabilities_of("fs") == {FS_WRITE}


def test_http_family_is_tagged_network() -> None:
    for name in ("http", "web_fetch", "webhook", "web_search"):
        assert capabilities_of(name) == {NETWORK}, name


def test_a_plugin_can_carry_more_than_one_capability() -> None:
    assert capabilities_of("git") == {SHELL, NETWORK}
    assert capabilities_of("ffmpeg") == {SHELL}


def test_benign_local_compute_plugins_need_nothing() -> None:
    for name in ("math", "regex", "json", "clock", "hash", "uuid"):
        assert capabilities_of(name) == frozenset(), name


def test_unrecognized_name_needs_nothing() -> None:
    assert capabilities_of("not-a-real-plugin") == frozenset()
    assert capabilities_of("") == frozenset()


def test_name_matching_is_case_and_whitespace_insensitive() -> None:
    assert capabilities_of(" SHELL ") == {SHELL}
