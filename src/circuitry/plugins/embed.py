"""Text embedding tool plugin via sentence-transformers.

Optional dep: ``sentence-transformers``. Install with
``pip install circuitry-cof[embed]``.

The first call with a given ``model`` triggers a model download
(hundreds of MB; cached under ``~/.cache/huggingface``). For latency-
sensitive paths, warm the model with a no-op call at startup.

Params:
  - ``input`` (required, str | list[str]): text(s) to embed.
  - ``model`` (optional, str): model name. Default
    ``"sentence-transformers/all-MiniLM-L6-v2"`` (~80MB, 384 dims).
  - ``normalize`` (optional, bool, default True): L2-normalize so dot
    products are cosine similarities.
  - ``device`` (optional, str): ``"cpu"`` / ``"cuda"`` / ``"mps"``.

Returns ``value`` = list of float lists. When ``input`` is a single
string, returns a single embedding (still wrapped in a list of length 1
to keep the type stable for downstream consumers).
"""

from __future__ import annotations

import importlib.util
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult

_DEFAULT_MODEL = "sentence-transformers/all-MiniLM-L6-v2"

# Tiny in-process cache: model name → SentenceTransformer instance.
# Avoids reloading the same model across plugin invocations within a run.
_MODEL_CACHE: dict[str, Any] = {}


def _resolve_model(model_name: str, device: str | None) -> Any:
    cache_key = f"{model_name}@{device or 'auto'}"
    if cache_key not in _MODEL_CACHE:
        from sentence_transformers import (
            SentenceTransformer,  # type: ignore[import-not-found]
        )

        _MODEL_CACHE[cache_key] = SentenceTransformer(
            model_name, device=device
        )
    return _MODEL_CACHE[cache_key]


@dataclass(frozen=True)
class EmbedPlugin:
    name: str = "embed"

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
                "embed: sentence-transformers not installed. "
                "Install with: pip install sentence-transformers"
            ) from exc

        text_input = params.get("input")
        if isinstance(text_input, str):
            texts = [text_input]
            single = True
        elif isinstance(text_input, list) and all(
            isinstance(t, str) for t in text_input
        ):
            texts = list(text_input)
            single = False
        else:
            raise ValueError(
                "embed: params['input'] must be a string or list of strings."
            )
        model_name = str(params.get("model") or _DEFAULT_MODEL)
        normalize = bool(params.get("normalize", True))
        device = params.get("device")

        model = _resolve_model(
            model_name, device if isinstance(device, str) else None
        )
        embeddings = model.encode(
            texts, normalize_embeddings=normalize, convert_to_numpy=True
        )
        # numpy arrays don't JSON-serialize cleanly — coerce to list[list[float]].
        vectors = [list(map(float, row)) for row in embeddings]

        return ToolResult(
            value=vectors,
            raw={
                "model": model_name,
                "count": len(vectors),
                "dimensions": len(vectors[0]) if vectors else 0,
                "single_input": single,
            },
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
