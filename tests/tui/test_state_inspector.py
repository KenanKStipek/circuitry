"""The state inspector's model: the tree, the paths, the meta panel, the sources.

No Textual here — everything below is the reasoning the Runs view draws,
tested against one fixture state (``state_fixture``) shaped like a real run.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
from state_fixture import LONG_VALUE, T0, T1, every_path, fixture_state

from circuitry.cli.last_run import LastRun, read_last_run, state_from_env_pairs
from circuitry.cli.redaction import REDACTED
from circuitry.tui.execution import DONE, FAILED, RUNNING
from circuitry.tui.inspector import (
    MAX_PREVIEW,
    EffectMeta,
    StateStore,
    build_state_nodes,
    detail_lines,
    find_node,
    flatten,
    full_value_text,
    load_state_file,
    preview,
    render_text,
)


@pytest.fixture
def nodes() -> Any:
    return build_state_nodes(fixture_state())


def _paths(nodes: Any) -> set[str]:
    return {node.path for node in flatten(nodes)}


# -- the tree ----------------------------------------------------------------


def test_every_node_of_the_state_is_in_the_tree(nodes: Any) -> None:
    """Nothing in state is unreachable — including list items and meta."""
    assert _paths(nodes) == every_path(fixture_state())


def test_loop_iterations_and_use_namespaces_are_addressable(nodes: Any) -> None:
    for path in (
        "prime.over_items.iter_0.handle.value",
        "prime.over_items.iter_1.handle.meta.error",
        "prime.helper.summarise.value.headline",
        "prime.items.value[1]",
    ):
        assert find_node(nodes, path) is not None, f"{path} missing from:\n{render_text(nodes)}"


def test_prime_comes_first(nodes: Any) -> None:
    """A person opens this to look at ``prime``; the bookkeeping can wait."""
    assert [node.key for node in nodes][0] == "prime"
    assert set(node.key for node in nodes) == {"prime", "runtime", "topic"}


def test_a_copied_path_is_template_usable(nodes: Any) -> None:
    """The whole point of copy-path: it pastes into a template and resolves."""
    chevron = pytest.importorskip("chevron")
    state = fixture_state()
    node = find_node(nodes, "prime.helper.summarise.value.headline")
    assert node is not None
    rendered = chevron.render(f"{{{{{node.path}}}}}", state)
    assert rendered == "Control and communication"


def test_a_key_that_is_not_a_name_gets_the_bracket_form() -> None:
    nodes = build_state_nodes({"prime": {"a.b": {"value": 1}, "with space": 2}})
    assert _paths(nodes) >= {'prime["a.b"]', 'prime["a.b"].value', 'prime["with space"]'}


def test_the_tree_stops_at_the_node_cap() -> None:
    """A runaway state is truncated, and the row that owns it says so."""
    state = {"prime": {f"effect_{index}": {"value": index} for index in range(50)}}
    nodes = build_state_nodes(state, limit=10)
    assert len(flatten(nodes)) <= 10
    assert any(node.omitted for node in flatten(nodes))
    assert "more" in render_text(nodes)


# -- values ------------------------------------------------------------------


def test_a_long_string_is_truncated_on_the_row_and_whole_in_the_pane(nodes: Any) -> None:
    node = find_node(nodes, "prime.draft.value")
    assert node is not None
    assert len(node.preview) < len(LONG_VALUE)
    assert node.preview.endswith(f"({len(LONG_VALUE)} chars)")
    # Expanding is the detail pane, and it truncates nothing.
    assert LONG_VALUE in "\n".join(detail_lines(node))


def test_previews_read_by_type() -> None:
    assert preview({"a": 1, "b": 2}) == "{2 keys}"
    assert preview({}) == "{}"
    assert preview([1]) == "[1 item]"
    assert preview(None) == "null"
    assert preview(True) == "true"
    assert preview(3.5) == "3.5"
    assert preview("short") == '"short"'
    assert len(preview("x" * 500)) <= MAX_PREVIEW + 20


def test_structures_pretty_print_and_strings_stay_verbatim() -> None:
    assert full_value_text({"b": 1}) == '{\n  "b": 1\n}'
    assert full_value_text("line\nline") == "line\nline"


# -- the meta panel ----------------------------------------------------------


def test_the_meta_panel_reports_the_effect_that_produced_the_value(nodes: Any) -> None:
    node = find_node(nodes, "prime.draft.value")
    assert node is not None
    # A value inherits the meta of the effect it belongs to, so selecting
    # the value still says which adapter and model produced it.
    assert node.meta_path == "prime.draft"
    rows = dict(EffectMeta.from_mapping(node.meta).rows())
    assert rows["adapter"] == "openai"
    assert rows["model"] == "gpt-4o-mini"
    assert rows["tokens"] == "↑120 ↓340"
    assert rows["created"] == T0
    assert rows["completed"].startswith(T1)
    assert "(4.0s)" in rows["completed"]


def test_a_failed_effect_reports_its_error(nodes: Any) -> None:
    node = find_node(nodes, "prime.over_items.iter_1.handle")
    assert node is not None
    meta = EffectMeta.from_mapping(node.meta)
    assert meta.status == FAILED
    assert dict(meta.rows())["error"] == "adapter timed out"


def test_status_is_derived_the_way_the_execution_view_derives_it() -> None:
    assert EffectMeta.from_mapping({"created_at": T0}).status == RUNNING
    assert EffectMeta.from_mapping({"created_at": T0, "completed_at": T1}).status == DONE
    assert EffectMeta.from_mapping({"disabled": True}).status == "skipped"
    assert EffectMeta.from_mapping(None).status == "pending"


def test_detail_lines_lead_with_the_path(nodes: Any) -> None:
    lines = detail_lines(find_node(nodes, "prime.gate"))
    assert lines[0] == "prime.gate"
    assert "branch" in "\n".join(lines)


def test_nothing_selected_is_a_written_out_state() -> None:
    assert "Nothing selected." in detail_lines(None)[0]


# -- redaction ---------------------------------------------------------------


def test_effective_settings_are_shown_exactly_as_stored(nodes: Any) -> None:
    """Already redacted on the way in; re-redacting would misreport the file."""
    node = find_node(nodes, "runtime.effective_settings.api_key")
    assert node is not None
    assert node.value == REDACTED
    assert REDACTED in "\n".join(detail_lines(node))


# -- file mode ---------------------------------------------------------------


def test_a_saved_out_file_opens_into_the_same_tree(tmp_path: Path) -> None:
    path = tmp_path / "out.json"
    path.write_text(json.dumps(fixture_state()), encoding="utf-8")
    loaded = load_state_file(path)
    assert loaded.ok
    assert _paths(build_state_nodes(loaded.state)) == every_path(fixture_state())


def test_malformed_json_is_a_designed_error_not_an_exception(tmp_path: Path) -> None:
    path = tmp_path / "half-written.json"
    path.write_text('{"prime": {"draft": ', encoding="utf-8")
    loaded = load_state_file(path)
    assert not loaded.ok
    assert "Not valid JSON" in loaded.error
    assert "line 1" in loaded.error
    assert "live-state" in loaded.hint


def test_a_missing_file_says_so(tmp_path: Path) -> None:
    loaded = load_state_file(tmp_path / "nope.json")
    assert not loaded.ok
    assert "No such file" in loaded.error
    assert "--out" in loaded.hint


def test_a_directory_and_a_non_object_are_both_refused(tmp_path: Path) -> None:
    assert "directory" in load_state_file(tmp_path).error
    listing = tmp_path / "list.json"
    listing.write_text("[1, 2]", encoding="utf-8")
    assert "not an object" in load_state_file(listing).error


# -- the live source ---------------------------------------------------------


def test_the_store_tracks_a_run_from_launch_to_landing() -> None:
    store = StateStore()
    assert store.empty and not store.running

    store.begin(label="demo.yml")
    assert store.running
    first = store.revision

    store.publish({"prime": {"draft": {"value": "partial"}}})
    revision, state, running = store.snapshot()
    assert revision > first
    assert running
    assert state["prime"]["draft"]["value"] == "partial"

    store.finish({"prime": {"draft": {"value": "final"}}})
    revision_after, final, still_running = store.snapshot()
    assert revision_after > revision
    assert not still_running
    assert final["prime"]["draft"]["value"] == "final"


def test_a_failed_run_keeps_the_last_snapshot_it_published() -> None:
    """``finish(None)`` is what a failed run reports; the state stays put."""
    store = StateStore()
    store.publish({"prime": {"draft": {"value": "partial"}}})
    store.finish(None)
    _, state, running = store.snapshot()
    assert not running
    assert state["prime"]["draft"]["value"] == "partial"


# -- the last-run stash ------------------------------------------------------


def test_no_stash_is_none(tmp_path: Path) -> None:
    assert read_last_run(tmp_path / "last-run.json") is None


def test_a_corrupt_stash_reads_as_an_error(tmp_path: Path) -> None:
    path = tmp_path / "last-run.json"
    path.write_text("{not json", encoding="utf-8")
    stashed = read_last_run(path)
    assert stashed is not None
    assert not stashed.ok
    assert "not valid JSON" in stashed.error
    assert stashed.blocked_reason


def test_a_stashed_run_describes_itself(tmp_path: Path) -> None:
    path = tmp_path / "last-run.json"
    path.write_text(
        json.dumps(
            {
                "orchestration": "/tmp/demo.yml",
                "adapter": "ollama",
                "model": "llama3.1:8b",
                "env_vars": ["topic=cats", "count=3"],
                "dry_run": True,
            }
        ),
        encoding="utf-8",
    )
    stashed = read_last_run(path)
    assert stashed is not None
    assert stashed.ok and not stashed.blocked_reason
    assert stashed.initial_state() == {"topic": "cats", "count": 3}
    rows = dict(stashed.summary_rows())
    assert rows["orchestration"] == "/tmp/demo.yml"
    assert rows["adapter"] == "ollama"
    assert rows["dry run"] == "yes"


def test_a_run_that_stashed_redacted_secrets_refuses_to_replay() -> None:
    stashed = LastRun(path=Path("last-run.json"), args={
        "orchestration": "demo.yml",
        "env_vars": [f"api_key={REDACTED}"],
    })
    assert stashed.has_redacted_secrets
    assert "redacted" in stashed.blocked_reason


def test_env_pairs_parse_json_where_they_can_and_skip_junk() -> None:
    assert state_from_env_pairs(["a=1", 'b={"k": 2}', "c=plain", "junk"]) == {
        "a": 1,
        "b": {"k": 2},
        "c": "plain",
    }
    assert state_from_env_pairs(None) == {}
