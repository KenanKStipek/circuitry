"""``runtime.state.prompt_sent``: how much of a prompt's text a ``prompt`` effect keeps in state (#577).

Every ``prompt`` effect records the text it sent in ``meta.prompt_sent``. The
setting decides what that field holds, and only that field: the text in memory
(what the adapter receives, retries, fallback, complexity scoring, token
estimates) is never touched.

* ``full`` (the default): the whole text.
* ``sha256``: ``null``.
* ``head:<N>`` (N >= 1): the first N characters, then a marker saying how many
  characters were left out. A text of N characters or fewer is kept whole.

In every mode two fields describe the whole text: ``meta.prompt_sent_sha256``
(lowercase hex SHA-256 of its UTF-8 bytes) and ``meta.prompt_chars`` (its
length in Unicode code points).
"""

from __future__ import annotations

import hashlib
import re
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

SETTING_PATH = "runtime.state.prompt_sent"

_HEAD = re.compile(r"head:([0-9]+)")

_FORM = "'full', 'sha256' or 'head:<N>' (N a positive integer)"


@dataclass(frozen=True)
class PromptSentSetting:
    """A parsed ``runtime.state.prompt_sent``.

    *mode* is ``"full"``, ``"sha256"`` or ``"head"``; *head_chars* is N for
    ``"head"`` and ``None`` otherwise.
    """

    mode: str = "full"
    head_chars: int | None = None


FULL = PromptSentSetting()


def parse_prompt_sent_setting(value: Any) -> tuple[PromptSentSetting, list[str]]:
    """Validate ``runtime.state.prompt_sent``. Returns ``(setting, errors)``.

    An absent value (``None``) is the default, ``full``. Anything else that is
    not one of the three forms, ``head:0`` included, is an error naming the key.
    """
    if value is None:
        return FULL, []
    if isinstance(value, str):
        if value == "full":
            return FULL, []
        if value == "sha256":
            return PromptSentSetting("sha256"), []
        match = _HEAD.fullmatch(value)
        if match is not None and int(match.group(1)) >= 1:
            return PromptSentSetting("head", int(match.group(1))), []
    return FULL, [f"{SETTING_PATH} must be {_FORM}, got {value!r}."]


def prompt_sent_errors(runtime_config: Mapping[str, Any] | None) -> list[str]:
    """The configuration errors of ``runtime.state.prompt_sent`` in *runtime_config*."""
    state_cfg = (runtime_config or {}).get("state")
    if not isinstance(state_cfg, Mapping):
        return []
    return parse_prompt_sent_setting(state_cfg.get("prompt_sent"))[1]


def prompt_sent_setting(runtime_config: Mapping[str, Any] | None) -> PromptSentSetting:
    """The run's ``runtime.state.prompt_sent``; raises ``ValueError`` when invalid.

    Config resolution and ``cof check`` reject a bad value long before a run
    gets here; failing loudly beats quietly recording a prompt in full that
    the document asked to keep out of state.
    """
    state_cfg = (runtime_config or {}).get("state")
    if not isinstance(state_cfg, Mapping):
        return FULL
    setting, errors = parse_prompt_sent_setting(state_cfg.get("prompt_sent"))
    if errors:
        raise ValueError(errors[0])
    return setting


def record_text(text: str, setting: PromptSentSetting) -> str | None:
    """What *setting* leaves of *text* in a state record: all of it, a head, or ``None``."""
    if setting.mode == "sha256":
        return None
    if setting.mode == "head":
        keep = setting.head_chars or 0
        if len(text) <= keep:
            return text
        return f"{text[:keep]}\n[… {len(text) - keep} more characters not recorded]"
    return text


def prompt_sent_fields(text: str, setting: PromptSentSetting) -> dict[str, Any]:
    """``meta.prompt_sent``, ``meta.prompt_sent_sha256`` and ``meta.prompt_chars`` for *text*."""
    return {
        "prompt_sent": record_text(text, setting),
        "prompt_sent_sha256": hashlib.sha256(text.encode("utf-8", "replace")).hexdigest(),
        "prompt_chars": len(text),
    }
