"""Cross-encoder re-rank tool plugin via sentence-transformers.

Optional dep: ``sentence-transformers``. Install with
``pip install circuitry-cof[rerank]``.

A retrieve-then-rerank pipeline first uses an embedding model
(``embed``) for cheap nearest-neighbour candidate retrieval, then this
plugin scores each candidate against the query with a more expensive
cross-encoder. Returns candidates sorted by descending relevance score.

Params:
  - ``query`` (required, str).
  - ``candidates`` (required, list[str]): documents to score.
  - ``top_k`` (optional, int): cap result count.
  - ``model`` (optional, str). Default
    ``"cross-encoder/ms-marco-MiniLM-L-6-v2"``.
  - ``device`` (optional, str): ``"cpu"`` / ``"cuda"`` / ``"mps"``.

Returns ``value`` = list of ``{"text", "score", "index"}`` dicts where
``index`` references the position in the original ``candidates`` list.
"""

from __future__ import annotations

import importlib.util
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult

_DEFAULT_MODEL = "cross-encoder/ms-marco-MiniLM-L-6-v2"

_MODEL_CACHE: dict[str, Any] = {}


def _resolve_model(model_name: str, device: str | None) -> Any:
    cache_key = f"{model_name}@{device or 'auto'}"
    if cache_key not in _MODEL_CACHE:
        from sentence_transformers import CrossEncoder  # type: ignore[import-not-found]

        _MODEL_CACHE[cache_key] = CrossEncoder(model_name, device=device)
    return _MODEL_CACHE[cache_key]


@dataclass(frozen=True)
class RerankPlugin:
    name: str = "rerank"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        try:
            import sentence_transformers  # type: ignore[import-not-found]
            del sentence_transformers
        except ImportError as exc:
            raise RuntimeError(
                "rerank: sentence-transformers not installed. "
                "Install with: pip install sentence-transformers"
            ) from exc

        query = params.get("query")
        candidates = params.get("candidates")
        if not isinstance(query, str) or not query.strip():
            raise ValueError("rerank requires params['query'].")
        if not isinstance(candidates, list) or not all(
            isinstance(c, str) for c in candidates
        ):
            raise ValueError(
                "rerank requires params['candidates'] as a list of strings."
            )
        if not candidates:
            return ToolResult(
                value=[], raw={"count": 0},
                stdout=None, stderr=None, exit_code=None,
            )

        model_name = str(params.get("model") or _DEFAULT_MODEL)
        device = params.get("device")
        top_k = params.get("top_k")

        model = _resolve_model(
            model_name, device if isinstance(device, str) else None
        )
        pairs = [(query, c) for c in candidates]
        scores = model.predict(pairs)

        ranked = sorted(
            (
                {"index": i, "text": candidates[i], "score": float(scores[i])}
                for i in range(len(candidates))
            ),
            key=lambda r: r["score"],
            reverse=True,
        )
        if isinstance(top_k, int) and top_k > 0:
            ranked = ranked[:top_k]

        return ToolResult(
            value=ranked,
            raw={"model": model_name, "count": len(ranked)},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        if importlib.util.find_spec("sentence_transformers") is None:
            return CheckResult(
                ok=False,
                missing=["library:sentence-transformers"],
                message="pip install sentence-transformers",
            )
        return CheckResult(ok=True, missing=[])
