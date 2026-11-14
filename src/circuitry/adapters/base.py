from __future__ import annotations

import base64
import inspect
import logging
from collections.abc import Callable, Mapping
from dataclasses import dataclass, field, replace
from typing import TYPE_CHECKING, Any, Protocol

if TYPE_CHECKING:
    from ..preflight import CheckResult

logger = logging.getLogger(__name__)

#: Seed sent with ``deterministic: true`` by adapters whose provider takes
#: one, unless ``params`` names its own ``seed``.
DETERMINISTIC_SEED = 0

#: ``finish_reason`` values that mean the provider stopped at its token cap.
TRUNCATED_FINISH_REASONS = frozenset({"length", "max_tokens"})


@dataclass(frozen=True)
class GenerateResult:
    text: str
    raw: dict[str, Any]
    tokens_sent: int | None = None
    tokens_received: int | None = None
    #: Input tokens the provider read from its own cache / wrote to it,
    #: each set only when the provider's reply carries that count (even
    #: when it is 0) -- ``None`` otherwise, never a placeholder zero.
    #: Anthropic reports both; OpenAI-compatible providers report only
    #: ``tokens_cached_in`` (no write count); ``tokens_sent`` already
    #: counts both either way (see each adapter's own docstring).
    tokens_cached_in: int | None = None
    tokens_cache_write: int | None = None
    #: The provider's own stop reason (``stop``, ``length``, ``end_turn``,
    #: ``max_tokens``, ...), or ``None`` when it reports none.
    finish_reason: str | None = None
    #: Things the adapter could not honour, e.g. options it ignored.
    warnings: tuple[str, ...] = ()
    #: The call's cost in US dollars, when the provider reports one.
    cost_usd: float | None = None


@dataclass(frozen=True)
class GenerateProgress:
    """One delta of a streamed reply, handed to ``GenerateOptions.on_progress``.

    ``delta_chars`` is the number of reply characters this delta added;
    ``delta_count`` is always ``1`` (one call per provider chunk, so a
    caller counting calls gets a running chunk count for free).
    ``tokens_sent``/``tokens_received`` are the provider's own exact
    figures once it has reported them on this call (Anthropic's
    ``message_start`` reports ``tokens_sent`` before any text arrives;
    every streaming protocol here reports ``tokens_received`` only on its
    final event) — ``None`` until then.
    """

    delta_chars: int
    delta_count: int
    tokens_sent: int | None = None
    tokens_received: int | None = None


@dataclass(frozen=True)
class ChatMessage:
    """One conversation turn, already rendered."""

    role: str  # system | user | assistant | tool
    content: str


@dataclass(frozen=True)
class ImageInput:
    """An image for a vision model: the bytes of a local file, or a URL.

    Exactly one of ``data`` and ``url`` is set. ``media_type`` is known for
    local files (``image/png``, ...) and may be empty for a URL.
    """

    data: bytes | None = None
    url: str | None = None
    media_type: str = ""

    def base64_data(self) -> str:
        return base64.b64encode(self.data or b"").decode("ascii")

    def data_url(self) -> str:
        """``url`` as is, or the bytes as a ``data:`` URL."""
        if self.url is not None:
            return self.url
        return f"data:{self.media_type};base64,{self.base64_data()}"


@dataclass(frozen=True)
class GenerateOptions:
    """Per-call generation settings a prompt effect hands its adapter.

    ``temperature``, ``max_tokens`` and ``stop`` are the portable knobs each
    adapter maps to its provider's own names. ``params`` holds every other
    key of the effect's ``params:`` and goes to the provider unchanged, in
    the slot its API keeps them (ollama ``options``, the request body for
    OpenAI-style and Anthropic APIs). ``deterministic`` asks for a fixed seed
    where the provider takes one; the runtime has already turned it into
    ``temperature=0`` unless ``params`` set a temperature.

    ``messages`` are the effect's role-tagged turns; when present they replace
    the flattened ``prompt`` for adapters that can send real turns. ``images``
    go with the last user turn.

    ``json_schema`` is the effect's ``schema`` when it expects JSON, for an
    adapter that can ask its provider for output matching it
    (``claude_code``). The runtime validates the reply against it either
    way, so an adapter that ignores it is not warned about.

    ``on_progress``, when set, is called once per chunk a streaming adapter
    reads from its provider, with a :class:`GenerateProgress`. An adapter
    that does not stream a given call never calls it. It is not a "set
    field" for :meth:`set_fields` — streaming is not something an adapter
    can refuse and need warn about the way it warns about an ignored
    ``temperature`` or ``stop`` — but it does make :meth:`is_empty` return
    ``False``, the same as ``json_schema``, so ``call_generate`` still
    passes a non-empty ``options`` through to an adapter that accepts them.
    """

    temperature: float | None = None
    max_tokens: int | None = None
    stop: tuple[str, ...] = ()
    params: Mapping[str, Any] = field(default_factory=dict)
    deterministic: bool = False
    messages: tuple[ChatMessage, ...] = ()
    images: tuple[ImageInput, ...] = ()
    json_schema: Mapping[str, Any] | None = None
    #: Excluded from equality/repr (like a callback would be anywhere else
    #: in this codebase): two otherwise-identical ``GenerateOptions`` built
    #: with different closures over the same attempt must still compare
    #: equal, and a repr naming a closure's own address is never useful.
    on_progress: Callable[[GenerateProgress], None] | None = field(
        default=None, compare=False, repr=False
    )

    def set_fields(self) -> list[str]:
        """The fields carrying a value, named the way a warning shows them.

        ``deterministic`` is never listed: the runtime realises it as
        ``temperature``, and its seed is only sent where a provider has one.
        """
        names: list[str] = []
        if self.temperature is not None:
            names.append("temperature")
        if self.max_tokens is not None:
            names.append("max_tokens")
        if self.stop:
            names.append("stop")
        if self.params:
            names.append(f"params ({', '.join(sorted(self.params))})")
        if self.messages:
            names.append("messages (roles flattened into one prompt)")
        if self.images:
            names.append("images")
        return names

    def is_empty(self) -> bool:
        return (
            not self.set_fields()
            and not self.deterministic
            and self.json_schema is None
            and self.on_progress is None
        )


def ignored_options_warning(
    adapter_name: str, options: GenerateOptions | None, *, used: frozenset[str] = frozenset()
) -> tuple[str, ...]:
    """One warning naming the set fields of ``options`` outside ``used``.

    ``used`` holds bare field names (``"temperature"``, ``"messages"``, ...).
    Returns ``()`` when nothing was ignored.
    """
    if options is None:
        return ()
    ignored = [name for name in options.set_fields() if name.split(" ", 1)[0] not in used]
    if not ignored:
        return ()
    return (f"adapter '{adapter_name}' ignored: {'; '.join(ignored)}",)


def last_user_index(messages: list[dict[str, Any]]) -> int | None:
    """Index of the last ``user`` turn, where images are attached."""
    for index in range(len(messages) - 1, -1, -1):
        if messages[index].get("role") == "user":
            return index
    return None


class Adapter(Protocol):
    """What every adapter must provide.

    Two hooks are *optional* and therefore live outside this Protocol, so
    adapters written before they existed keep type-checking and running:

      * ``check() -> CheckResult`` — see :func:`circuitry.preflight.call_check`
      * ``list_models() -> list[str]`` — see
        :func:`circuitry.adapters.models.call_list_models` and the
        :class:`~circuitry.adapters.models.ModelLister` structural type

    Call both through their shims, never directly.

    ``generate()`` may also take an ``options: GenerateOptions | None``
    keyword. It is optional in the same sense: call ``generate`` through
    :func:`call_generate`, which passes ``options`` only to an adapter that
    accepts it. An adapter that can send images says so with a class-level
    ``accepts_images = True`` (see :func:`adapter_accepts_images`).
    """

    @property
    def name(self) -> str: ...

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult: ...

    def check(self) -> CheckResult: ...


def adapter_accepts_images(adapter: Any) -> bool:
    return getattr(adapter, "accepts_images", False) is True


def _accepts_options(generate: Any) -> bool:
    try:
        parameters = inspect.signature(generate).parameters.values()
    except (TypeError, ValueError):
        return False
    return any(
        p.name == "options" or p.kind is inspect.Parameter.VAR_KEYWORD for p in parameters
    )


def call_generate(
    adapter: Any,
    *,
    model: str,
    prompt: str,
    timeout_seconds: int,
    options: GenerateOptions | None = None,
) -> GenerateResult:
    """Invoke ``adapter.generate``, passing ``options`` only if it takes them.

    Backwards-compat shim, like :func:`~circuitry.preflight.call_check`: an
    adapter written before ``options`` existed is called exactly as before,
    and its result carries one warning naming what it never received.
    """
    if options is None or options.is_empty():
        return adapter.generate(model=model, prompt=prompt, timeout_seconds=timeout_seconds)
    if _accepts_options(adapter.generate):
        return adapter.generate(
            model=model, prompt=prompt, timeout_seconds=timeout_seconds, options=options
        )
    result = adapter.generate(model=model, prompt=prompt, timeout_seconds=timeout_seconds)
    warnings = ignored_options_warning(getattr(adapter, "name", "unknown"), options)
    if not warnings:
        return result
    if not isinstance(result, GenerateResult):
        # Nowhere to carry it; the log is all that is left.
        logger.warning("%s", warnings[0])
        return result
    return replace(result, warnings=(*result.warnings, *warnings))
