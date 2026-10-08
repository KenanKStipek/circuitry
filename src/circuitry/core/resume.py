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

__all__ = ["effect_completed_ok"]


def _legacy_document_sha256(path: Path) -> str:
    """The pre-#407 `document_hash` algorithm: *path*'s own bytes, then
    every referenced prompt file's bytes (sorted, no path label, no
    length) — every release before #407 wrote this to
    `state.runtime.last_run.document_hash`.

    `cli.app._resolve_resume_state`'s content-hash check (#270) falls back
    to this when the current `core.prompt_compose.document_content_digest`
    (the one algorithm every surface now uses, #396/#405/#407) doesn't
    match a saved state's recorded hash — so a run saved by an older
    release still resumes without `--force`. COMPATIBILITY FALLBACK ONLY:
    keep this, and the fallback call, through at least the first tagged
    release after this one, then remove both.
    """
    hasher = hashlib.sha256()
    hasher.update(path.read_bytes())
    for prompt_file in sorted(set(_referenced_prompt_files(path))):
        try:
            hasher.update(prompt_file.read_bytes())
        except OSError:
            continue
    return hasher.hexdigest()


def _referenced_prompt_files(path: Path) -> list[Path]:
    try:
        from .prompt_compose import referenced_prompt_file_paths
        from .prompt_files import default_project_root
        from .yaml_load import load_yaml

        orch = load_yaml(path.read_text(encoding="utf-8"))
        if not isinstance(orch, dict):
            return []
        document_dir = path.resolve().parent
        return referenced_prompt_file_paths(
            orch,
            document_dir=document_dir,
            confinement_root=default_project_root(document_dir),
        )
    except Exception:
        return []


def effect_completed_ok(node: Any) -> bool:
    """True when *node* is an effect record that finished without error."""
    if not isinstance(node, dict):
        return False
    meta = node.get("meta")
    if not isinstance(meta, dict):
        return False
    return bool(meta.get("completed_at")) and not meta.get("error")

