"""Shared subprocess harness for the conformance suite: loads a case's
`case.json` and runs `cof run` (as `python -m circuitry.cli.app run`, the
same invocation `tests/cli/test_run_cancel_parallel.py` uses for a hermetic
CLI subprocess) with a temporary `HOME`, no credential env vars, no
`CIRCUITRY_*` env vars (CLAUDE.md's hermeticity rule — `CIRCUITRY_CONFIG`,
`CIRCUITRY_MODEL`, `CIRCUITRY_ADAPTER` and friends all change
`effective_settings` if the caller's shell happens to export one), the
case's own `fakes/` directory first on `PATH` if present, and cwd set to the
case directory. Used by both `scripts/generate-conformance-cases.py` and
the Python/electricity pytest runners, so generation and verification can
never drift apart on *how* a case is run — only on what's done with the
result.
"""

from __future__ import annotations

import contextlib
import json
import os
import re
import subprocess
import sys
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from circuitry.adapters.scripted import (
    LEFTOVER_REPLIES_EXPORT_ENV_VAR,
    total_replies_by_path,
)

from .mock_http import MockHttpServer, RecordedRequest
from .normalize import (
    assert_states_equal,
    canonicalize_http_requests,
    normalize_for_comparison,
)

CASES_DIR = Path(__file__).resolve().parent / "cases"

#: Substituted, verbatim, for the mock HTTP server's real ephemeral port
#: wherever a case's own `config` file content or `cli_args` need to name
#: it -- an adapter's `base_url` (config) or a `-e` input an `http` tool
#: URL template reads (cli_args). The harness materializes both with the
#: real port right before invoking either engine; see `run_case` and
#: `materialize_cli_args` below.
MOCK_HTTP_PORT_PLACEHOLDER = "__MOCK_HTTP_PORT__"

#: Substituted, verbatim, for the case directory's own absolute path in a
#: case's `cli_args` -- a `-e` input that must name a directory as an
#: absolute path (a prompt's `within:`, issue #578) can't be committed with
#: one. Both engines run with that directory as `cwd`; the expected state
#: carries it as `<CASE_DIR>` (`case_redaction_replacements`).
CASE_DIR_PLACEHOLDER = "__CASE_DIR__"

#: Mirrors `tests/cli/test_run_cancel_parallel.py`'s `_CREDENTIAL_ENV_VARS` —
#: a case subprocess must never see a live credential, real or scripted.
#: `_sandboxed_env` additionally drops every `CIRCUITRY_*` variable
#: (`CIRCUITRY_CONFIG`, `CIRCUITRY_MODEL`, ... — see `config.py`'s
#: `CONFIG_ENV_VARS`/`_apply_env_vars`), since those aren't credentials but
#: still change `effective_settings` if set in the caller's shell.
#:
#: Grown in issue #503 (lane F2) to cover every real provider's own
#: credential variable (the issue's own "Environment variables" table) —
#: before that lane, only `openai`/`anthropic` were real adapters, so a
#: contributor's own exported `GROQ_API_KEY` (say) could never have
#: reached a case; now that the whole family is buildable, it could.
CREDENTIAL_ENV_VARS = (
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    # `claude_code`/`pi` (issue #520): `agent_cli.py::DEFAULT_UNSET_ENV`'s
    # own second name -- a case that needs the CLI to actually see a
    # value sets it through `fake_credentials` like any other provider
    # credential, never inherited from the caller's shell.
    "ANTHROPIC_AUTH_TOKEN",
    "GOOGLE_API_KEY",
    "GROQ_API_KEY",
    "OPENROUTER_API_KEY",
    "PERPLEXITY_API_KEY",
    "XAI_API_KEY",
    "DEEPSEEK_API_KEY",
    "TOGETHER_API_KEY",
    "FIREWORKS_API_KEY",
    "MISTRAL_API_KEY",
    "AI21_API_KEY",
    "COHERE_API_KEY",
    "HF_TOKEN",
    "DASHSCOPE_API_KEY",
    "NIM_API_KEY",
    "DATABRICKS_TOKEN",
    "DATABRICKS_HOST",
    "CF_API_TOKEN",
    "CF_ACCOUNT_ID",
    "AZURE_OPENAI_API_KEY",
    "AZURE_OPENAI_ENDPOINT",
    "REPLICATE_API_TOKEN",
    "WATSONX_API_KEY",
    "WATSONX_PROJECT_ID",
    "WATSONX_REGION",
    "WATSONX_API_VERSION",
    "CYBERDINER_TOKEN",
    "CYBERDINER_EXPO_URL",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "NPM_TOKEN",
)

#: `case.json`'s own `fake_credentials` key (issue #503): the marker every
#: synthetic credential value must start with, so a conformance case can
#: never accidentally carry a real-looking one — and so a reviewer can
#: `grep` for it to confirm a committed fixture/expected file never leaked
#: an operator's own key.
FAKE_CREDENTIAL_MARKER = "circuitry-test-fake-"

#: tools-adapters-plugins.md §7 case 22 (the redaction blanket), run as a
#: post-check on every case (issue #448's acceptance criterion, closed by
#: #537): a real-looking credential's own prefix -- a live key never
#: belongs in a conformance run's own output at all, redacted or not.
#: `FAKE_CREDENTIAL_MARKER` and a case's own `fake_credentials` values are
#: checked the same way, for the same reason (a scripted-looking
#: credential leaking somewhere a case never meant it to).
_LEAKED_CREDENTIAL_PREFIX = "sk-live-"


def assert_no_leaked_credentials(
    texts: dict[str, str], *, case_name: str, fake_credentials: dict[str, str] | None = None
) -> None:
    """Case 22's own blanket check, run on every case (not just
    `c-http-redaction-blanket`) after every run: none of *texts* (keyed by
    a label naming where each came from -- stdout, stderr, `--out`,
    `--events`, `--live-state`) may contain `_LEAKED_CREDENTIAL_PREFIX`,
    `FAKE_CREDENTIAL_MARKER`, or any of the case's own `fake_credentials`
    values. `expected.http_requests.json` (what the mock server actually
    received) is deliberately never one of *texts* -- nine cases record a
    real-looking credential there on purpose (the point of the header/URL
    redaction those cases check: display-only, never a what-gets-sent
    change), so it is the one exempt file (see the acceptance map's own
    case-22 row in `tests/conformance/README.md`)."""
    patterns = [_LEAKED_CREDENTIAL_PREFIX, FAKE_CREDENTIAL_MARKER]
    if fake_credentials:
        patterns.extend(fake_credentials.values())
    for label, text in texts.items():
        for pattern in patterns:
            if pattern in text:
                raise AssertionError(
                    f"{case_name}: {label} contains a credential-shaped string "
                    f"({pattern!r}) -- case 22's own blanket check"
                )


def run_result_texts(
    result: CaseResult, *, events_path: Path | None = None, live_state_path: Path | None = None
) -> dict[str, str]:
    """stdout/stderr plus `--out`/`--events`/`--live-state`'s own content,
    for [`assert_no_leaked_credentials`] -- a file that was never written
    (a load/check failure before any effect ever dispatches writes no
    `--events`/`--live-state` at all) is simply omitted, not an empty
    string, so a label only appears when there is real content to check."""
    texts = {"stdout": result.stdout, "stderr": result.stderr}
    if result.out_path.exists():
        texts["--out"] = result.out_path.read_text(encoding="utf-8")
    if events_path is not None and events_path.exists():
        texts["--events"] = events_path.read_text(encoding="utf-8")
    if live_state_path is not None and live_state_path.exists():
        texts["--live-state"] = live_state_path.read_text(encoding="utf-8")
    return texts

#: Every `http`/`https`/`ftp`/`all`-proxy variable, both cases (`getproxies_
#: environment`'s own convention, lowercase and uppercase alike) plus
#: `REQUEST_METHOD` (the CGI guard that only ever *removes* the plain
#: `http` entry, so it can't hide a real proxy on its own, but is dropped
#: here anyway for the same reason every other ambient variable is).
#: `_sandboxed_env` drops every one of these the caller's shell might
#: export, then sets `no_proxy=*` explicitly (issue #485, lane E's own
#: network-isolation rule): a developer's real proxy settings must never
#: change an `http`-tool conformance case's own loopback request, on
#: either engine, and a bare `no_proxy=*` also makes CPython's own
#: `getproxies_environment()` return a non-empty dict, which suppresses
#: its macOS system-proxy fallback (`getproxies()` only ever falls back to
#: `getproxies_macosx_sysconf()` when the environment lookup is empty).
PROXY_ENV_VARS = (
    "http_proxy",
    "https_proxy",
    "ftp_proxy",
    "all_proxy",
    "no_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "FTP_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "REQUEST_METHOD",
)

DEFAULT_TIMEOUT_SECONDS = 30


@dataclass(frozen=True)
class CaseResult:
    returncode: int
    stdout: str
    stderr: str
    out_path: Path


def list_case_dirs() -> list[Path]:
    return sorted(p.parent for p in CASES_DIR.glob("*/case.json"))


#: `case.json`'s allowed top-level keys (`tests/conformance/README.md`'s table).
_CASE_JSON_KEYS = frozenset(
    {
        "spec_case",
        "description",
        "engines",
        "expect",
        "cli_args",
        "config",
        "also_pretty",
        "error_compare",
        "location_pattern",
        "mock_http_fixture",
        "sort_http_requests",
        "known_divergence",
        "electricity_preview_ok",
        "orchestration",
        "timeout_seconds",
        "sort_warnings",
        "env",
        "profile",
        "compare_run_warnings",
        "fake_credentials",
        "third_party_text",
        "python_runner_versions",
    }
)

#: `case.json`'s own `python_runner_versions` key -- a case whose expected
#: output depends on cof's own CEL nesting limits, which come from CPython's
#: recursion handling and differ between Python versions (3.12 in particular
#: changed it substantially), lists the exact `"major.minor"` version(s) its
#: `expected.json`/`expected.warnings.txt` were generated on. The Python
#: runner (`test_python_runner.py`) skips the case on any other interpreter;
#: the electricity runner ignores this key entirely, since electricity's own
#: caps are fixed constants, not derived from whatever interpreter is running
#: the test.
_PYTHON_VERSION_PATTERN = re.compile(r"[1-9][0-9]*\.[0-9]+")


def running_python_version() -> str:
    return f"{sys.version_info.major}.{sys.version_info.minor}"

#: `case.json`'s own `profile` key -- a path relative to the case
#: directory, of the form `profiles/<name>.yml` or `profiles/<name>.yaml`
#: (`load_case`'s own validation below).
_PROFILE_PATH_PATTERN = re.compile(r"profiles/([^/.]+)\.(yml|yaml)")
#: A case's own `case.json` 'env' key may not reintroduce exactly what
#: `_sandboxed_env`'s own filter above just stripped from the caller's
#: shell: one of its `CREDENTIAL_ENV_VARS` names, or any `CIRCUITRY_*`
#: name. This is a denylist of *exact* names, not a substring heuristic --
#: a synthetic name that merely *contains* a word like "API_KEY" (§7 case
#: 12's own `..._MY_API_KEY_SUFFIX`, there to exercise `env_vars`' own
#: secret-masking pattern) is not itself a credential and stays allowed.
def _is_safe_case_env_key(key: str) -> bool:
    if key.upper().startswith("CIRCUITRY_"):
        return False
    return key not in CREDENTIAL_ENV_VARS


_VALID_EXPECT = frozenset({"success", "failure"})
_VALID_ERROR_COMPARE = frozenset({"exact", "location"})
_VALID_ENGINES = frozenset({"python", "electricity"})
#: `known_divergence`'s only two keys (`tests/conformance/README.md`'s table):
#: the dotted-path location in the captured state, and the value electricity
#: is documented to produce there instead.
_KNOWN_DIVERGENCE_KEYS = frozenset({"location", "electricity_value"})


def load_case(case_dir: Path) -> dict[str, Any]:
    metadata: dict[str, Any] = json.loads((case_dir / "case.json").read_text(encoding="utf-8"))

    unknown_keys = set(metadata) - _CASE_JSON_KEYS
    if unknown_keys:
        raise ValueError(
            f"{case_dir.name}: case.json has unknown key(s): {sorted(unknown_keys)}"
        )

    metadata.setdefault("orchestration", "orchestration.yml")
    metadata.setdefault("cli_args", [])
    metadata.setdefault("engines", ["python"])
    metadata.setdefault("config", None)
    metadata.setdefault("mock_http_fixture", None)
    metadata.setdefault("sort_http_requests", False)
    metadata.setdefault("known_divergence", None)
    metadata.setdefault("electricity_preview_ok", False)
    metadata.setdefault("timeout_seconds", DEFAULT_TIMEOUT_SECONDS)
    metadata.setdefault("sort_warnings", False)
    metadata.setdefault("env", None)
    metadata.setdefault("profile", None)
    metadata.setdefault("compare_run_warnings", False)
    metadata.setdefault("fake_credentials", {})
    metadata.setdefault("third_party_text", [])
    metadata.setdefault("python_runner_versions", None)

    env = metadata["env"]
    if env is not None and (
        not isinstance(env, dict)
        or not all(isinstance(k, str) and isinstance(v, str) for k, v in env.items())
    ):
        raise ValueError(f"{case_dir.name}: case.json 'env' must be a mapping of str to str")
    if env is not None:
        bad_keys = sorted(k for k in env if not _is_safe_case_env_key(k))
        if bad_keys:
            raise ValueError(
                f"{case_dir.name}: case.json 'env' key(s) {bad_keys} look like a credential "
                "or a CIRCUITRY_* config override -- a case subprocess must never see a live "
                "credential, and `_sandboxed_env` already drops every CIRCUITRY_* variable "
                "the caller's own shell might export, so 'env' may not reintroduce one"
            )

    if not isinstance(metadata["electricity_preview_ok"], bool):
        raise ValueError(f"{case_dir.name}: case.json 'electricity_preview_ok' must be a bool")
    if not isinstance(metadata["sort_http_requests"], bool):
        raise ValueError(f"{case_dir.name}: case.json 'sort_http_requests' must be a bool")

    if "expect" not in metadata:
        raise ValueError(f"{case_dir.name}: case.json is missing the required 'expect' key")
    if metadata["expect"] not in _VALID_EXPECT:
        raise ValueError(
            f"{case_dir.name}: case.json 'expect' must be one of {sorted(_VALID_EXPECT)}, "
            f"got {metadata['expect']!r}"
        )
    engines = metadata["engines"]
    if not isinstance(engines, list) or not engines or set(engines) - _VALID_ENGINES:
        raise ValueError(
            f"{case_dir.name}: case.json 'engines' must be a non-empty list drawn from "
            f"{sorted(_VALID_ENGINES)}, got {engines!r}"
        )
    if "error_compare" in metadata and metadata["error_compare"] not in _VALID_ERROR_COMPARE:
        raise ValueError(
            f"{case_dir.name}: case.json 'error_compare' must be one of "
            f"{sorted(_VALID_ERROR_COMPARE)}, got {metadata['error_compare']!r}"
        )
    if metadata.get("also_pretty") and metadata["expect"] != "success":
        raise ValueError(
            f"{case_dir.name}: case.json 'also_pretty' only applies to a 'success' case"
        )
    if not isinstance(metadata["sort_warnings"], bool):
        raise ValueError(
            f"{case_dir.name}: case.json 'sort_warnings' must be a bool, "
            f"got {metadata['sort_warnings']!r}"
        )
    if not isinstance(metadata["compare_run_warnings"], bool):
        raise ValueError(
            f"{case_dir.name}: case.json 'compare_run_warnings' must be a bool, "
            f"got {metadata['compare_run_warnings']!r}"
        )
    fake_credentials = metadata["fake_credentials"]
    if not isinstance(fake_credentials, dict) or not all(
        isinstance(k, str) and isinstance(v, str) for k, v in fake_credentials.items()
    ):
        raise ValueError(
            f"{case_dir.name}: case.json 'fake_credentials' must be a mapping of str to str"
        )
    bad_names = sorted(k for k in fake_credentials if k not in CREDENTIAL_ENV_VARS)
    if bad_names:
        raise ValueError(
            f"{case_dir.name}: case.json 'fake_credentials' key(s) {bad_names} are not in "
            "CREDENTIAL_ENV_VARS -- only a real provider credential variable belongs here"
        )
    bad_values = sorted(
        k for k, v in fake_credentials.items() if not v.startswith(FAKE_CREDENTIAL_MARKER)
    )
    if bad_values:
        raise ValueError(
            f"{case_dir.name}: case.json 'fake_credentials' value(s) for {bad_values} must "
            f"start with {FAKE_CREDENTIAL_MARKER!r}"
        )
    third_party_text = metadata["third_party_text"]
    if not isinstance(third_party_text, list) or not all(
        isinstance(spec, dict) and set(spec) == {"location", "pattern"}
        and isinstance(spec["location"], str)
        and spec["location"]
        and isinstance(spec["pattern"], str)
        and spec["pattern"]
        for spec in third_party_text
    ):
        raise ValueError(
            f"{case_dir.name}: case.json 'third_party_text' must be a list of "
            "{'location': <non-empty str>, 'pattern': <non-empty str>} mappings"
        )
    python_runner_versions = metadata["python_runner_versions"]
    if python_runner_versions is not None and (
        not isinstance(python_runner_versions, list)
        or not python_runner_versions
        or not all(
            isinstance(v, str) and _PYTHON_VERSION_PATTERN.fullmatch(v)
            for v in python_runner_versions
        )
    ):
        raise ValueError(
            f"{case_dir.name}: case.json 'python_runner_versions' must be a non-empty list of "
            "'major.minor' strings like '3.11'"
        )

    profile_rel = metadata["profile"]
    if profile_rel is not None:
        if not isinstance(profile_rel, str):
            raise ValueError(f"{case_dir.name}: case.json 'profile' must be a string path")
        match = _PROFILE_PATH_PATTERN.fullmatch(profile_rel)
        if match is None:
            raise ValueError(
                f"{case_dir.name}: case.json 'profile' must look like 'profiles/<name>.yml' "
                f"or 'profiles/<name>.yaml', got {profile_rel!r}"
            )
        stem, suffix = match.group(1), match.group(2)
        other_suffix = "yaml" if suffix == "yml" else "yml"
        named_path = case_dir / profile_rel
        other_path = case_dir / "profiles" / f"{stem}.{other_suffix}"
        if not named_path.is_file():
            raise ValueError(
                f"{case_dir.name}: case.json 'profile' names {profile_rel}, which doesn't exist"
            )
        if other_path.is_file():
            raise ValueError(
                f"{case_dir.name}: case.json 'profile' names {profile_rel}, but "
                f"{other_path.name} also exists -- cof run's own discovery tries .yml "
                "before .yaml, which would silently pick a different file than "
                "electricity's own --profile <path> names"
            )
    if metadata.get("also_pretty") and metadata.get("mock_http_fixture"):
        raise ValueError(
            f"{case_dir.name}: case.json 'also_pretty' cannot combine with "
            "'mock_http_fixture' -- the --pretty re-run gets neither a fresh mock server "
            "nor a fresh leftover-replies path, and runs after the server has already "
            "stopped"
        )
    if metadata.get("electricity_preview_ok") and "electricity" not in metadata["engines"]:
        raise ValueError(
            f"{case_dir.name}: case.json 'electricity_preview_ok' only applies to a "
            "case that lists 'electricity' in 'engines' -- it names a case electricity "
            "is expected to refuse, so there is nothing for it to mean otherwise"
        )
    known_divergence = metadata["known_divergence"]
    if known_divergence is not None:
        if (
            not isinstance(known_divergence, dict)
            or set(known_divergence) != _KNOWN_DIVERGENCE_KEYS
        ):
            raise ValueError(
                f"{case_dir.name}: case.json 'known_divergence' must be a mapping with "
                f"exactly {sorted(_KNOWN_DIVERGENCE_KEYS)}"
            )
        if not isinstance(known_divergence["location"], str) or not known_divergence["location"]:
            raise ValueError(
                f"{case_dir.name}: case.json 'known_divergence.location' must be a "
                "non-empty string"
            )
        if metadata.get("also_pretty"):
            raise ValueError(
                f"{case_dir.name}: case.json 'known_divergence' cannot combine with "
                "'also_pretty' -- the --pretty comparison never applies the override, so "
                "a documented divergence there would just fail the --pretty assertion"
            )
        if metadata["expect"] != "success":
            raise ValueError(
                f"{case_dir.name}: case.json 'known_divergence' only applies to a "
                "'success' case -- a documented divergence is by definition a case "
                "both engines succeed on"
            )
        if set(engines) != _VALID_ENGINES:
            raise ValueError(
                f"{case_dir.name}: case.json 'known_divergence' requires 'engines' to "
                f"list both {sorted(_VALID_ENGINES)} -- there is nothing to diverge "
                "from with only one engine running"
            )
    return metadata


def _sandboxed_env(
    case_dir: Path,
    home_dir: Path,
    *,
    leftover_replies_path: Path | None = None,
    extra_env: dict[str, str] | None = None,
    fake_credentials: dict[str, str] | None = None,
) -> dict[str, str]:
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in CREDENTIAL_ENV_VARS
        and k not in PROXY_ENV_VARS
        and not k.startswith("CIRCUITRY_")
    }
    env["HOME"] = str(home_dir)
    # Bypass every proxy explicitly, rather than merely clearing the
    # variables above -- a case subprocess's own loopback request to the
    # mock HTTP server must never depend on, or be routed through, a
    # proxy the caller's shell happened to export (issue #485, lane E's
    # own network-isolation rule).
    env["no_proxy"] = "*"
    fakes_dir = case_dir / "fakes"
    if fakes_dir.is_dir():
        env["PATH"] = f"{fakes_dir}{os.pathsep}{env.get('PATH', '')}"
    # A case's own `case.json` 'env' key (issue #460's own test plan: "if a
    # case needs a controlled variable, add an env key to case.json in the
    # harness") -- applied after every filter above, so a case can set a
    # name the filter would otherwise have dropped. `load_case` rejects any
    # key that looks like a credential or a `CIRCUITRY_*` override before a
    # case ever reaches this function, so `extra_env` is already known-safe
    # by the time it gets here.
    if extra_env:
        env.update(extra_env)
    # `case.json`'s own `fake_credentials` (issue #503): set after the
    # credential filter above, same as `extra_env`, for both engines --
    # `load_case` already rejected any name outside `CREDENTIAL_ENV_VARS`
    # or any value not carrying `FAKE_CREDENTIAL_MARKER`, so this is
    # already known-synthetic by the time it gets here. Without this, a
    # case that needs `openai`/`anthropic`/... to actually see a key (a
    # success case, or a 401) has no way to give it one at all, since the
    # filter above drops every real `CREDENTIAL_ENV_VARS` name first.
    if fake_credentials:
        env.update(fake_credentials)
    # Deliberately set *after* the CIRCUITRY_* filter above: a test-only
    # variable the harness itself injects for this one subprocess, not
    # inherited from the caller's shell (electricity/docs/spec/
    # scripted-replies.md §7) -- never a real config knob, so it is never
    # one of `config.py`'s CONFIG_ENV_VARS and can't change
    # `effective_settings`.
    if leftover_replies_path is not None:
        env[LEFTOVER_REPLIES_EXPORT_ENV_VAR] = str(leftover_replies_path)
    return env


def resolved_scripted_replies_path(case_dir: Path, metadata: dict[str, Any]) -> Path | None:
    """The case's own `runtime.adapters.scripted.replies_file`, resolved
    against the case directory, or `None` when the case's `config` doesn't
    set one (`config` is always JSON -- `cli.config.load_config`'s own
    format -- so this reads it the same way, never the materialized-port
    copy, since a replies-file name never carries the mock-port
    placeholder)."""
    config_name = metadata.get("config")
    if not config_name:
        return None
    config_data = json.loads((case_dir / config_name).read_text(encoding="utf-8"))
    replies_file = (
        ((config_data.get("runtime") or {}).get("adapters") or {}).get("scripted") or {}
    ).get("replies_file")
    if not replies_file:
        return None
    return case_dir / replies_file


def read_leftover_replies(path: Path, *, replies_file: Path | None = None) -> dict[str, int]:
    """The merged leftover-reply counts a run exported (scripted-replies.md
    §7), when the file is present. An **absent** file means no instance
    ever registered for it -- either nothing in the run ever loaded
    *replies_file*, or the engine running it doesn't implement the export at
    all -- so nothing was *consumed*, and every reply *replies_file* itself
    configures is left over; this is computed by parsing that file with the
    same loader the adapter uses (`total_replies_by_path`), never assumed to
    be `{}`. When *replies_file* is also `None` (the case's own config
    doesn't name a scripted replies file at all), there is nothing to check
    and an absent export file means exactly that: `{}`."""
    if path.exists():
        return dict(json.loads(path.read_text(encoding="utf-8")))
    if replies_file is None:
        return {}
    return total_replies_by_path(replies_file)


def assert_no_leftover_replies(
    leftover_replies_path: Path, *, case_dir: Path, metadata: dict[str, Any], case_name: str
) -> None:
    """A case that leaves a scripted reply unused fails here, on either
    engine (issue #450's acceptance criterion) -- over-provisioning a script
    is not itself a run failure (scripted-replies.md §6), but it is always a
    conformance-case bug: a fixture author who left dead replies behind, or
    a document that stopped calling a path the fixture still answers for.
    The one place both pytest runners and the generator make this check, so
    it can never drift into three slightly different copies of the same
    assertion."""
    replies_file = resolved_scripted_replies_path(case_dir, metadata)
    leftover = read_leftover_replies(leftover_replies_path, replies_file=replies_file)
    if leftover:
        raise AssertionError(
            f"{case_name}: scripted replies left over after the run: {leftover} "
            "-- remove them from the replies file or make the document consume them"
        )


def materialize_cli_args(
    cli_args: list[Any], *, mock_http_port: int | None, case_dir: Path | None = None
) -> list[str]:
    """`cli_args` as given, or with `MOCK_HTTP_PORT_PLACEHOLDER` substituted
    for the mock server's real port, and `CASE_DIR_PLACEHOLDER` for
    *case_dir*, in every element -- the only way a `-e key=value` input can
    name the port an `http` tool URL needs, since the port is only known once
    the harness has actually started the server for this run."""
    args = [str(a) for a in cli_args]
    if case_dir is not None:
        args = [a.replace(CASE_DIR_PLACEHOLDER, str(case_dir)) for a in args]
    if mock_http_port is None:
        return args
    return [a.replace(MOCK_HTTP_PORT_PLACEHOLDER, str(mock_http_port)) for a in args]


def materialize_config(
    case_dir: Path, config_name: str, *, mock_http_port: int, tmp_dir: Path
) -> Path:
    """Write *config_name*'s own content, with `MOCK_HTTP_PORT_PLACEHOLDER`
    substituted for the mock server's real port, to *tmp_dir*, and return
    the new absolute path -- an adapter's `base_url` can only name the
    port this way, materialized fresh for each run since the port is
    different every time (electricity/DESIGN.md §12). Both runners and the
    generator call this (and `materialize_cli_args`) so generation and
    verification can never diverge on how the port reaches either engine.
    """
    text = (case_dir / config_name).read_text(encoding="utf-8")
    materialized = text.replace(MOCK_HTTP_PORT_PLACEHOLDER, str(mock_http_port))
    target = tmp_dir / f"materialized-{config_name}"
    target.write_text(materialized, encoding="utf-8")
    return target


def case_redaction_replacements(
    case_dir: Path, *, mock_http_port: int | None = None
) -> list[tuple[str, str]]:
    """`(literal, placeholder)` pairs for `normalize.redact_runtime_strings`,
    in the order they must be applied: the case's own `fakes/`
    subdirectory (a longer path than, and a prefix of, the case directory
    itself) before the case directory, then the mock server's own
    ephemeral port, if one is running for this case -- only in the
    `host:port` forms it can actually appear in (`127.0.0.1:<port>`, the
    loopback address `MockHttpServer` binds, and `localhost:<port>`, since
    a case's own `base_url`/URL input is free to spell the same loopback
    address either way), never as the bare port number on its own: a bare
    digit string would also match anywhere those same digits happen to
    occur inside a run id/UUID, a compact timestamp, or any other field
    with no relation to the mock server at all, corrupting it instead of a
    genuine port reference."""
    replacements: list[tuple[str, str]] = []
    fakes_dir = case_dir / "fakes"
    if fakes_dir.is_dir():
        replacements.append((str(fakes_dir), "<FAKES_DIR>"))
    replacements.append((str(case_dir), "<CASE_DIR>"))
    if mock_http_port is not None:
        port = str(mock_http_port)
        replacements.append((f"127.0.0.1:{port}", "127.0.0.1:<MOCK_PORT>"))
        replacements.append((f"localhost:{port}", "localhost:<MOCK_PORT>"))
    return replacements


def run_case(
    case_dir: Path,
    metadata: dict[str, Any],
    *,
    out_path: Path,
    home_dir: Path,
    pretty: bool = False,
    events_path: Path | None = None,
    live_state_path: Path | None = None,
    leftover_replies_path: Path | None = None,
    mock_http_port: int | None = None,
) -> CaseResult:
    """Run one case's document through `cof run`, writing state to
    `out_path`. `cwd` is the case directory, so `orchestration.yml` and any
    relative `config`/`fakes/` resolve the same way for every case.
    `mock_http_port`, when given, is substituted for
    `MOCK_HTTP_PORT_PLACEHOLDER` in both `cli_args` and (materialized to a
    fresh file under `out_path`'s own directory, never the committed
    `config.json` itself) the case's own `config`."""
    cmd = [
        sys.executable,
        "-m",
        "circuitry.cli.app",
        "run",
        metadata["orchestration"],
        "--out",
        str(out_path),
        "--quiet",
    ]
    if pretty:
        cmd.append("--pretty")
    if events_path is not None:
        cmd += ["--events", str(events_path)]
    if live_state_path is not None:
        cmd += ["--live-state", str(live_state_path)]
    config_name = metadata.get("config")
    if config_name:
        if mock_http_port is not None:
            config_path = materialize_config(
                case_dir, config_name, mock_http_port=mock_http_port, tmp_dir=out_path.parent
            )
            cmd += ["--config", str(config_path)]
        else:
            cmd += ["--config", config_name]
    profile_rel = metadata.get("profile")
    if profile_rel:
        # `cof run --profile <name>` discovers `profiles/<name>.yml`/
        # `.yaml` under the orchestration's own directory -- the case
        # directory, since `cwd` is set to it below and `orchestration`
        # is always a relative path (issue #495's own harness addition).
        profile_stem = profile_name_from_path(profile_rel)
        cmd += ["--profile", profile_stem]
    cmd += materialize_cli_args(
        metadata.get("cli_args", []), mock_http_port=mock_http_port, case_dir=case_dir
    )

    proc = subprocess.run(
        cmd,
        cwd=case_dir,
        env=_sandboxed_env(
            case_dir,
            home_dir,
            leftover_replies_path=leftover_replies_path,
            extra_env=metadata.get("env"),
            fake_credentials=metadata.get("fake_credentials"),
        ),
        capture_output=True,
        text=True,
        timeout=metadata.get("timeout_seconds", DEFAULT_TIMEOUT_SECONDS),
        check=False,
    )
    return CaseResult(proc.returncode, proc.stdout, proc.stderr, out_path)


#: Circuitry's own CLI logging handler (`cli/logging_setup.py`) formats
#: every record `{levelname}: {message}` -- always one of these five
#: (Python's own fixed `logging` level names), never `_print_run_
#: warnings`'s own `Warning:`/`Error:` lines, which don't share this
#: prefix shape at all (lowercase "arning"/"rror", a different
#: mechanism entirely -- see electricity/DESIGN.md §6.9's "CLI output").
_LOG_LEVEL_PREFIXES = ("DEBUG: ", "INFO: ", "WARNING: ", "ERROR: ", "CRITICAL: ")


def profile_name_from_path(profile_rel: str) -> str:
    """The profile's own stem -- `profiles/<name>.yml`/`.yaml` minus the
    `profiles/` prefix and the one trailing suffix, the name `cof run
    --profile <name>` itself expects."""
    return Path(profile_rel).stem


def run_warning_lines(stderr: str) -> list[str]:
    """`cli/app.py::_print_run_warnings`'s own `Warning: ...` lines on
    stderr (issue #495's own `compare_run_warnings` case key) -- preflight's
    soft warnings, image-asset warnings, and any other `RunResult.warnings`
    entry. Distinct from `warning_lines`' own `WARNING:`/`ERROR:` Python
    `logging`-module lines (issue #442): a run's own warnings print as
    `Warning: <text>` (capital `W`, no module-level prefix), on both
    engines -- `cli/app.py`'s own rich console strips its `[yellow]...
    [/yellow]` markup once stdout/stderr isn't a terminal, and electricity's
    own `print_stderr_contract` writes the identical `Warning: {text}`
    line for the same reason.
    """
    return [line for line in stderr.splitlines() if line.startswith("Warning: ")]


def warning_lines(stderr: str, *, sort: bool = False) -> list[str]:
    """The subset of *stderr*'s own lines that came from Circuitry's
    `logging` module (issue #442's "Warning lines on stderr" item) --
    `cli/dynamic.py`/`conditional.py`'s on_error/`finally:` degradation
    warnings, `cli/config.py`'s "Unknown environment" warning, and
    `cli/live_state.py`/`cli/events.py`'s mid-run write-failure warnings
    among them. A chain keeps `cof run`'s own real order; *sort* (a case
    whose document dispatches a tree `dynamic`, where two branches' own
    warnings can land in either relative order between runs) compares
    them as a sorted set instead -- `case.json`'s own `sort_warnings` key.
    """
    lines = [line for line in stderr.splitlines() if line.startswith(_LOG_LEVEL_PREFIXES)]
    return sorted(lines) if sort else lines


@contextlib.contextmanager
def mock_http_server(case_dir: Path, metadata: dict[str, Any]) -> Iterator[MockHttpServer | None]:
    """Start the case's own mock HTTP server (`case.json`'s
    `mock_http_fixture`) for the duration of the `with` block, or yield
    `None` when the case doesn't set one. Always stops the server, even
    on failure -- the one place that owns its lifecycle, used by both
    pytest runners and the generator so a case is scripted identically
    everywhere it runs."""
    fixture_name = metadata.get("mock_http_fixture")
    if fixture_name is None:
        yield None
        return
    server = MockHttpServer(case_dir / fixture_name)
    server.start()
    try:
        yield server
    finally:
        server.stop()


def assert_recorded_http_requests(
    case_dir: Path,
    metadata: dict[str, Any],
    recorded_requests: list[RecordedRequest] | None,
    *,
    replacements: list[tuple[str, str]],
) -> None:
    """Compare *recorded_requests* (captured from the case's own mock HTTP
    server, `None` when the case sets no `mock_http_fixture`) against the
    case's own committed `expected.http_requests.json` -- the one place
    both the Python and electricity runners make this comparison, so a gap
    in one (issue #450 item 3: "both engines' records must match") can
    never go unnoticed the way an inlined copy in only one runner did.
    Headers are compared case-insensitively on name and order-insensitively
    as a whole, values exactly (`normalize.canonicalize_http_requests`);
    requests themselves are compared in arrival order unless `case.json`
    sets `sort_http_requests: true` (a tree-flow case, whose concurrent
    branches may dispatch in a different wall-clock order on either
    engine)."""
    if recorded_requests is None:
        return
    expected_requests = json.loads(
        (case_dir / "expected.http_requests.json").read_text(encoding="utf-8")
    )
    actual_requests = [r.to_json() for r in recorded_requests]
    sort_requests = bool(metadata.get("sort_http_requests"))
    assert_states_equal(
        canonicalize_http_requests(
            normalize_for_comparison(actual_requests, replacements), sort_requests=sort_requests
        ),
        canonicalize_http_requests(
            normalize_for_comparison(expected_requests, replacements), sort_requests=sort_requests
        ),
    )


def parse_cli_error(stdout: str) -> str:
    """Pull the `error` field out of `cof run`'s failure-path JSON (printed
    to stdout on any non-zero exit, with or without `--json`)."""
    payload = json.loads(stdout)
    error = payload.get("error")
    if not isinstance(error, str):
        raise AssertionError(f"expected a string 'error' field in CLI output, got: {payload!r}")
    return error
