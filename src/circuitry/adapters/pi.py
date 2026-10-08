from __future__ import annotations

import os
import tempfile
from dataclasses import dataclass

from ..agent_cli import (
    DEFAULT_UNSET_ENV,
    child_env,
    parse_pi_output,
    pi_command,
    run_agent_cli,
)
from ..preflight import CheckResult
from ._agent_cli import agent_cli_call_errors, check_binary, generate_result
from .base import GenerateOptions, GenerateResult


@dataclass(frozen=True)
class PiAdapter:
    """One completion through the ``pi`` coding-agent CLI and its own login.

    Runs ``pi -p --mode json --no-session --no-tools --no-approve`` with the
    prompt in a temporary file attached as ``@<file>`` (pi reads no prompt
    from stdin), in a fresh temporary directory so no project's context
    files are picked up. ``model`` is pi's ``provider/id``; ``thinking`` its
    thinking level. Generation options pi has no flag for are ignored, with
    a warning.
    """

    name: str = "pi"
    binary: str = "pi"
    default_model: str = ""
    thinking: str = ""
    unset_env: tuple[str, ...] = DEFAULT_UNSET_ENV
    extra_args: tuple[str, ...] = ()

    def generate(
        self,
        *,
        model: str,
        prompt: str,
        timeout_seconds: int = 120,
        options: GenerateOptions | None = None,
    ) -> GenerateResult:
        with tempfile.TemporaryDirectory(prefix="circuitry-pi-") as workdir:
            prompt_file = os.path.join(workdir, "prompt.md")
            with open(prompt_file, "w", encoding="utf-8") as handle:
                handle.write(prompt)
            cmd = pi_command(
                binary=self.binary,
                prompt_file=prompt_file,
                model=model or self.default_model,
                thinking=self.thinking,
                extra_args=self.extra_args,
            )
            with agent_cli_call_errors():
                proc = run_agent_cli(
                    cmd,
                    env=child_env(self.unset_env),
                    timeout_seconds=timeout_seconds,
                    cwd=workdir,
                )
                result = parse_pi_output(proc)
        return generate_result(self.name, result, options)

    def check(self) -> CheckResult:
        return check_binary(self.name, self.binary)
