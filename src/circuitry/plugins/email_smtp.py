"""SMTP email tool plugin — send a message via stdlib smtplib.

Params:
  - ``host`` (required): SMTP server host.
  - ``port`` (optional, default 587): SMTP port.
  - ``username``, ``password`` (optional): SMTP auth. When both omitted,
    no auth is attempted (useful for relay-only servers).
  - ``use_tls`` (optional, default True): STARTTLS upgrade.
  - ``from_addr`` (required): envelope sender.
  - ``to`` (required, str|list[str]): recipient(s).
  - ``subject`` (required, str).
  - ``body`` (required, str).
  - ``content_type`` (optional, default ``"text/plain"``): ``"text/plain"``
    or ``"text/html"``.
  - ``cc`` (optional, str|list[str]).
  - ``bcc`` (optional, str|list[str]).

Returns ``value`` = number of recipients accepted by the SMTP server.
"""

from __future__ import annotations

import smtplib
import ssl
from dataclasses import dataclass
from email.mime.multipart import MIMEMultipart
from email.mime.text import MIMEText
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


def _coerce_addr_list(value: Any, *, field: str) -> list[str]:
    if value is None or value == "":
        return []
    if isinstance(value, str):
        return [value]
    if isinstance(value, list) and all(isinstance(v, str) for v in value):
        return list(value)
    raise ValueError(
        f"email_smtp: params['{field}'] must be a string or list of strings."
    )


@dataclass(frozen=True)
class EmailSmtpPlugin:
    name: str = "email_smtp"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        host = params.get("host")
        if not isinstance(host, str) or not host:
            raise ValueError("email_smtp requires params['host'].")
        port = int(params.get("port") or 587)
        from_addr = params.get("from_addr")
        if not isinstance(from_addr, str) or not from_addr:
            raise ValueError("email_smtp requires params['from_addr'].")
        subject = str(params.get("subject") or "")
        body = params.get("body")
        if not isinstance(body, str):
            raise ValueError("email_smtp requires params['body'] as str.")
        content_type = str(params.get("content_type") or "text/plain")
        if content_type not in ("text/plain", "text/html"):
            raise ValueError(
                "email_smtp: content_type must be 'text/plain' or 'text/html'."
            )

        to_list = _coerce_addr_list(params.get("to"), field="to")
        cc_list = _coerce_addr_list(params.get("cc"), field="cc")
        bcc_list = _coerce_addr_list(params.get("bcc"), field="bcc")
        if not to_list:
            raise ValueError("email_smtp requires at least one params['to'] address.")

        msg = MIMEMultipart("alternative")
        msg["Subject"] = subject
        msg["From"] = from_addr
        msg["To"] = ", ".join(to_list)
        if cc_list:
            msg["Cc"] = ", ".join(cc_list)
        msg.attach(MIMEText(body, content_type.split("/", 1)[1], "utf-8"))

        all_recipients = to_list + cc_list + bcc_list
        username = params.get("username")
        password = params.get("password")
        use_tls = bool(params.get("use_tls", True))

        # smtplib accepts a single timeout for connect; honour the
        # tool's timeout_seconds budget there.
        with smtplib.SMTP(host, port, timeout=int(timeout_seconds)) as server:
            if use_tls:
                server.starttls(context=ssl.create_default_context())
            if username and password:
                server.login(username, password)
            refused = server.sendmail(from_addr, all_recipients, msg.as_string())
            accepted = len(all_recipients) - len(refused)

        return ToolResult(
            value=accepted,
            raw={
                "host": host,
                "port": port,
                "to": to_list,
                "cc": cc_list,
                "bcc": bcc_list,
                "refused": list(refused.keys()) if isinstance(refused, dict) else [],
            },
            stdout=None,
            stderr=None,
            exit_code=None,
        )

    def check(self) -> CheckResult:
        return CheckResult(
            ok=True,
            missing=[],
            message="email_smtp uses stdlib smtplib; SMTP host reachability "
                    "is validated when a message is sent.",
        )
