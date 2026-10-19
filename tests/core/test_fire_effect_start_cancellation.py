"""A TUI-style cancel landing inside `Store.fire_effect_start`'s own publish
(#634) must not leave an inconsistent node or an unmatched lifecycle event.

A TUI session cancels a run by raising from its own `on_write` callback the
next time it is called (`tui.launch.RunSession._observe`), not through the
signal-based `CancellationToken`. Since `fire_effect_start` now republishes
the live state at every effect start -- including a tree-flow pass still
queued when the cancel lands -- that republish is itself one such write,
and can be the one that raises.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import yaml

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run


class _CancelOnNthSight:
    """Mimics a TUI's own cancel: once *armed*, raises the *nth* time it
    sees *marker_path* appear in the state -- the first sighting is the
    pass's own `fire_effect_start` republish (before its meta is even set
    up); a tool's own per-attempt republish, right after `created_at` is
    set, is the next one.
    """

    def __init__(self, marker_path: tuple[str, ...], nth: int) -> None:
        self.marker_path = marker_path
        self.nth = nth
        self.armed = False
        self.raised = False
        self._seen = 0

    def __call__(self, state: dict[str, Any]) -> None:
        if not self.armed or self.raised:
            return
        cur: Any = state
        for part in self.marker_path:
            if not isinstance(cur, dict) or part not in cur:
                return
            cur = cur[part]
        self._seen += 1
        if self._seen < self.nth:
            return
        self.raised = True
        raise RuntimeError("cancelled")


def _run_sequential_tree_loop(tmp_path: Path, observer: _CancelOnNthSight) -> Any:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "lp",
                "flow": "tree",
                "max_concurrency": 1,
                "each": {"in": "input.items", "as": "it"},
                "body": [{"type": "tool", "name": "step", "provider": "uuid"}],
            }
        ],
    }
    path = tmp_path / "orch.yml"
    path.write_text(yaml.dump(orch, sort_keys=False), encoding="utf-8")

    starts: list[str] = []
    completes: list[str] = []

    def _on_start(path_: str, node: dict[str, Any]) -> None:
        starts.append(path_)
        if path_ == "prime.lp.iter_0.step":
            # iter_0 has just started -- arm the cancel so it fires the
            # next time iter_1's own pass appears at all, i.e. exactly
            # when the pass still queued behind max_concurrency: 1 is
            # about to start.
            observer.armed = True

    return run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={"input": {"items": ["a", "b", "c"]}},
            config=CircuitryConfig(),
            skip_preflight=True,
            state_observer=observer,
            effect_start_observer=_on_start,
            effect_observer=lambda p, n: completes.append(p),
        )
    ), starts, completes


def test_a_cancel_landing_in_fire_effect_start_leaves_no_orphaned_node(
    tmp_path: Path,
) -> None:
    """The cancel lands on the pass's own first republish (before any of
    its meta exists) -- `fire_effect_start`'s own guard must still close
    the pair.
    """
    observer = _CancelOnNthSight(("prime", "lp", "iter_1"), nth=1)
    result, starts, completes = _run_sequential_tree_loop(tmp_path, observer)

    assert observer.raised, "the cancel never actually landed"
    assert result.ok is False

    lp = result.state.get("prime", {}).get("lp", {})
    # iter_0 ran to completion before the cancel was even armed.
    assert lp["iter_0"]["step"]["meta"]["completed_at"] is not None
    iter1_meta = lp["iter_1"]["step"]["meta"]
    assert iter1_meta.get("created_at") is None or iter1_meta.get("completed_at") is not None
    assert iter1_meta.get("error") is not None

    # Every effect-start event has a matching effect-complete event.
    assert set(starts) - set(completes) == set()


def test_a_cancel_landing_in_a_tools_own_retry_publish_leaves_no_orphaned_node(
    tmp_path: Path,
) -> None:
    """The cancel lands on the tool's own per-attempt republish
    (`Store.publish_or_finalize`), after `created_at` is already set --
    exactly the shape the review flagged: a node left with `created_at`
    set and `completed_at` still null.
    """
    observer = _CancelOnNthSight(("prime", "lp", "iter_1"), nth=2)
    result, starts, completes = _run_sequential_tree_loop(tmp_path, observer)

    assert observer.raised, "the cancel never actually landed"
    assert result.ok is False

    lp = result.state.get("prime", {}).get("lp", {})
    iter1_meta = lp["iter_1"]["step"]["meta"]
    # The cancel landed after created_at was set -- confirming this test
    # actually exercises the retry-publish call, not the first one.
    assert iter1_meta.get("created_at") is not None
    assert iter1_meta.get("completed_at") is not None
    assert iter1_meta.get("error") is not None

    assert set(starts) - set(completes) == set()
