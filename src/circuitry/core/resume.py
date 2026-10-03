"""`cof run --resume` — which effects a resumed run skips, and which rerun.

An effect is safe to reuse exactly when its node shows a *finished* run:
``meta.completed_at`` is set and ``meta.error`` is not. Anything else —
never started, still mid-flight when the process died (Ctrl-C, a crash),
or finished with an error — reruns. This mirrors every runtime's own
completion contract (``prompt.py``, ``tool.py``, ``use.py``, ``dynamic.py``,
``loop.py``, ``conditional.py`` all set ``meta.completed_at`` *and clear*
``meta.error`` on their success path, and set both on an absorbed failure),
so the same check applies uniformly to a leaf effect or a container.

A loop pass is different: a named chain loop (every ``while``, or an
``each`` not running ``flow: tree``) doesn't ask whether an ``iter_<N>``
node merely *looks* finished — that can't tell a pass that genuinely
completed from one that was only partway written when a run stopped (an
unnamed ``if`` in the body whose condition raised before touching the
node; any interruption between body effects). Instead the loop keeps its
own authoritative record, ``meta.completed_passes`` — the indices
``_execute_body`` actually returned successfully for, persisted on both
its success and its failure path — and resumes at the first index missing
from the contiguous prefix of that list. See ``LoopRuntime.execute``.
"""

from __future__ import annotations

import hashlib
from pathlib import Path
from typing import Any

__all__ = ["document_sha256", "effect_completed_ok"]


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

