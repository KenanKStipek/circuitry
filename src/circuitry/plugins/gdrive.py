"""Google Drive tool plugin via google-api-python-client.

Optional deps: ``google-api-python-client``, ``google-auth``. Install
with ``pip install circuitry-cof[gdrive]``.

Auth: ``GOOGLE_APPLICATION_CREDENTIALS`` pointing to a service-account
JSON file. Drive scope:
``https://www.googleapis.com/auth/drive``.

Params:
  - ``mode``: ``"list" | "upload" | "download" | "delete"``.
  - ``query`` (list, optional, str): Drive query syntax (e.g.
    ``"'parent_id' in parents"``). Default lists root.
  - ``page_size`` (list, optional, int).
  - ``path`` (upload, str): local file to upload.
  - ``name`` (upload, optional, str): override remote name.
  - ``parent_id`` (upload, optional, str): destination folder.
  - ``mime_type`` (upload, optional, str): override detected type.
  - ``file_id`` (download / delete, str).
  - ``destination`` (download, str): local target path.
"""

from __future__ import annotations

import importlib.util
import os
from dataclasses import dataclass
from io import BytesIO
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult

_SCOPES = ("https://www.googleapis.com/auth/drive",)


def _service(timeout_seconds: int) -> Any:
    from google.oauth2 import service_account  # type: ignore[import-not-found]
    from googleapiclient.discovery import build  # type: ignore[import-not-found]

    creds_path = os.environ.get("GOOGLE_APPLICATION_CREDENTIALS", "")
    if not creds_path:
        raise RuntimeError("gdrive: GOOGLE_APPLICATION_CREDENTIALS not set.")
    creds = service_account.Credentials.from_service_account_file(
        creds_path, scopes=list(_SCOPES)
    )
    return build("drive", "v3", credentials=creds, cache_discovery=False)


@dataclass(frozen=True)
class GDrivePlugin:
    name: str = "gdrive"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        try:
            import googleapiclient  # type: ignore[import-not-found]
            del googleapiclient
        except ImportError as exc:
            raise RuntimeError(
                "gdrive: google-api-python-client not installed. "
                "Install with: pip install google-api-python-client google-auth"
            ) from exc
        from googleapiclient.http import (  # type: ignore[import-not-found]
            MediaFileUpload,
            MediaIoBaseDownload,
        )

        service = _service(int(timeout_seconds))
        mode = str(params.get("mode") or "list").lower()

        if mode == "list":
            kwargs: dict[str, Any] = {
                "pageSize": int(params.get("page_size") or 50),
                "fields": "files(id,name,mimeType,size,modifiedTime,parents)",
            }
            if isinstance(params.get("query"), str):
                kwargs["q"] = params["query"]
            response = service.files().list(**kwargs).execute()
            value: Any = response.get("files") or []

        elif mode == "upload":
            path = params.get("path")
            if not isinstance(path, str) or not path:
                raise ValueError("gdrive upload requires params['path'].")
            local = Path(path).expanduser()
            metadata: dict[str, Any] = {"name": params.get("name") or local.name}
            if isinstance(params.get("parent_id"), str):
                metadata["parents"] = [params["parent_id"]]
            mime = params.get("mime_type")
            media = MediaFileUpload(
                str(local), mimetype=str(mime) if isinstance(mime, str) else None
            )
            value = service.files().create(
                body=metadata, media_body=media, fields="id,name,webViewLink"
            ).execute()

        elif mode == "download":
            file_id = params.get("file_id")
            destination = params.get("destination")
            if not isinstance(file_id, str) or not file_id:
                raise ValueError("gdrive download requires params['file_id'].")
            if not isinstance(destination, str) or not destination:
                raise ValueError("gdrive download requires params['destination'].")
            request = service.files().get_media(fileId=file_id)
            buf = BytesIO()
            downloader = MediaIoBaseDownload(buf, request)
            done = False
            while not done:
                _status, done = downloader.next_chunk()
            dst = Path(destination).expanduser()
            dst.parent.mkdir(parents=True, exist_ok=True)
            dst.write_bytes(buf.getvalue())
            value = {"path": str(dst), "bytes": len(buf.getvalue())}

        elif mode == "delete":
            file_id = params.get("file_id")
            if not isinstance(file_id, str) or not file_id:
                raise ValueError("gdrive delete requires params['file_id'].")
            service.files().delete(fileId=file_id).execute()
            value = {"deleted": file_id}

        else:
            raise ValueError(f"gdrive: unknown mode {mode!r}")

        return ToolResult(
            value=value, raw={"mode": mode},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        missing: list[str] = []
        if importlib.util.find_spec("googleapiclient") is None:
            missing.append("library:google-api-python-client")
        try:
            has_oauth2 = importlib.util.find_spec("google.oauth2") is not None
        except ModuleNotFoundError:
            has_oauth2 = False
        if not has_oauth2:
            missing.append("library:google-auth")
        if not os.environ.get("GOOGLE_APPLICATION_CREDENTIALS"):
            missing.append("env:GOOGLE_APPLICATION_CREDENTIALS")
        if missing:
            return CheckResult(ok=False, missing=missing)
        return CheckResult(ok=True, missing=[])
