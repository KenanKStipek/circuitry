from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Protocol

if TYPE_CHECKING:
    from ..preflight import CheckResult


@dataclass(frozen=True)
class GenerateResult:
    text: str
    raw: dict[str, Any]
    tokens_sent: int | None = None
    tokens_received: int | None = None


class Adapter(Protocol):
    """What every adapter must provide.

    Two hooks are *optional* and therefore live outside this Protocol, so
    adapters written before they existed keep type-checking and running:

      * ``check() -> CheckResult`` — see :func:`circuitry.preflight.call_check`
      * ``list_models() -> list[str]`` — see
        :func:`circuitry.adapters.models.call_list_models` and the
        :class:`~circuitry.adapters.models.ModelLister` structural type

    Call both through their shims, never directly.
    """

    @property
    def name(self) -> str: ...

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult: ...

    def check(self) -> CheckResult: ...


