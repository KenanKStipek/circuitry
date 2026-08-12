"""GPG tool plugin — encrypt / decrypt / sign / verify via the ``gpg`` binary.

Multi-mode wrapper rather than a raw pass-through: each mode constructs
a sensible argument list so YAML callers don't need to memorise GPG's
flag conventions.

Params:
  - ``mode``: ``"encrypt" | "decrypt" | "sign" | "verify"``.
  - ``input``: payload (string) when ``from_path`` is False (default),
    else a path to read.
  - ``output`` (optional): path to write result to.
  - ``recipient`` (encrypt): public-key UID.
  - ``signer`` (sign): private-key UID (omit for default key).
  - ``armor`` (encrypt/sign, bool, default True): produce ASCII-armored
    output.
  - ``detached`` (sign, bool, default False): produce a detached
    signature.
  - ``signature`` (verify, str|path): signature to verify against.
  - ``passphrase`` (optional): for non-interactive operations. Passed
    via ``--passphrase`` (NOT echoed in error messages).
  - ``from_path`` (bool, default False).
"""

from __future__ import annotations

import shlex
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from ._subprocess import check_binary, resolve_binary
from .base import ToolResult

_GPG_CANDIDATES = ("gpg", "gpg2")
_VALID_MODES = ("encrypt", "decrypt", "sign", "verify")


@dataclass(frozen=True)
class GpgPlugin:
    name: str = "gpg"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        mode = str(params.get("mode") or "encrypt").lower()
        if mode not in _VALID_MODES:
            raise ValueError(
                f"gpg: mode must be one of {_VALID_MODES}, got {mode!r}"
            )

        binary = resolve_binary(_GPG_CANDIDATES)
        if binary is None:
            raise RuntimeError("gpg: binary not found on PATH.")

        from_path = bool(params.get("from_path"))
        passphrase = params.get("passphrase")
        cmd: list[str] = [binary, "--batch", "--yes", "--quiet"]
        if passphrase:
            cmd += ["--pinentry-mode", "loopback", "--passphrase", str(passphrase)]
        if params.get("armor", True) and mode in ("encrypt", "sign"):
            cmd.append("--armor")

        if mode == "encrypt":
            recipient = params.get("recipient")
            if not isinstance(recipient, str) or not recipient:
                raise ValueError("gpg encrypt: params['recipient'] required.")
            cmd += ["--recipient", recipient, "--encrypt"]
        elif mode == "decrypt":
            cmd += ["--decrypt"]
        elif mode == "sign":
            signer = params.get("signer")
            if isinstance(signer, str) and signer:
                cmd += ["--local-user", signer]
            cmd += ["--detach-sign"] if params.get("detached") else ["--sign"]
        else:  # verify
            sig = params.get("signature")
            if not isinstance(sig, str) or not sig:
                raise ValueError("gpg verify: params['signature'] required.")

        output_path = params.get("output")
        if isinstance(output_path, str) and output_path:
            cmd += ["--output", output_path]

        # Build subprocess invocation. For verify we need to manage
        # signature + signed data separately; for the others the input
        # comes via stdin or as a file argument.
        try:
            if mode == "verify":
                return _run_verify(
                    binary=binary,
                    base_cmd=cmd,
                    params=params,
                    from_path=from_path,
                    timeout_seconds=timeout_seconds,
                )

            if from_path:
                src = params.get("input")
                if not isinstance(src, str) or not src:
                    raise ValueError(
                        "gpg: from_path=True requires params['input'] path."
                    )
                cmd.append(str(Path(src).expanduser()))
                proc = subprocess.run(
                    cmd, capture_output=True, text=True,
                    timeout=int(timeout_seconds), check=False,
                )
            else:
                payload = params.get("input")
                if payload is None:
                    payload = ""
                if not isinstance(payload, str):
                    raise ValueError(
                        "gpg in-memory mode: params['input'] must be str."
                    )
                proc = subprocess.run(
                    cmd, capture_output=True, text=True,
                    timeout=int(timeout_seconds), input=payload, check=False,
                )
        except subprocess.TimeoutExpired as exc:
            raise RuntimeError(
                f"gpg exceeded timeout of {timeout_seconds}s"
            ) from exc

        return _build_result(
            mode=mode, params=params, cmd=cmd, proc=proc, passphrase=passphrase
        )

    def check(self) -> CheckResult:
        return check_binary(_GPG_CANDIDATES, label="gpg")


def _run_verify(
    *,
    binary: str,
    base_cmd: list[str],
    params: dict[str, Any],
    from_path: bool,
    timeout_seconds: int,
) -> ToolResult:
    """``gpg --verify`` takes the signature path as the first positional
    argument and (optionally) the signed-data path as the second.

    For the in-memory case we materialise the inputs to temp files
    because ``gpg`` won't accept both via stdin simultaneously.
    """
    sig = params["signature"]
    payload = params.get("input")
    cmd = base_cmd + ["--verify"]

    with tempfile.TemporaryDirectory() as tmp_dir:
        if from_path:
            sig_path = str(Path(sig).expanduser())
            data_path = str(Path(payload).expanduser()) if isinstance(payload, str) else None
        else:
            sig_path = str(Path(tmp_dir) / "sig.asc")
            Path(sig_path).write_text(str(sig), encoding="utf-8")
            data_path = str(Path(tmp_dir) / "data.txt")
            Path(data_path).write_text(
                str(payload) if payload is not None else "", encoding="utf-8"
            )
        cmd.append(sig_path)
        if data_path:
            cmd.append(data_path)
        proc = subprocess.run(
            cmd, capture_output=True, text=True,
            timeout=int(timeout_seconds), check=False,
        )
    # gpg --verify exits 0 on good sig, non-zero on bad. We surface this
    # as exit_code rather than raising — orchestration decides.
    return ToolResult(
        value=proc.returncode == 0,
        raw={"args": cmd[1:], "binary": binary},
        stdout=proc.stdout,
        stderr=proc.stderr,
        exit_code=proc.returncode,
    )


def _build_result(
    *,
    mode: str,
    params: dict[str, Any],
    cmd: list[str],
    proc: subprocess.CompletedProcess[str],
    passphrase: Any,
) -> ToolResult:
    if proc.returncode != 0:
        # Mask passphrase out of error context.
        masked_cmd = " ".join(shlex.quote(c) for c in cmd)
        if passphrase:
            masked_cmd = masked_cmd.replace(str(passphrase), "***")
        err = (proc.stderr or proc.stdout or "").strip()
        if passphrase:
            err = err.replace(str(passphrase), "***")
        raise RuntimeError(f"gpg failed (exit {proc.returncode}): {err}")

    output_path = params.get("output")
    if isinstance(output_path, str) and output_path:
        # Output went to file — surface the path.
        value: Any = output_path
    else:
        value = proc.stdout

    return ToolResult(
        value=value,
        raw={"mode": mode, "args": cmd[1:]},
        stdout=proc.stdout,
        stderr=proc.stderr,
        exit_code=proc.returncode,
    )
