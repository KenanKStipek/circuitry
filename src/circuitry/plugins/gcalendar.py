"""Google Calendar tool plugin via google-api-python-client.

Optional deps: ``google-api-python-client``, ``google-auth``. Install
with ``pip install circuitry-cof[gcalendar]``.

Auth: ``GOOGLE_APPLICATION_CREDENTIALS`` pointing to a service-account
JSON file with calendar scope (``https://www.googleapis.com/auth/calendar``).
For domain-wide delegation, the service account must be granted access
to the calendar.

Params:
  - ``mode``: ``"list_events" | "create_event" | "delete_event"``.
  - ``calendar_id`` (optional, default ``"primary"``).
  - ``time_min`` / ``time_max`` (list_events, ISO 8601 strings).
  - ``max_results`` (list_events, int, default 50).
  - ``summary``, ``description``, ``start``, ``end`` (create_event).
    ``start``/``end`` accept ISO 8601 strings or dicts in Google's format.
  - ``event_id`` (delete_event).
"""

from __future__ import annotations

import importlib.util
import os
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult

_SCOPES = ("https://www.googleapis.com/auth/calendar",)


def _service(timeout_seconds: int) -> Any:
    from google.oauth2 import service_account  # type: ignore[import-not-found]
    from googleapiclient.discovery import build  # type: ignore[import-not-found]

    creds_path = os.environ.get("GOOGLE_APPLICATION_CREDENTIALS", "")
    if not creds_path:
        raise RuntimeError("gcalendar: GOOGLE_APPLICATION_CREDENTIALS not set.")
    creds = service_account.Credentials.from_service_account_file(
        creds_path, scopes=list(_SCOPES)
    )
    return build("calendar", "v3", credentials=creds, cache_discovery=False)


def _to_event_time(value: Any) -> dict[str, Any]:
    if isinstance(value, dict):
        return value
    if isinstance(value, str):
        # Plain ISO 8601 → dateTime field with implicit timezone.
        return {"dateTime": value}
    raise ValueError(
        "gcalendar: start/end must be a string (ISO 8601) or "
        "dict in Google's event-time format."
    )


@dataclass(frozen=True)
class GCalendarPlugin:
    name: str = "gcalendar"

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
                "gcalendar: google-api-python-client not installed. "
                "Install with: pip install google-api-python-client google-auth"
            ) from exc

        service = _service(int(timeout_seconds))
        mode = str(params.get("mode") or "list_events").lower()
        cal_id = str(params.get("calendar_id") or "primary")

        if mode == "list_events":
            kwargs: dict[str, Any] = {
                "calendarId": cal_id,
                "maxResults": int(params.get("max_results") or 50),
                "singleEvents": True,
                "orderBy": "startTime",
            }
            if isinstance(params.get("time_min"), str):
                kwargs["timeMin"] = params["time_min"]
            if isinstance(params.get("time_max"), str):
                kwargs["timeMax"] = params["time_max"]
            response = service.events().list(**kwargs).execute()
            value: Any = response.get("items") or []

        elif mode == "create_event":
            summary = params.get("summary")
            start = params.get("start")
            end = params.get("end")
            if not isinstance(summary, str) or not summary:
                raise ValueError("gcalendar create_event requires params['summary'].")
            if start is None or end is None:
                raise ValueError(
                    "gcalendar create_event requires params['start'] and ['end']."
                )
            body: dict[str, Any] = {
                "summary": summary,
                "start": _to_event_time(start),
                "end": _to_event_time(end),
            }
            if isinstance(params.get("description"), str):
                body["description"] = params["description"]
            value = service.events().insert(calendarId=cal_id, body=body).execute()

        elif mode == "delete_event":
            event_id = params.get("event_id")
            if not isinstance(event_id, str) or not event_id:
                raise ValueError("gcalendar delete_event requires params['event_id'].")
            service.events().delete(calendarId=cal_id, eventId=event_id).execute()
            value = {"deleted": event_id}

        else:
            raise ValueError(f"gcalendar: unknown mode {mode!r}")

        return ToolResult(
            value=value, raw={"mode": mode, "calendar_id": cal_id},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        missing: list[str] = []
        if importlib.util.find_spec("googleapiclient") is None:
            missing.append("library:google-api-python-client")
        # ``google`` is a namespace package — find_spec raises
        # ModuleNotFoundError when the parent isn't installed.
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
