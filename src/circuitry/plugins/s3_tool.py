"""S3 tool plugin via boto3 — distinct from the s3 *runtime plugin* (which
persists run state). This one is the LLM-callable surface for object
storage operations within an orchestration.

Optional dep: ``boto3``. Install with ``pip install circuitry-cof[s3-tool]``.

Auth: standard AWS credentials chain (env, profile, instance role, etc).

Params:
  - ``mode``: ``"list" | "get" | "put" | "delete"``.
  - ``bucket`` (required, str).
  - ``key`` (get / put / delete, str).
  - ``prefix`` (list, optional, str).
  - ``content`` (put, str|bytes): payload (mutually exclusive with ``path``).
  - ``path`` (put / get, str): local file source/destination.
  - ``content_type`` (put, optional, str).
  - ``region`` (optional, str): override default region.
"""

from __future__ import annotations

import importlib.util
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


def _client(timeout_seconds: int, region: str | None = None) -> Any:
    import boto3  # type: ignore[import-not-found]
    from botocore.config import Config  # type: ignore[import-not-found]

    cfg = Config(
        connect_timeout=min(60, int(timeout_seconds)),
        read_timeout=int(timeout_seconds),
        retries={"max_attempts": 3, "mode": "standard"},
    )
    kwargs: dict[str, Any] = {"config": cfg}
    if region:
        kwargs["region_name"] = region
    return boto3.client("s3", **kwargs)


@dataclass(frozen=True)
class S3ToolPlugin:
    name: str = "s3"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        try:
            import boto3  # type: ignore[import-not-found]
            del boto3
        except ImportError as exc:
            raise RuntimeError(
                "s3: boto3 not installed. Install with: pip install boto3"
            ) from exc

        bucket = params.get("bucket")
        if not isinstance(bucket, str) or not bucket:
            raise ValueError("s3 requires params['bucket'].")
        mode = str(params.get("mode") or "list").lower()
        region = params.get("region") if isinstance(params.get("region"), str) else None
        client = _client(int(timeout_seconds), region=region)

        if mode == "list":
            kwargs: dict[str, Any] = {"Bucket": bucket}
            if isinstance(params.get("prefix"), str):
                kwargs["Prefix"] = params["prefix"]
            response = client.list_objects_v2(**kwargs)
            value: Any = [
                {
                    "key": obj.get("Key"),
                    "size": obj.get("Size"),
                    "etag": obj.get("ETag"),
                    "last_modified": str(obj.get("LastModified")),
                }
                for obj in response.get("Contents") or []
            ]

        elif mode == "get":
            key = params.get("key")
            if not isinstance(key, str) or not key:
                raise ValueError("s3 get requires params['key'].")
            response = client.get_object(Bucket=bucket, Key=key)
            body_bytes = response["Body"].read()
            destination = params.get("path")
            if isinstance(destination, str) and destination:
                dst = Path(destination).expanduser()
                dst.parent.mkdir(parents=True, exist_ok=True)
                dst.write_bytes(body_bytes)
                value = {"path": str(dst), "bytes": len(body_bytes)}
            else:
                # Best-effort decode for text payloads; fall back to byte length.
                try:
                    value = body_bytes.decode("utf-8")
                except UnicodeDecodeError:
                    value = {"bytes": len(body_bytes)}

        elif mode == "put":
            key = params.get("key")
            if not isinstance(key, str) or not key:
                raise ValueError("s3 put requires params['key'].")
            content = params.get("content")
            path = params.get("path")
            if (content is None) == (path is None):
                raise ValueError(
                    "s3 put: provide exactly one of params['content'] / ['path']."
                )
            kwargs = {"Bucket": bucket, "Key": key}
            if isinstance(params.get("content_type"), str):
                kwargs["ContentType"] = params["content_type"]
            if path is not None:
                with open(Path(str(path)).expanduser(), "rb") as fh:
                    kwargs["Body"] = fh.read()
            else:
                kwargs["Body"] = (
                    content.encode("utf-8") if isinstance(content, str) else bytes(content)
                )
            client.put_object(**kwargs)
            value = {"bucket": bucket, "key": key}

        elif mode == "delete":
            key = params.get("key")
            if not isinstance(key, str) or not key:
                raise ValueError("s3 delete requires params['key'].")
            client.delete_object(Bucket=bucket, Key=key)
            value = {"deleted": key}

        else:
            raise ValueError(f"s3: unknown mode {mode!r}")

        return ToolResult(
            value=value, raw={"mode": mode, "bucket": bucket},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        if importlib.util.find_spec("boto3") is None:
            return CheckResult(
                ok=False,
                missing=["library:boto3"],
                message="pip install boto3",
            )
        return CheckResult(ok=True, missing=[])
