from __future__ import annotations

import tempfile
from dataclasses import dataclass

from ..agent_cli import (
    DEFAULT_UNSET_ENV,
    child_env,
    claude_command,
    parse_claude_output,
    run_agent_cli,
)
from ..preflight import CheckResult
from ._agent_cli import agent_cli_call_errors, check_binary, generate_result
from .base import GenerateOptions, GenerateResult


@dataclass(frozen=True)
class ClaudeCodeAdapter:
    """One completion through the Claude Code CLI (``claude``) and its own login.

    Runs ``claude -p --output-format json --no-session-persistence --tools ""``
    with the prompt on stdin, in a fresh temporary directory so no project's
    ``CLAUDE.md`` is picked up. A JSON prompt with a ``schema`` adds
    ``--json-schema`` and takes Claude Code's ``structured_output`` as the
    answer. Tokens and cost come from Claude Code's result. Generation
    options Claude Code has no flag for are ignored, with a warning.
    """

    name: str = "claude_code"
    binary: str = "claude"
    default_model: str = ""
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
        cmd = claude_command(
            binary=self.binary,
            model=model or self.default_model,
            json_schema=options.json_schema if options is not None else None,
            extra_args=self.extra_args,
        )
        with tempfile.TemporaryDirectory(prefix="circuitry-claude-") as workdir:
            with agent_cli_call_errors():
                proc = run_agent_cli(
                    cmd,
                    env=child_env(self.unset_env),
                    timeout_seconds=timeout_seconds,
                    input=prompt,
                    cwd=workdir,
                )
                result = parse_claude_output(proc)
        return generate_result(self.name, result, options)

    def check(self) -> CheckResult:
        return check_binary(self.name, self.binary)
