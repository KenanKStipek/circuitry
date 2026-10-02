"""`cof run --resume` — which effects a resumed run skips, and which rerun.

An effect is safe to reuse exactly when its node shows a *finished* run:
``meta.completed_at`` is set and ``meta.error`` is not. Anything else —
never started, still mid-flight when the process died (Ctrl-C, a crash),
or finished with an error — reruns. This mirrors every runtime's own
completion contract (``prompt.py``, ``tool.py``, ``use.py``, ``dynamic.py``,
``loop.py`` all set ``meta.completed_at`` on both their success and their
absorbed-failure paths, and only on those paths), so the same check applies
uniformly to a leaf effect, a container, or one loop pass.
"""

from __future__ import annotations

import hashlib
from pathlib import Path
from typing import Any

__all__ = ["document_sha256", "effect_completed_ok", "loop_pass_completed_ok"]


def document_sha256(path: Path) -> str:
    """Content hash of an orchestration file, for resume's change check.

    Every run stamps this at `state.runtime.last_run.document_hash`
    (`cli.runtime_shim.run`); `cof run --resume` recomputes it for the
    document about to run and refuses to resume on a mismatch unless
    `--force` (#270).
    """
    return hashlib.sha256(path.read_bytes()).hexdigest()


def effect_completed_ok(node: Any) -> bool:
    """True when *node* is an effect record that finished without error."""
    if not isinstance(node, dict):
        return False
    meta = node.get("meta")
    if not isinstance(meta, dict):
        return False
    return bool(meta.get("completed_at")) and not meta.get("error")


def loop_pass_completed_ok(iter_node: Any) -> bool:
    """True when every effect inside one loop pass (an ``iter_<N>`` node)
    finished without error.

    *iter_node* is a container of the pass's own body-effect nodes, not an
    effect record itself — it carries no ``meta`` of its own — so this walks
    every child looking for one, recursing through nested containers (an
    unnamed ``if``/``dynamic`` inside the body merges its own children
    straight into the pass, a named one nests them one level deeper) rather
    than trusting a fixed set of body names.
    """
    if isinstance(iter_node, dict):
        meta = iter_node.get("meta")
        if isinstance(meta, dict):
            if meta.get("error"):
                return False
            if not meta.get("completed_at"):
                return False
        return all(
            loop_pass_completed_ok(value)
            for key, value in iter_node.items()
            if key != "meta" and isinstance(value, (dict, list))
        )
    if isinstance(iter_node, list):
        return all(loop_pass_completed_ok(item) for item in iter_node)
    return True
