"""Vector search tool plugin via chromadb.

Optional dep: ``chromadb``. Install with
``pip install circuitry-cof[vector_search]``.

Persistent storage is on by default — collections survive across runs.
Override ``persist_path`` to share the store across orchestrations or
point at an existing index.

Params:
  - ``mode``: ``"add" | "query" | "delete" | "count"``.
  - ``collection`` (required, str): logical namespace inside the store.
  - ``persist_path`` (optional, str): chromadb storage directory.
    Defaults to ``./.chromadb`` next to the working directory.
  - ``documents`` (add, list[str]).
  - ``embeddings`` (add, optional list[list[float]]): when omitted,
    chromadb computes embeddings via its default-embedded model
    (downloaded on first use).
  - ``ids`` (add, optional list[str]): one per document. Auto-generated
    when absent (UUID4).
  - ``metadatas`` (add, optional list[dict]).
  - ``query_text`` (query, str): natural-language query.
  - ``query_embedding`` (query, list[float]): explicit vector
    (mutually exclusive with query_text).
  - ``top_k`` (query, int, default 5).
  - ``where`` (query / delete, optional dict): metadata filter.
  - ``ids_to_delete`` (delete, optional list[str]).
"""

from __future__ import annotations

import importlib.util
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


_DEFAULT_PERSIST = "./.chromadb"


def _client(persist_path: str) -> Any:
    import chromadb  # type: ignore[import-not-found]

    persist_dir = Path(persist_path).expanduser()
    persist_dir.mkdir(parents=True, exist_ok=True)
    return chromadb.PersistentClient(path=str(persist_dir))


@dataclass(frozen=True)
class VectorSearchPlugin:
    name: str = "vector_search"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        try:
            import chromadb  # type: ignore[import-not-found]
            del chromadb
        except ImportError as exc:
            raise RuntimeError(
                "vector_search: chromadb not installed. "
                "Install with: pip install chromadb"
            ) from exc

        collection_name = params.get("collection")
        if not isinstance(collection_name, str) or not collection_name:
            raise ValueError("vector_search requires params['collection'].")
        persist_path = str(params.get("persist_path") or _DEFAULT_PERSIST)
        client = _client(persist_path)
        collection = client.get_or_create_collection(name=collection_name)

        mode = str(params.get("mode") or "query").lower()

        if mode == "add":
            documents = params.get("documents")
            if not isinstance(documents, list) or not all(
                isinstance(d, str) for d in documents
            ):
                raise ValueError(
                    "vector_search add: params['documents'] must be list[str]."
                )
            kwargs: dict[str, Any] = {"documents": list(documents)}
            ids = params.get("ids")
            if isinstance(ids, list) and len(ids) == len(documents):
                kwargs["ids"] = [str(i) for i in ids]
            else:
                kwargs["ids"] = [str(uuid.uuid4()) for _ in documents]
            if isinstance(params.get("embeddings"), list):
                kwargs["embeddings"] = params["embeddings"]
            if isinstance(params.get("metadatas"), list):
                kwargs["metadatas"] = params["metadatas"]
            collection.add(**kwargs)
            value: Any = {"added": len(documents), "ids": kwargs["ids"]}

        elif mode == "query":
            top_k = int(params.get("top_k") or 5)
            query_text = params.get("query_text")
            query_embedding = params.get("query_embedding")
            kwargs = {"n_results": top_k}
            if isinstance(params.get("where"), dict):
                kwargs["where"] = params["where"]
            if isinstance(query_text, str):
                kwargs["query_texts"] = [query_text]
            elif isinstance(query_embedding, list):
                kwargs["query_embeddings"] = [query_embedding]
            else:
                raise ValueError(
                    "vector_search query: provide params['query_text'] or "
                    "params['query_embedding']."
                )
            response = collection.query(**kwargs)
            # chromadb returns parallel lists wrapped in outer lists; flatten
            # the single-query case for friendlier consumption.
            ids = (response.get("ids") or [[]])[0]
            documents = (response.get("documents") or [[None] * len(ids)])[0]
            metadatas = (response.get("metadatas") or [[None] * len(ids)])[0]
            distances = (response.get("distances") or [[None] * len(ids)])[0]
            value = [
                {
                    "id": ids[i],
                    "document": documents[i],
                    "metadata": metadatas[i],
                    "distance": distances[i],
                }
                for i in range(len(ids))
            ]

        elif mode == "delete":
            kwargs = {}
            if isinstance(params.get("ids_to_delete"), list):
                kwargs["ids"] = [str(i) for i in params["ids_to_delete"]]
            if isinstance(params.get("where"), dict):
                kwargs["where"] = params["where"]
            if not kwargs:
                raise ValueError(
                    "vector_search delete: provide params['ids_to_delete'] or ['where']."
                )
            collection.delete(**kwargs)
            value = {"deleted": kwargs.get("ids") or "via_where"}

        elif mode == "count":
            value = {"count": collection.count()}

        else:
            raise ValueError(f"vector_search: unknown mode {mode!r}")

        return ToolResult(
            value=value,
            raw={"collection": collection_name, "mode": mode, "persist_path": persist_path},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        if importlib.util.find_spec("chromadb") is None:
            return CheckResult(
                ok=False,
                missing=["library:chromadb"],
                message="pip install chromadb",
            )
        return CheckResult(ok=True, missing=[])
