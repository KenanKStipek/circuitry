"""Secrets for ``http`` headers and subprocess tools' host ``env`` (#598).

A secret is host configuration, ``runtime.secrets.<name>``, one of:

- ``{keychain: {service, account?}}``: the macOS Keychain, read with
  ``security find-generic-password -w -s <service> [-a <account>]``;
- ``{command: [argv...]}``: the command's stdout, trimmed;
- ``{env: NAME}``: an environment variable of the ``cof`` process.

A document names one with ``{secret: <name>}`` (optionally ``prefix:``, a
literal text put before the value, such as ``"Bearer "``), only as a value in
an ``http`` effect's ``params.headers`` or in a subprocess tool's
``runtime.plugins.<name>.env``. It is resolved when the effect dispatches,
never earlier, and its value never reaches state, events, logs or an error:
:class:`SecretValues` replaces it, wherever a result or an error echoes it,
with ``[secret:<name>]``.
"""

from __future__ import annotations

import os
import re
import subprocess
from collections.abc import Iterator, Mapping
from dataclasses import dataclass, field
from typing import Any

from .core.cancellation import run_tracked

SECRET_KEY = "secret"
PREFIX_KEY = "prefix"

#: A secret's name: what ``runtime.secrets`` keys it by, and what a
#: placeholder shows.
_NAME_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_-]*$")

#: How long a ``keychain`` or ``command`` secret may take to print its value.
SECRET_COMMAND_TIMEOUT_SECONDS = 60

#: Characters a resolved value may not hold: it travels in an HTTP header or
#: an environment variable, where any of them would end it early.
_FORBIDDEN_CHARS = ("\r", "\n", "\0")

_SOURCES = ("keychain", "command", "env")


@dataclass(frozen=True)
class SecretRef:
    """A document's ``{secret: <name>, prefix?: <text>}``."""

    name: str
    prefix: str = ""

    @property
    def placeholder(self) -> str:
        """What every record shows in place of ``prefix`` plus the value."""
        return f"{self.prefix}[secret:{self.name}]"


class SecretError(RuntimeError):
    """A secret could not be resolved. The message names the secret, never
    its value or anything its command printed."""


def has_secret_key(value: Any) -> bool:
    """Whether *value* is a mapping with a ``secret`` key. In an ``http``
    header value or a host ``env`` value, where a reference is allowed, that
    is a reference, well-formed or not."""
    return isinstance(value, Mapping) and SECRET_KEY in value


def is_secret_ref(value: Any) -> bool:
    """Whether *value* reads as a reference anywhere else in a tool's params:
    a mapping whose keys are ``secret`` plus, optionally, ``prefix``, with a
    string ``secret``. The same rule as a ``{from: ...}`` leaf: any other
    mapping (a ``json`` body or a schema with a ``secret`` property, say) is
    passed through literally."""
    return (
        has_secret_key(value)
        and set(value) <= {SECRET_KEY, PREFIX_KEY}
        and isinstance(value[SECRET_KEY], str)
    )


def secret_ref_error(value: Any) -> str | None:
    """Why a mapping with a ``secret`` key is not a well-formed reference,
    or ``None`` when it is one."""
    if not isinstance(value, Mapping):
        return "must be a mapping"
    extra = sorted(str(key) for key in value if key not in (SECRET_KEY, PREFIX_KEY))
    if extra:
        return (
            f"has unknown key(s) {', '.join(repr(key) for key in extra)}; "
            "a secret reference has 'secret' and an optional 'prefix'"
        )
    name = value.get(SECRET_KEY)
    if not isinstance(name, str) or not _NAME_RE.match(name):
        return (
            "'secret' must name a runtime.secrets entry (letters, digits, '_' and '-', "
            f"not starting with a digit or '-'), got {name!r}"
        )
    prefix = value.get(PREFIX_KEY, "")
    if not isinstance(prefix, str):
        return f"'prefix' must be a string, got {type(prefix).__name__}"
    if any(ch in prefix for ch in _FORBIDDEN_CHARS):
        return "'prefix' must not contain a line break or a NUL"
    return None


def secret_ref(value: Any) -> SecretRef | None:
    """*value* as a :class:`SecretRef`, or ``None`` when it is not a
    well-formed ``{secret: <name>, prefix?: <text>}``."""
    if not has_secret_key(value) or secret_ref_error(value) is not None:
        return None
    return SecretRef(name=value[SECRET_KEY], prefix=value.get(PREFIX_KEY, ""))


def env_secret_refs(env: Any) -> dict[str, SecretRef]:
    """The variables of a ``runtime.plugins.<name>.env`` mapping whose value
    is a secret reference, by variable name."""
    if not isinstance(env, Mapping):
        return {}
    return {
        str(var): ref
        for var, value in env.items()
        if (ref := secret_ref(value)) is not None
    }


def runtime_env_secret_paths(runtime: Any) -> Iterator[tuple[str, Any]]:
    """``(runtime.plugins.<name>.env.<VAR>, value)`` for every value of a
    ``runtime:`` block's plugin ``env`` that is a secret reference,
    well-formed or not."""
    if not isinstance(runtime, Mapping):
        return
    plugins = runtime.get("plugins")
    if not isinstance(plugins, Mapping):
        return
    for plugin_name, cfg in plugins.items():
        env = cfg.get("env") if isinstance(cfg, Mapping) else None
        if not isinstance(env, Mapping):
            continue
        for var, value in env.items():
            if has_secret_key(value):
                yield f"runtime.plugins.{plugin_name}.env.{var}", value


def resolve_secret(name: str, runtime_config: Mapping[str, Any]) -> str:
    """The value of ``runtime.secrets.<name>``.

    Raises :class:`SecretError`, naming the secret, when it is not defined,
    is malformed, or its source fails or gives an empty or multi-line value.
    """
    secrets_cfg = runtime_config.get("secrets")
    entry = secrets_cfg.get(name) if isinstance(secrets_cfg, Mapping) else None
    if entry is None:
        raise SecretError(f"secret {name!r} is not defined in runtime.secrets.")
    label = f"runtime.secrets.{name}"
    if not isinstance(entry, Mapping) or len(entry) != 1 or next(iter(entry)) not in _SOURCES:
        raise SecretError(
            f"{label} must be exactly one of {{keychain: {{service, account?}}}}, "
            "{command: [argv...]} or {env: NAME}."
        )
    source, spec = next(iter(entry.items()))
    if source == "env":
        if not isinstance(spec, str) or not spec:
            raise SecretError(f"{label}.env must name an environment variable.")
        value = os.environ.get(spec)
        if value is None:
            raise SecretError(f"secret {name!r}: environment variable {spec!r} is not set.")
        return _checked(name, value)
    if source == "keychain":
        if not isinstance(spec, Mapping) or not isinstance(spec.get("service"), str):
            raise SecretError(f"{label}.keychain must be {{service, account?}} with a string service.")
        account = spec.get("account")
        if account is not None and not isinstance(account, str):
            raise SecretError(f"{label}.keychain.account must be a string.")
        unknown = sorted(str(key) for key in spec if key not in ("service", "account"))
        if unknown:
            raise SecretError(
                f"{label}.keychain has unknown key(s) {', '.join(repr(k) for k in unknown)}."
            )
        argv = ["security", "find-generic-password", "-w", "-s", spec["service"]]
        if account is not None:
            argv += ["-a", account]
        return _checked(name, _command_output(name, argv, what="the Keychain lookup"))
    if (
        not isinstance(spec, list)
        or not spec
        or not all(isinstance(arg, str) for arg in spec)
    ):
        raise SecretError(f"{label}.command must be a non-empty list of strings.")
    return _checked(name, _command_output(name, list(spec), what="its command"))


def _command_output(name: str, argv: list[str], *, what: str) -> str:
    try:
        proc = run_tracked(argv, timeout=SECRET_COMMAND_TIMEOUT_SECONDS)
    except FileNotFoundError:
        raise SecretError(
            f"secret {name!r}: {what} could not start ({argv[0]!r} was not found)."
        ) from None
    except subprocess.TimeoutExpired:
        raise SecretError(
            f"secret {name!r}: {what} took longer than {SECRET_COMMAND_TIMEOUT_SECONDS}s."
        ) from None
    except OSError as exc:
        raise SecretError(
            f"secret {name!r}: {what} could not start ({type(exc).__name__})."
        ) from None
    if proc.returncode != 0:
        raise SecretError(f"secret {name!r}: {what} exited with status {proc.returncode}.")
    return str(proc.stdout or "").strip()


def _checked(name: str, value: str) -> str:
    if not value:
        raise SecretError(f"secret {name!r} resolved to an empty value.")
    if any(ch in value for ch in _FORBIDDEN_CHARS):
        raise SecretError(f"secret {name!r} holds a line break or a NUL, which it cannot carry.")
    return value


@dataclass
class SecretValues:
    """The secrets one tool effect dispatch resolved, by name, and the
    replacements :meth:`scrub` makes for them."""

    values: dict[str, str] = field(default_factory=dict)
    #: ``(text, placeholder)`` pairs, longest text first, so a prefixed value
    #: is replaced whole before its bare value is.
    _replacements: list[tuple[str, str]] = field(default_factory=list)

    def resolve(self, ref: SecretRef, runtime_config: Mapping[str, Any]) -> str:
        """``ref.prefix`` plus the value of ``ref``'s secret, resolved once
        per dispatch."""
        value = self.values.get(ref.name)
        if value is None:
            value = resolve_secret(ref.name, runtime_config)
            self.values[ref.name] = value
            self._add(value, f"[secret:{ref.name}]")
        if ref.prefix:
            self._add(ref.prefix + value, ref.placeholder)
        return ref.prefix + value

    def _add(self, text: str, placeholder: str) -> None:
        if (text, placeholder) not in self._replacements:
            self._replacements.append((text, placeholder))
            self._replacements.sort(key=lambda pair: len(pair[0]), reverse=True)

    def __bool__(self) -> bool:
        return bool(self.values)

    def scrub(self, value: Any) -> Any:
        """*value* with every resolved secret replaced by its placeholder, in
        every string at any depth (mapping keys included)."""
        if not self._replacements:
            return value
        if isinstance(value, str):
            for text, placeholder in self._replacements:
                value = value.replace(text, placeholder)
            return value
        if isinstance(value, dict):
            return {self.scrub(key): self.scrub(item) for key, item in value.items()}
        if isinstance(value, list):
            return [self.scrub(item) for item in value]
        if isinstance(value, tuple):
            return tuple(self.scrub(item) for item in value)
        return value
