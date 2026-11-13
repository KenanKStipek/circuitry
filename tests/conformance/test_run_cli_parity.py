"""`cof run` and `electricity run` on the same command line (issue #728):
the same options (`-e`, `--config`, `--live-state`, `--out`, `--events`,
`--allow-capabilities`, `--adapter`, `--quiet`), the same config
discovery when `--config` is absent, the same capability-consent gate
for a document inside the library cache directory, and the same exit
code/message for a missing input, an unknown option, and a missing
document -- plus the legacy `electricity <config.json> <doc>` form
staying a working alias. Every comparison is exact (full stdout/stderr,
or the specific line/core DESIGN.md's own row says the rest of a frame
can't match), never a plain exit-code check or a substring look that a
build ignoring the option entirely could still pass.

Not a `tests/conformance/cases/` case (`harness`'s own per-case fixtures
hold a document/config pair and compare final state; several of these
invocations never reach a document load at all) -- same rationale as
`test_user_env_parity.py`, whose `electricity_binary` fixture and
sandboxed-environment helpers this module reuses the pattern of.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import pytest

from . import harness

ELECTRICITY_DIR = Path(__file__).resolve().parents[2] / "electricity"

#: A trivial, adapter-free document both engines already run identically.
ORCHESTRATION = (
    "effects:\n"
    "  - name: ok\n"
    "    type: tool\n"
    "    provider: json\n"
    "    params: {mode: parse, input: '{}'}\n"
)

#: Needs the `shell` capability (`plugins/capabilities.py::PLUGIN_CAPABILITIES`)
#: -- used to show neither engine gates an *ordinary* path run (outside
#: the library cache directory) on capability consent, with or without
#: `--allow-capabilities` (`cli/document_consent.py::enforce_consent`'s
#: own `gate_whole_document` is false there). A path resolving *inside*
#: the cache directory is the one exception (issue #337): see
#: `test_a_document_inside_the_library_cache_path_is_gated_on_both_engines`
#: below.
SHELL_ORCHESTRATION = (
    "effects:\n"
    "  - name: ok\n"
    "    type: tool\n"
    "    provider: shell\n"
    "    params: {command: 'true', allowed_commands: ['true']}\n"
)

#: A single slow `awk` step (issue #741/#751's own cross-engine `meta.live`
#: parity requirement) -- two 2.5s `system("sleep ...")` calls between
#: three `print`s, one of them to stderr. Both engines publish `meta.live`
#: at most once a second (plus one trailing publish after a second of
#: quiet); a 0.6s gap is shorter than that throttle window, so on cof's
#: own side (a separate reader thread per stream, racing the publisher's
#: own 1 s tick) it can land a first publish *after* `line2` has already
#: arrived, never showing `line1` alone at all -- 2.5s leaves a wide,
#: unambiguous margin past the throttle on both engines.
SLOW_AWK_ORCHESTRATION = (
    "effects:\n"
    "  - name: slow_awk\n"
    "    type: tool\n"
    "    provider: awk\n"
    "    params:\n"
    "      args:\n"
    '        - \'BEGIN{print "line1"; fflush(); system("sleep 2.5"); '
    'print "line2" > "/dev/stderr"; fflush("/dev/stderr"); '
    'system("sleep 2.5"); print "line3"}\'\n'
)

_CREDENTIAL_ENV_VARS = (
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "CYBERDINER_TOKEN",
    "CYBERDINER_EXPO_URL",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "NPM_TOKEN",
)


def _sandboxed_env(home_dir: Path, extra: dict[str, str] | None = None) -> dict[str, str]:
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in _CREDENTIAL_ENV_VARS and not k.startswith("CIRCUITRY_")
    }
    env["HOME"] = str(home_dir)
    env.update(extra or {})
    return env


def _real_last_run_snapshot() -> tuple[bool, float, bytes] | None:
    """The real `~/.config/circuitry/last-run.json` -- never the fake
    home every subprocess below gets via `_sandboxed_env`'s own `HOME`
    override, `test_service_parity.py`'s own identical rationale for its
    `services` directory: this account's own `cof`/`electricity` runs
    write this file right now, and this module must never touch it.
    `None` when the file doesn't exist.
    """
    real_path = Path.home() / ".config" / "circuitry" / "last-run.json"
    if not real_path.is_file():
        return None
    stat = real_path.stat()
    return (True, stat.st_mtime, real_path.read_bytes())


@pytest.fixture(autouse=True)
def _guard_against_the_real_last_run_stash() -> Any:
    """Fails the test outright, rather than silently touching it, if
    anything in this module ever resolves `~/.config/circuitry/
    last-run.json` under this account's real home -- every test here
    must pass its own sandboxed `HOME` (`_sandboxed_env`) to every
    `cof`/`electricity` subprocess it starts instead."""
    before = _real_last_run_snapshot()
    yield
    after = _real_last_run_snapshot()
    assert after == before, (
        "a test in this module touched the real "
        "~/.config/circuitry/last-run.json"
    )


def _write_case(case_dir: Path, orchestration: str = ORCHESTRATION) -> None:
    case_dir.mkdir()
    (case_dir / "orchestration.yml").write_text(orchestration, encoding="utf-8")
    (case_dir / "config.json").write_text("{}", encoding="utf-8")


def _run_cof(
    case_dir: Path, home_dir: Path, *, args: list[str], env: dict[str, str] | None = None,
    orchestration: str = ORCHESTRATION,
) -> subprocess.CompletedProcess[str]:
    _write_case(case_dir, orchestration)
    cmd = [sys.executable, "-m", "circuitry.cli.app", "run", *args]
    return subprocess.run(
        cmd, cwd=case_dir, env=_sandboxed_env(home_dir, env), capture_output=True, text=True,
        timeout=30, check=False,
    )


def _run_electricity(
    binary: Path, case_dir: Path, home_dir: Path, *, args: list[str],
    env: dict[str, str] | None = None, orchestration: str = ORCHESTRATION,
) -> subprocess.CompletedProcess[str]:
    _write_case(case_dir, orchestration)
    cmd = [str(binary), "run", *args]
    return subprocess.run(
        cmd, cwd=case_dir, env=_sandboxed_env(home_dir, env), capture_output=True, text=True,
        timeout=30, check=False,
    )


def _effective_settings(stdout: str) -> dict:
    """`runtime.effective_settings` out of a successful run's own
    stdout JSON (no `--out` given) -- the one place `--adapter`/
    `--allow-capabilities` are observable when nothing in the document
    itself needs either."""
    return json.loads(stdout)["runtime"]["effective_settings"]


def _no_such_option_core(stderr: str) -> str:
    """The `No such option: <flag>` core of a usage-error's own text --
    on `cof run`, inside its own Rich panel (possibly followed by
    ` (Possible options: ...)`, stripped here, and possibly wrapped
    across lines at a narrow terminal width, so this joins the whole
    stderr into one line before searching); on `electricity run`, the
    one line after the blank line in its own plain frame
    (`print_run_usage_error`). DESIGN.md's own row covers why the rest
    of either frame isn't compared."""
    # Rich colours the panel where it decides the output supports it
    # (CI runners do), so drop ANSI styling before searching.
    plain = re.sub(r"\x1b\[[0-9;]*m", "", stderr)
    joined = " ".join(plain.split())
    match = re.search(r"No such option: \S+", joined)
    assert match, f"no 'No such option: ...' text in stderr: {stderr!r}"
    return match.group(0)


@pytest.fixture(scope="module")
def electricity_binary() -> Path:
    """Builds the `electricity` binary once for this module -- same
    pattern as `test_electricity_runner.py`'s own session fixture
    (incremental, so a prior build in the same tree is cache-hit fast)."""
    if shutil.which("cargo") is None:
        pytest.skip("cargo not available; electricity binary cannot be built")
    proc = subprocess.run(
        ["cargo", "build", "-p", "electricity-cli", "--bin", "electricity"],
        cwd=ELECTRICITY_DIR, env=harness.cargo_env(), capture_output=True, text=True,
        timeout=600, check=False,
    )
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p electricity-cli failed:\n{proc.stdout}\n{proc.stderr}")
    binary = ELECTRICITY_DIR / "target" / "debug" / "electricity"
    assert binary.exists(), f"cargo build succeeded but {binary} is missing"
    return binary


def test_a_basic_run_succeeds_with_the_same_exit_code_and_state_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    args = ["orchestration.yml", "--config", "config.json"]

    cof_result = _run_cof(tmp_path / "cof_case", home_dir, args=args)
    electricity_result = _run_electricity(
        electricity_binary, tmp_path / "electricity_case", home_dir, args=args
    )

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr
    assert json.loads(cof_result.stdout)["prime"]["ok"]["value"] == {}
    assert json.loads(electricity_result.stdout)["prime"]["ok"]["value"] == {}


def test_allow_capabilities_adapter_and_quiet_are_accepted_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """None of the three options this issue adds change a plain path
    run's own document output on either engine (`SHELL_ORCHESTRATION`'s
    own docstring) -- but `--adapter`/`--allow-capabilities` are still
    observable in `runtime.effective_settings`, compared here so a
    build that silently drops either (deletes `adapter_override: ...`
    at the real call site, say) fails this test, not just a build that
    can't parse them at all."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    args = [
        "orchestration.yml", "--config", "config.json",
        "--allow-capabilities", "shell,network,shell",
        "--adapter", "claude_code",
        "--quiet",
    ]

    cof_result = _run_cof(tmp_path / "cof_case", home_dir, args=args)
    electricity_result = _run_electricity(
        electricity_binary, tmp_path / "electricity_case", home_dir, args=args
    )

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr

    cof_settings = _effective_settings(cof_result.stdout)
    electricity_settings = _effective_settings(electricity_result.stdout)
    assert cof_settings["adapter"] == "claude_code"
    assert electricity_settings["adapter"] == "claude_code"
    assert cof_settings["sources"]["adapter"] == "cli"
    assert electricity_settings["sources"]["adapter"] == "cli"
    # Deduplicated and sorted on both engines -- a repeated name must
    # not survive as a literal duplicate in state.
    assert cof_settings["runtime"]["_capability_allow"] == ["network", "shell"]
    assert electricity_settings["runtime"]["_capability_allow"] == ["network", "shell"]


def test_a_document_needing_a_capability_runs_without_allow_capabilities_on_either_engine(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """An *ordinary* path run (outside the library cache directory) is
    never gated on capability consent at all
    (`cli/document_consent.py::enforce_consent`'s own
    `gate_whole_document` is false for it): `--allow-capabilities`
    being absent never refuses `SHELL_ORCHESTRATION` on `cof run`, so
    the same is true, by construction, of `electricity run`. The one
    exception -- a path resolving *inside* the cache directory -- is
    `test_a_document_inside_the_library_cache_path_is_gated_on_both_engines`
    below."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    args = ["orchestration.yml", "--config", "config.json"]

    cof_result = _run_cof(
        tmp_path / "cof_case", home_dir, args=args, orchestration=SHELL_ORCHESTRATION
    )
    electricity_result = _run_electricity(
        electricity_binary, tmp_path / "electricity_case", home_dir, args=args,
        orchestration=SHELL_ORCHESTRATION,
    )

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr


def test_missing_orchestration_fails_the_same_way_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    args = ["--config", "config.json"]

    cof_result = _run_cof(tmp_path / "cof_case", home_dir, args=args)
    electricity_result = _run_electricity(
        electricity_binary, tmp_path / "electricity_case", home_dir, args=args
    )

    # cof's own stdout carries a second line electricity's doesn't --
    # `Tip: run cof list to see available orchestrations.` (DESIGN.md's
    # own row: electricity has no `list` subcommand for it to name) --
    # so only the `Error: ...` line itself, not the whole of stdout,
    # is compared.
    expected = "Error: Missing orchestration. Use --last or provide a path/name."
    assert cof_result.returncode == 1, cof_result.stderr
    assert electricity_result.returncode == 1, electricity_result.stderr
    assert cof_result.stdout.splitlines()[0] == expected
    assert electricity_result.stdout == expected + "\n"


def test_orchestration_not_found_fails_with_the_same_message_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    args = ["does-not-exist.yml", "--config", "config.json"]

    cof_result = _run_cof(tmp_path / "cof_case", home_dir, args=args)
    electricity_result = _run_electricity(
        electricity_binary, tmp_path / "electricity_case", home_dir, args=args
    )

    # Same Tip-line divergence as the missing-orchestration case above.
    expected = "Error: Orchestration not found: does-not-exist.yml"
    assert cof_result.returncode == 1, cof_result.stderr
    assert electricity_result.returncode == 1, electricity_result.stderr
    assert cof_result.stdout.splitlines()[0] == expected
    assert electricity_result.stdout == expected + "\n"


def test_an_unknown_option_fails_with_exit_code_2_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """cof run's own usage-error frame is a Rich panel whose exact
    rendering (box width, close-match suggestions) depends on the
    terminal/`COLUMNS` and is never byte-stable even across two real
    `cof run` invocations (DESIGN.md's own row) -- so only the
    `No such option: --bogus` core inside it, not the whole frame, is
    compared, alongside the exit code."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    args = ["orchestration.yml", "--config", "config.json", "--bogus"]

    cof_result = _run_cof(tmp_path / "cof_case", home_dir, args=args)
    electricity_result = _run_electricity(
        electricity_binary, tmp_path / "electricity_case", home_dir, args=args
    )

    assert cof_result.returncode == 2, cof_result.stderr
    assert electricity_result.returncode == 2, electricity_result.stderr
    assert _no_such_option_core(cof_result.stderr) == _no_such_option_core(
        electricity_result.stderr
    )


def test_short_version_and_help_flags_are_unknown_options_under_run_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """`cof run` has no `-V`/`--version`/`-h` of its own at all -- each
    is itself an unrecognized option there, unlike the legacy form."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    for flag in ("-V", "--version", "-h"):
        args = ["orchestration.yml", "--config", "config.json", flag]
        cof_result = _run_cof(tmp_path / f"cof_case_{flag}", home_dir, args=args)
        electricity_result = _run_electricity(
            electricity_binary, tmp_path / f"electricity_case_{flag}", home_dir, args=args
        )
        assert cof_result.returncode == 2, (flag, cof_result.stderr)
        assert electricity_result.returncode == 2, (flag, electricity_result.stderr)
        assert _no_such_option_core(cof_result.stderr) == _no_such_option_core(
            electricity_result.stderr
        ), flag


def test_config_discovery_finds_a_trusted_project_config_with_no_config_flag(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    env = {"CIRCUITRY_TRUST_PROJECT_CONFIG": "1"}

    def prepare(case_dir: Path) -> None:
        case_dir.mkdir()
        (case_dir / "orchestration.yml").write_text(ORCHESTRATION, encoding="utf-8")
        (case_dir / "circuitry.config.json").write_text(
            '{"default_model": "project-model"}', encoding="utf-8"
        )

    cof_case = tmp_path / "cof_case"
    electricity_case = tmp_path / "electricity_case"
    prepare(cof_case)
    prepare(electricity_case)

    cof_result = subprocess.run(
        [sys.executable, "-m", "circuitry.cli.app", "run", "orchestration.yml"],
        cwd=cof_case, env=_sandboxed_env(home_dir, env), capture_output=True, text=True,
        timeout=30, check=False,
    )
    electricity_result = subprocess.run(
        [str(electricity_binary), "run", "orchestration.yml"],
        cwd=electricity_case, env=_sandboxed_env(home_dir, env), capture_output=True, text=True,
        timeout=30, check=False,
    )

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr
    # Not just a successful exit -- the discovered file's own setting
    # must actually have been applied (a build that silently ignored
    # the discovered file would also exit 0 here).
    assert _effective_settings(cof_result.stdout)["model"] == "project-model"
    assert _effective_settings(electricity_result.stdout)["model"] == "project-model"


def test_config_discovery_skips_an_untrusted_project_config_with_the_same_warning(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()

    def prepare(case_dir: Path) -> None:
        case_dir.mkdir()
        (case_dir / "orchestration.yml").write_text(ORCHESTRATION, encoding="utf-8")
        (case_dir / "circuitry.config.json").write_text(
            '{"default_model": "project-model"}', encoding="utf-8"
        )

    cof_case = tmp_path / "cof_case"
    electricity_case = tmp_path / "electricity_case"
    prepare(cof_case)
    prepare(electricity_case)

    cof_result = subprocess.run(
        [sys.executable, "-m", "circuitry.cli.app", "run", "orchestration.yml"],
        cwd=cof_case, env=_sandboxed_env(home_dir), capture_output=True, text=True,
        timeout=30, check=False,
    )
    electricity_result = subprocess.run(
        [str(electricity_binary), "run", "orchestration.yml"],
        cwd=electricity_case, env=_sandboxed_env(home_dir), capture_output=True, text=True,
        timeout=30, check=False,
    )

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr

    def skip_line(stderr: str) -> str:
        for line in stderr.splitlines():
            if "Skipped project config" in line:
                return line
        raise AssertionError(f"no skip warning in stderr: {stderr!r}")

    cof_warning = skip_line(cof_result.stderr)
    electricity_warning = skip_line(electricity_result.stderr)
    assert "not trusted" in cof_warning
    assert cof_warning.replace(str(cof_case), "") == electricity_warning.replace(
        str(electricity_case), ""
    )
    # The skip isn't merely reported -- the skipped file's own setting
    # must not have been applied either, on either engine.
    assert _effective_settings(cof_result.stdout)["model"] != "project-model"
    assert _effective_settings(electricity_result.stdout)["model"] != "project-model"


def test_circuitry_config_env_var_is_honored_with_no_config_flag(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()

    cof_case = tmp_path / "cof_case"
    electricity_case = tmp_path / "electricity_case"
    _write_case(cof_case)
    _write_case(electricity_case)
    cof_env_config = cof_case / "env-config.json"
    cof_env_config.write_text('{"default_model": "env-var-model"}', encoding="utf-8")
    electricity_env_config = electricity_case / "env-config.json"
    electricity_env_config.write_text('{"default_model": "env-var-model"}', encoding="utf-8")

    cof_result = subprocess.run(
        [sys.executable, "-m", "circuitry.cli.app", "run", "orchestration.yml"],
        cwd=cof_case,
        env=_sandboxed_env(home_dir, {"CIRCUITRY_CONFIG": str(cof_env_config)}),
        capture_output=True, text=True, timeout=30, check=False,
    )
    electricity_result = subprocess.run(
        [str(electricity_binary), "run", "orchestration.yml"],
        cwd=electricity_case,
        env=_sandboxed_env(home_dir, {"CIRCUITRY_CONFIG": str(electricity_env_config)}),
        capture_output=True, text=True, timeout=30, check=False,
    )

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr
    # `config.json` (`_write_case`'s own default) must never have been
    # read instead -- only `CIRCUITRY_CONFIG`'s own file.
    assert _effective_settings(cof_result.stdout)["model"] == "env-var-model"
    assert _effective_settings(electricity_result.stdout)["model"] == "env-var-model"


def test_the_legacy_positional_form_is_still_a_working_alias_for_electricity(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()

    cof_result = _run_cof(
        tmp_path / "cof_case", home_dir, args=["orchestration.yml", "--config", "config.json"]
    )
    case_dir = tmp_path / "electricity_case"
    _write_case(case_dir)
    electricity_result = subprocess.run(
        [str(electricity_binary), "config.json", "orchestration.yml"],
        cwd=case_dir, env=_sandboxed_env(home_dir), capture_output=True, text=True,
        timeout=30, check=False,
    )

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr
    assert (
        json.loads(cof_result.stdout)["prime"]["ok"]["value"]
        == json.loads(electricity_result.stdout)["prime"]["ok"]["value"]
    )


def test_label_flag_overrides_the_document_and_is_not_rendered_on_either_engine(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """`--label TEXT` (#752) is `cof run`/`electricity run`-only: literal
    text, never rendered, that wins over the document's own top-level
    `label:` template -- compared through `runtime.last_run.label` in
    each engine's own `--out`, since this orchestration has no `--out`
    of its own on either (the state JSON on stdout carries it too)."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    labelled_orchestration = "label: 'from the document'\n" + ORCHESTRATION
    args = [
        "orchestration.yml", "--config", "config.json",
        "--label", "{{input.ticket}}",
    ]

    cof_result = _run_cof(
        tmp_path / "cof_case", home_dir, args=args, orchestration=labelled_orchestration
    )
    electricity_result = _run_electricity(
        electricity_binary, tmp_path / "electricity_case", home_dir, args=args,
        orchestration=labelled_orchestration,
    )

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr
    cof_state = json.loads(cof_result.stdout)
    electricity_state = json.loads(electricity_result.stdout)
    assert cof_state["runtime"]["last_run"]["label"] == "{{input.ticket}}"
    assert electricity_state["runtime"]["last_run"]["label"] == "{{input.ticket}}"


def test_a_document_inside_the_library_cache_path_is_gated_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """The one exception to `SHELL_ORCHESTRATION`'s own "never gated"
    rule above: a path resolving inside the library cache directory
    (`CIRCUITRY_CACHE_DIR`/`XDG_CACHE_HOME`/`~/.cache/circuitry/
    library`) names the exact same fetched content a library-name run
    would, so it is gated exactly as if it were one (issue #337) -- on
    both engines now (issue #728's own cache-path port). Refused
    without `--allow-capabilities`, with the reference's own text and
    exit code; allowed once it covers what the document needs."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()

    def prepare(case_dir: Path) -> Path:
        cache_dir = case_dir / "cache"
        library_dir = cache_dir / "circuitry" / "library" / "src"
        library_dir.mkdir(parents=True)
        doc = library_dir / "doc.yml"
        doc.write_text(SHELL_ORCHESTRATION, encoding="utf-8")
        (case_dir / "config.json").write_text("{}", encoding="utf-8")
        return doc

    cof_case = tmp_path / "cof_case"
    electricity_case = tmp_path / "electricity_case"
    cof_case.mkdir()
    electricity_case.mkdir()
    cof_doc = prepare(cof_case)
    electricity_doc = prepare(electricity_case)

    def run(
        case_dir: Path, doc: Path, *, cmd: list[str], extra_args: list[str]
    ) -> subprocess.CompletedProcess[str]:
        env = _sandboxed_env(home_dir, {"XDG_CACHE_HOME": str(case_dir / "cache")})
        return subprocess.run(
            [*cmd, str(doc), "--config", "config.json", *extra_args],
            cwd=case_dir, env=env, capture_output=True, text=True, timeout=30, check=False,
        )

    cof_refused = run(
        cof_case, cof_doc, cmd=[sys.executable, "-m", "circuitry.cli.app", "run"], extra_args=[]
    )
    electricity_refused = run(
        electricity_case, electricity_doc, cmd=[str(electricity_binary), "run"], extra_args=[]
    )
    assert cof_refused.returncode == 1, cof_refused.stderr
    assert electricity_refused.returncode == 1, electricity_refused.stderr
    cof_error = json.loads(cof_refused.stdout)["error"]
    electricity_error = json.loads(electricity_refused.stdout)["error"]
    expected = (
        f"'{cof_doc}' needs capabilities nobody has consented to: shell. Run "
        f"`cof trust {cof_doc}` to review and approve it, or pass "
        "`--allow-capabilities shell` for a scripted/CI run."
    )
    assert cof_error == expected
    assert electricity_error == expected.replace(str(cof_doc), str(electricity_doc))

    cof_allowed = run(
        cof_case,
        cof_doc,
        cmd=[sys.executable, "-m", "circuitry.cli.app", "run"],
        extra_args=["--allow-capabilities", "shell"],
    )
    electricity_allowed = run(
        electricity_case,
        electricity_doc,
        cmd=[str(electricity_binary), "run"],
        extra_args=["--allow-capabilities", "shell"],
    )
    assert cof_allowed.returncode == 0, cof_allowed.stderr
    assert electricity_allowed.returncode == 0, electricity_allowed.stderr
    assert _effective_settings(cof_allowed.stdout)["runtime"]["_capability_ceiling"] == ["shell"]
    assert (
        _effective_settings(electricity_allowed.stdout)["runtime"]["_capability_ceiling"]
        == ["shell"]
    )


def _poll_live_state(
    proc: subprocess.Popen[str], live_path: Path, *, timeout: float = 30.0,
) -> tuple[list[dict], int]:
    """Samples *live_path* at ~10ms while *proc* runs, returning every
    distinct JSON-decoded version seen plus the process's own exit
    code -- the Python-side counterpart of
    `electricity/crates/electricity-cli/tests/live_state.rs::
    observe_live_state`, polling a real file on disk rather than
    reaching into either engine's own internals."""
    observations: list[dict] = []
    last_text: str | None = None

    def sample() -> None:
        nonlocal last_text
        if not live_path.exists():
            return
        try:
            text = live_path.read_text(encoding="utf-8")
        except OSError:
            return
        if not text or text == last_text:
            return
        try:
            parsed = json.loads(text)
        except json.JSONDecodeError:
            return
        observations.append(parsed)
        last_text = text

    deadline = time.monotonic() + timeout
    while True:
        sample()
        returncode = proc.poll()
        if returncode is not None:
            break
        assert time.monotonic() < deadline, "process did not exit within the bounded timeout"
        time.sleep(0.01)
    # One more read right after exit, same rationale as the Rust
    # helper: the final `close()` write may have landed in the gap
    # between the last poll and `proc.poll()` observing the exit.
    sample()
    return observations, returncode


def _slow_awk_live_checkpoint(observations: list[dict]) -> dict | None:
    """The first observed `prime.slow_awk.meta.live` with `line1` in
    its stdout tail and nothing in its stderr tail yet -- a single,
    deterministic point in the run (well before `line2`/`line3`) that
    both engines pass through, so comparing it is comparing the same
    moment on each side, not two independently-timed samples."""
    for parsed in observations:
        live = parsed.get("prime", {}).get("slow_awk", {}).get("meta", {}).get("live")
        if live is None:
            continue
        stdout_tail = live.get("stdout_tail", "")
        stderr_tail = live.get("stderr_tail", "")
        if "line1" in stdout_tail and "line2" not in stdout_tail and stderr_tail == "":
            return live
    return None


def test_a_running_tools_meta_live_tail_matches_byte_for_byte_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """Issue #741/#751's own cross-engine parity requirement: while the
    same slow `awk` step runs, both engines' `--live-state` must show
    the same `stdout_tail`/`stderr_tail`/`stdout_bytes`/`stderr_bytes`
    at the same point in the run (`_slow_awk_live_checkpoint`), apart
    from timestamps. #751 ported `on_live` to cof's own
    `GenericSubprocessTool` path, so this runs for real on both
    engines now -- a cof that publishes nothing is a real failure
    here, never a skip."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()

    def run_and_sample(
        binary_args: list[str], case_dir: Path, live_path: Path
    ) -> tuple[list[dict], int]:
        _write_case(case_dir, SLOW_AWK_ORCHESTRATION)
        proc = subprocess.Popen(
            [*binary_args, "--live-state", str(live_path)],
            cwd=case_dir, env=_sandboxed_env(home_dir), stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True,
        )
        try:
            observations, returncode = _poll_live_state(proc, live_path)
        finally:
            proc.communicate(timeout=5)
        return observations, returncode

    cof_observations, cof_returncode = run_and_sample(
        [sys.executable, "-m", "circuitry.cli.app", "run", "orchestration.yml", "--config", "config.json"],
        tmp_path / "cof_case",
        tmp_path / "cof_live.json",
    )
    assert cof_returncode == 0

    cof_checkpoint = _slow_awk_live_checkpoint(cof_observations)
    assert cof_checkpoint is not None, (
        "cof never published a prime.slow_awk.meta.live with line1 alone; observed "
        f"{len(cof_observations)} version(s): {cof_observations!r}"
    )

    electricity_observations, electricity_returncode = run_and_sample(
        [str(electricity_binary), "run", "orchestration.yml", "--config", "config.json"],
        tmp_path / "electricity_case",
        tmp_path / "electricity_live.json",
    )
    assert electricity_returncode == 0

    electricity_checkpoint = _slow_awk_live_checkpoint(electricity_observations)
    assert electricity_checkpoint is not None, (
        "electricity never published a prime.slow_awk.meta.live with line1 alone"
    )

    for key in ("stdout_tail", "stderr_tail", "stdout_bytes", "stderr_bytes"):
        assert cof_checkpoint[key] == electricity_checkpoint[key], key


# --- issue #760: the `--last`/`--resume last` stash, shared by both engines ---

ENGINES = ("cof", "electricity")

#: A document with a declared input, so a plain `-e` round-trips into
#: `state.input.topic` without the orchestration having to route it
#: through a tool first -- the stash's own replay is what these tests
#: check, not any particular tool's behavior.
LAST_STASH_ORCHESTRATION = (
    "interface:\n  inputs:\n    topic: {type: string, default: 'none'}\n"
    "effects:\n  - name: ok\n    type: tool\n    provider: json\n"
    "    params: {mode: parse, input: '{}'}\n"
)

#: step1 always succeeds, step2 always fails -- `--resume last` must
#: reach step2 again (proving the source resolution worked) without
#: re-dispatching step1 (proving the skip-and-continue behaviour did
#: too), the same split `tests/cli/test_run_resume.py`'s own cof-only
#: tests use.
LAST_STASH_RESUME_ORCHESTRATION = (
    "effects:\n"
    "  - name: step1\n    type: tool\n    provider: uuid\n"
    "  - name: step2\n    type: tool\n    provider: json\n"
    "    params: {mode: parse, input: 'not valid json'}\n"
)


def _cmd(engine: str, binary: Path, args: list[str]) -> list[str]:
    if engine == "cof":
        return [sys.executable, "-m", "circuitry.cli.app", "run", *args]
    return [str(binary), "run", *args]


def _run(
    engine: str, binary: Path, case_dir: Path, home_dir: Path, args: list[str],
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        _cmd(engine, binary, args), cwd=case_dir, env=_sandboxed_env(home_dir),
        capture_output=True, text=True, timeout=30, check=False,
    )


def _write_last_stash_case(case_dir: Path, orchestration: str) -> None:
    case_dir.mkdir()
    (case_dir / "orchestration.yml").write_text(orchestration, encoding="utf-8")
    (case_dir / "config.json").write_text("{}", encoding="utf-8")


@pytest.mark.parametrize("replay_engine", ENGINES)
@pytest.mark.parametrize("write_engine", ENGINES)
def test_last_replays_the_same_run_regardless_of_which_engine_wrote_or_reads_the_stash(
    tmp_path: Path, electricity_binary: Path, write_engine: str, replay_engine: str,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    case_dir = tmp_path / "case"
    _write_last_stash_case(case_dir, LAST_STASH_ORCHESTRATION)

    written = _run(
        write_engine, electricity_binary, case_dir, home_dir,
        ["orchestration.yml", "--config", "config.json", "-e", "topic=widgets", "--out", "out.json"],
    )
    assert written.returncode == 0, (write_engine, written.stdout, written.stderr)

    replayed = _run(replay_engine, electricity_binary, case_dir, home_dir, ["--last"])
    assert replayed.returncode == 0, (replay_engine, replayed.stdout, replayed.stderr)

    state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))
    assert state["input"]["topic"] == "widgets"


@pytest.mark.parametrize("resume_engine", ENGINES)
@pytest.mark.parametrize("write_engine", ENGINES)
def test_resume_last_resumes_the_same_state_regardless_of_which_engine_wrote_or_reads_the_stash(
    tmp_path: Path, electricity_binary: Path, write_engine: str, resume_engine: str,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    case_dir = tmp_path / "case"
    _write_last_stash_case(case_dir, LAST_STASH_RESUME_ORCHESTRATION)

    first = _run(
        write_engine, electricity_binary, case_dir, home_dir,
        ["orchestration.yml", "--config", "config.json", "--out", "out.json"],
    )
    assert first.returncode == 1, (write_engine, first.stdout, first.stderr)
    first_state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))
    step1_uuid = first_state["prime"]["step1"]["value"]

    resumed = _run(
        resume_engine, electricity_binary, case_dir, home_dir,
        ["orchestration.yml", "--config", "config.json", "--resume", "last"],
    )
    # step2's own input is always invalid JSON, so the resumed run still
    # fails there -- proving the resume reached it again -- while step1's
    # own uuid staying byte-identical proves it was skipped, not
    # re-dispatched.
    assert resumed.returncode == 1, (resume_engine, resumed.stdout, resumed.stderr)
    resumed_state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))
    assert resumed_state["prime"]["step1"]["value"] == step1_uuid


def _last_refusal_error(result: subprocess.CompletedProcess[str]) -> str:
    """The refusal text from either engine's own `--last`/`--resume last`
    stdout -- cof prints a plain `Error: <message>` line (`console.print`,
    unconditional); electricity wraps the identical message in its own
    `{"ok": false, "error": ...}` JSON payload (DESIGN.md's own row on
    this framing difference) -- so only the message itself is compared,
    never the surrounding frame.
    """
    stdout = result.stdout.strip()
    if stdout.startswith("{"):
        text = str(json.loads(stdout)["error"])
    else:
        assert stdout.startswith("Error: "), stdout
        text = stdout[len("Error: ") :]
    # cof's own `console.print` soft-wraps a long line at its console's
    # width (even off a terminal) -- collapsed back to one line here so
    # a wrap point newline never fails a comparison electricity's own
    # single-line stdout would otherwise pass trivially.
    return " ".join(text.split())


@pytest.mark.parametrize("engine", ENGINES)
def test_last_with_no_previous_run_fails_the_same_way_on_both_engines(
    tmp_path: Path, electricity_binary: Path, engine: str,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    case_dir = tmp_path / "case"
    case_dir.mkdir()

    result = _run(engine, electricity_binary, case_dir, home_dir, ["--last"])
    assert result.returncode == 2, (engine, result.stdout, result.stderr)
    assert "No previous run found. Run an orchestration first." in result.stderr


@pytest.mark.parametrize("engine", ENGINES)
def test_resume_last_with_no_previous_run_fails_the_same_way_on_both_engines(
    tmp_path: Path, electricity_binary: Path, engine: str,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    case_dir = tmp_path / "case"
    _write_last_stash_case(case_dir, LAST_STASH_ORCHESTRATION)

    result = _run(
        engine, electricity_binary, case_dir, home_dir,
        ["orchestration.yml", "--config", "config.json", "--resume", "last"],
    )
    assert result.returncode == 1, (engine, result.stdout, result.stderr)
    assert _last_refusal_error(result) == "No previous run found — nothing to resume from."


def test_resume_last_with_an_unreadable_stash_fails_the_same_way_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    # Compared engine-to-engine, not against a hardcoded string: the
    # message embeds this test's own absolute stash path, long enough
    # that `console.print`'s soft-wrap can fold it at an arbitrary
    # character (no space to re-join on), not just at a word boundary
    # -- whitespace is stripped entirely on both sides instead of
    # collapsed, so neither engine's own wrap point can fail this.
    # Checked structurally (prefix contains, suffix matches) rather than
    # by exact string equality between engines: the message embeds this
    # test's own absolute stash path (necessarily different between the
    # two engines' own separate home dirs, same as any other path-
    # bearing message elsewhere in this suite), and `console.print`'s
    # soft-wrap can fold a path that long at an arbitrary character --
    # including inside the path itself -- making a literal substring
    # replace on it unreliable too.
    for engine in ENGINES:
        home_dir = tmp_path / f"home-{engine}"
        home_dir.mkdir()
        stash_dir = home_dir / ".config" / "circuitry"
        stash_dir.mkdir(parents=True)
        (stash_dir / "last-run.json").write_text("not json", encoding="utf-8")
        case_dir = tmp_path / f"case-{engine}"
        _write_last_stash_case(case_dir, LAST_STASH_ORCHESTRATION)
        result = _run(
            engine, electricity_binary, case_dir, home_dir,
            ["orchestration.yml", "--config", "config.json", "--resume", "last"],
        )
        assert result.returncode == 1, (engine, result.stdout, result.stderr)
        error = _last_refusal_error(result)
        assert "is not valid JSON" in error and "Expecting value" in error, (engine, error)
        assert error.endswith("Nothing to resume from."), (engine, error)


@pytest.mark.parametrize("engine", ENGINES)
def test_resume_last_with_a_different_orchestration_fails_the_same_way_on_both_engines(
    tmp_path: Path, electricity_binary: Path, engine: str,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    case_dir = tmp_path / "case"
    _write_last_stash_case(case_dir, LAST_STASH_ORCHESTRATION)
    (case_dir / "other.yml").write_text(LAST_STASH_ORCHESTRATION, encoding="utf-8")

    written = _run(
        engine, electricity_binary, case_dir, home_dir,
        ["orchestration.yml", "--config", "config.json", "--out", "out.json"],
    )
    assert written.returncode == 0, (engine, written.stdout, written.stderr)

    result = _run(
        engine, electricity_binary, case_dir, home_dir,
        ["other.yml", "--config", "config.json", "--resume", "last"],
    )
    assert result.returncode == 1, (engine, result.stdout, result.stderr)
    assert _last_refusal_error(result) == (
        "The last run was 'orchestration.yml', not other.yml — run that orchestration "
        "again, or pass --state <file> to resume a specific saved state instead of 'last'."
    )


@pytest.mark.parametrize("engine", ENGINES)
def test_resume_last_with_no_out_from_the_last_run_fails_the_same_way_on_both_engines(
    tmp_path: Path, electricity_binary: Path, engine: str,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    case_dir = tmp_path / "case"
    _write_last_stash_case(case_dir, LAST_STASH_ORCHESTRATION)

    written = _run(
        engine, electricity_binary, case_dir, home_dir,
        ["orchestration.yml", "--config", "config.json"],
    )
    assert written.returncode == 0, (engine, written.stdout, written.stderr)

    result = _run(
        engine, electricity_binary, case_dir, home_dir,
        ["orchestration.yml", "--config", "config.json", "--resume", "last"],
    )
    assert result.returncode == 1, (engine, result.stdout, result.stderr)
    assert _last_refusal_error(result) == (
        "The last run did not use --out, so it saved no state to resume from."
    )


@pytest.mark.parametrize("engine", ENGINES)
def test_resume_last_with_the_out_file_gone_fails_the_same_way_on_both_engines(
    tmp_path: Path, electricity_binary: Path, engine: str,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    case_dir = tmp_path / "case"
    _write_last_stash_case(case_dir, LAST_STASH_ORCHESTRATION)

    written = _run(
        engine, electricity_binary, case_dir, home_dir,
        ["orchestration.yml", "--config", "config.json", "--out", "gone.json"],
    )
    assert written.returncode == 0, (engine, written.stdout, written.stderr)
    (case_dir / "gone.json").unlink()

    result = _run(
        engine, electricity_binary, case_dir, home_dir,
        ["orchestration.yml", "--config", "config.json", "--resume", "last"],
    )
    assert result.returncode == 1, (engine, result.stdout, result.stderr)
    assert _last_refusal_error(result) == (
        "The last run's --out file is gone: state file not found: gone.json"
    )


@pytest.mark.parametrize("engine", ENGINES)
def test_last_with_a_redacted_secret_refuses_to_replay_on_both_engines(
    tmp_path: Path, electricity_binary: Path, engine: str,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    stash_dir = home_dir / ".config" / "circuitry"
    stash_dir.mkdir(parents=True)
    (stash_dir / "last-run.json").write_text(
        json.dumps(
            {
                "orchestration": "orchestration.yml",
                "env_vars": ["OPENAI_API_KEY=***REDACTED***"],
            }
        ),
        encoding="utf-8",
    )
    case_dir = tmp_path / "case"
    _write_last_stash_case(case_dir, LAST_STASH_ORCHESTRATION)

    result = _run(engine, electricity_binary, case_dir, home_dir, ["--last"])
    assert result.returncode == 1, (engine, result.stdout, result.stderr)
    # Two plain lines on stdout for both engines (never the JSON failure
    # payload: this check runs before `run()`/`run_orchestration_with_
    # config` is ever reached) -- collapsed to one for the same soft-wrap
    # reason `_last_refusal_error` collapses its own single-line text.
    expected = (
        "Error: previous run included redacted secrets in -e values. "
        "Re-run explicitly with the secret supplied via env var or config file "
        "(recommended), or via -e for this invocation."
    )
    assert " ".join(result.stdout.split()) == expected


@pytest.mark.parametrize("engine", ENGINES)
def test_resume_and_last_together_are_mutually_exclusive_on_both_engines(
    tmp_path: Path, electricity_binary: Path, engine: str,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    case_dir = tmp_path / "case"
    _write_last_stash_case(case_dir, LAST_STASH_ORCHESTRATION)

    result = _run(
        engine, electricity_binary, case_dir, home_dir,
        ["orchestration.yml", "--config", "config.json", "--resume", "last", "--last"],
    )
    assert result.returncode == 1, (engine, result.stdout, result.stderr)
    assert result.stdout.strip() == "Error: --resume and --last are mutually exclusive."


def _stash_path(home_dir: Path) -> Path:
    return home_dir / ".config" / "circuitry" / "last-run.json"


@pytest.mark.parametrize("engine", ENGINES)
def test_resume_last_does_not_replay_the_stashs_own_e_values_on_either_engine(
    tmp_path: Path, electricity_binary: Path, engine: str,
) -> None:
    """issue #760's own ruling: `--resume last` resumes from the loaded
    `--out` file's own `input.*`, never replaying the stash's `-e`
    values on top of it -- unlike plain `--last`, which replays the
    whole prior invocation. The stash's own `env_vars` is rewritten
    between the two runs below (to a different value, then to one with a
    redacted secret) so this can't pass merely because the stashed and
    loaded values happen to agree -- a build that *did* replay the
    stash's `-e` on top would either see the rewritten value or refuse
    outright on the redacted one, and this asserts neither happens."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    case_dir = tmp_path / "case"
    _write_last_stash_case(case_dir, LAST_STASH_RESUME_ORCHESTRATION)

    first = _run(
        engine, electricity_binary, case_dir, home_dir,
        ["orchestration.yml", "--config", "config.json", "-e", "unused=original", "--out", "out.json"],
    )
    assert first.returncode == 1, (engine, first.stdout, first.stderr)

    stash_path = _stash_path(home_dir)
    stash = json.loads(stash_path.read_text(encoding="utf-8"))
    assert stash["env_vars"] == ["unused=original"], stash
    stash["env_vars"] = ["unused=changed"]
    stash_path.write_text(json.dumps(stash), encoding="utf-8")

    resumed = _run(
        engine, electricity_binary, case_dir, home_dir,
        ["orchestration.yml", "--config", "config.json", "--resume", "last"],
    )
    assert resumed.returncode == 1, (engine, resumed.stdout, resumed.stderr)
    resumed_state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))
    # The loaded state's own `input.unused` survives from the first run
    # (the saved file already carries it); the stash's own rewritten
    # `env_vars` is never replayed on top of it.
    assert resumed_state["input"]["unused"] == "original"

    # A redacted secret in the stash's `env_vars` refuses plain `--last`
    # (`test_last_with_a_redacted_secret_refuses_to_replay_on_both_engines`)
    # but must not refuse `--resume last`, which never reads `env_vars` at
    # all -- confirming the skip above isn't an artifact of an unrelated
    # refusal silently winning instead.
    stash = json.loads(stash_path.read_text(encoding="utf-8"))
    stash["env_vars"] = ["OPENAI_API_KEY=***REDACTED***"]
    stash_path.write_text(json.dumps(stash), encoding="utf-8")

    resumed_again = _run(
        engine, electricity_binary, case_dir, home_dir,
        ["orchestration.yml", "--config", "config.json", "--resume", "last"],
    )
    assert resumed_again.returncode == 1, (engine, resumed_again.stdout, resumed_again.stderr)
    resumed_again_state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))
    assert resumed_again_state["input"]["unused"] == "original"


#: A document with a declared input and one step whose own `uuid` output
#: changes on every run -- used to tell "resumed" (the skipped step's old
#: uuid survives) apart from "re-run from scratch" (a fresh one) the same
#: way `LAST_STASH_RESUME_ORCHESTRATION` does, but here it's the
#: *document* that changes between the two runs, not its content staying
#: fixed while `--resume` itself is refused.
RESUME_HASH_ORCHESTRATION = (
    "effects:\n"
    "  - name: step1\n    type: tool\n    provider: uuid\n"
    "  - name: step2\n    type: tool\n    provider: json\n"
    "    params: {mode: parse, input: 'not valid json'}\n"
)


@pytest.mark.parametrize("engine", ENGINES)
def test_a_refused_resume_never_overwrites_the_stash_it_read(
    tmp_path: Path, electricity_binary: Path, engine: str,
) -> None:
    """`cli/app.py::_resolve_resume_state`'s own document-hash refusal
    raises entirely in the CLI layer, before `cof run`'s own `run()` --
    and so `_save_last_run` -- is ever reached, so a resume it refuses
    must leave the stash exactly as the first run left it."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    case_dir = tmp_path / "case"
    _write_last_stash_case(case_dir, RESUME_HASH_ORCHESTRATION)

    first = _run(
        engine, electricity_binary, case_dir, home_dir,
        ["orchestration.yml", "--config", "config.json", "-e", "marker=first", "--out", "out.json"],
    )
    assert first.returncode == 1, (engine, first.stdout, first.stderr)
    stash_path = _stash_path(home_dir)
    stash_before = stash_path.read_bytes()

    # Changing the document's own bytes invalidates the hash `out.json`
    # recorded -- `--force` is not passed, so this must be refused.
    (case_dir / "orchestration.yml").write_text(
        RESUME_HASH_ORCHESTRATION + "  # changed\n", encoding="utf-8",
    )

    refused = _run(
        engine, electricity_binary, case_dir, home_dir,
        ["orchestration.yml", "--config", "config.json", "--resume", "last"],
    )
    assert refused.returncode == 1, (engine, refused.stdout, refused.stderr)
    assert "changed since the run being resumed" in _last_refusal_error(refused)

    assert stash_path.read_bytes() == stash_before, (
        "the refused --resume last overwrote the stash the first run wrote"
    )


@pytest.mark.parametrize("replay_engine", ENGINES)
@pytest.mark.parametrize("write_engine", ENGINES)
def test_resume_last_normalizes_a_leading_dot_slash_the_same_way_cof_does(
    tmp_path: Path, electricity_binary: Path, write_engine: str, replay_engine: str,
) -> None:
    """`cof run`'s own CLI path flags are all `Path`s by the time they
    reach `_save_last_run`/`_resolve_resume_state` (Typer's own
    conversion), so `./orchestration.yml` stashes and compares as
    `orchestration.yml` -- electricity's own plain-`String` flags must
    normalize the same way, on both the write and the read side, or a
    stash written for one spelling refuses a resume naming the other."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    case_dir = tmp_path / "case"
    _write_last_stash_case(case_dir, LAST_STASH_RESUME_ORCHESTRATION)

    first = _run(
        write_engine, electricity_binary, case_dir, home_dir,
        ["./orchestration.yml", "--config", "config.json", "--out", "out.json"],
    )
    assert first.returncode == 1, (write_engine, first.stdout, first.stderr)
    stash = json.loads(_stash_path(home_dir).read_text(encoding="utf-8"))
    assert stash["orchestration"] == "orchestration.yml", stash

    resumed = _run(
        replay_engine, electricity_binary, case_dir, home_dir,
        ["orchestration.yml", "--config", "config.json", "--resume", "last"],
    )
    assert resumed.returncode == 1, (replay_engine, resumed.stdout, resumed.stderr)


@pytest.mark.parametrize("write_engine", ENGINES)
def test_last_normalizes_a_leading_dot_slash_the_same_way_cof_does(
    tmp_path: Path, electricity_binary: Path, write_engine: str,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    case_dir = tmp_path / "case"
    _write_last_stash_case(case_dir, LAST_STASH_ORCHESTRATION)

    written = _run(
        write_engine, electricity_binary, case_dir, home_dir,
        ["./orchestration.yml", "--config", "config.json", "-e", "topic=widgets", "--out", "out.json"],
    )
    assert written.returncode == 0, (write_engine, written.stdout, written.stderr)

    replayed = _run("electricity", electricity_binary, case_dir, home_dir, ["--last"])
    assert replayed.returncode == 0, (replayed.stdout, replayed.stderr)
    state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))
    assert state["input"]["topic"] == "widgets"


@pytest.mark.parametrize("engine", ENGINES)
def test_last_stash_is_byte_identical_between_engines_for_the_same_run(
    tmp_path: Path, electricity_binary: Path, engine: str,
) -> None:
    """issue #760's own core requirement: the same keys, key order and
    JSON formatting, for the same invocation -- compared here against a
    fixed reference captured from `cof` once, rather than engine-to-
    engine, so a change that drifts *both* engines' own writers the same
    way (and so would pass an engine-to-engine comparison) still fails."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    case_dir = tmp_path / "case"
    _write_last_stash_case(case_dir, LAST_STASH_ORCHESTRATION)

    written = _run(
        engine, electricity_binary, case_dir, home_dir,
        ["orchestration.yml", "--config", "config.json", "-e", "topic=widgets", "--out", "out.json"],
    )
    assert written.returncode == 0, (engine, written.stdout, written.stderr)

    expected = {
        "orchestration": "orchestration.yml",
        "config": "config.json",
        "state": None,
        "out": "out.json",
        "pretty": False,
        "print_state": False,
        "dry_run": False,
        "json_out": True,
        "quiet": True,
        "verbose": False,
        "live_state": None,
        "events": None,
        "env_vars": ["topic=widgets"],
        "label": None,
        "tail": False,
        "skip_preflight": False,
        "allow_capabilities": None,
        "profile": None,
        "profile_from_state": None,
        "adapter": None,
        "model": None,
        "explain_routing": False,
        "decompose_out": None,
        "scoring": None,
        "routing": None,
        "decompose": None,
        "split": None,
        "trust_document": True,
        "remote_library_source": False,
    }
    stashed = json.loads(_stash_path(home_dir).read_text(encoding="utf-8"))
    assert stashed == expected, (engine, stashed)
    # `==` above doesn't check key order -- `json.loads` preserves
    # insertion order (Python 3.7+), so this does separately.
    assert list(stashed.keys()) == list(expected.keys()), (engine, list(stashed.keys()))
    raw = _stash_path(home_dir).read_bytes()
    assert raw.endswith(b"\n") and raw.count(b"\n") == 1
    assert b", " in raw and b": " in raw


#: One stashed key per case, each a setting `cof run` has a flag for and
#: electricity's own CLI doesn't (or a run-library shape electricity's
#: own positional orchestration argument can't reproduce) -- paired with
#: the setting name `unreplayable_setting` is expected to name in its
#: refusal. Every other required stash key is filled in by
#: `_minimal_stash` below with the default a plain run would have
#: written, so each case isolates exactly one divergent field.
UNREPLAYABLE_STASH_SETTINGS = (
    ({"remote_library_source": True, "trust_document": False}, "a remote library source"),
    ({"trust_document": False}, "library name"),
    ({"service_profile": "prod"}, "--service-profile"),
    ({"dry_run": True}, "--dry-run"),
    ({"skip_preflight": True}, "--skip-preflight"),
    ({"model": "claude-opus"}, "--model"),
    ({"profile_from_state": "recorded.json"}, "--profile-from-state"),
    ({"scoring": True}, "--scoring"),
    ({"routing": False}, "--routing"),
    ({"decompose": True}, "--decompose"),
    ({"split": False}, "--split"),
    ({"decompose_out": "parts.json"}, "--decompose-out"),
)


def _minimal_stash(**overrides: object) -> dict:
    stash: dict[str, object] = {
        "orchestration": "orchestration.yml",
        "config": "config.json",
        "env_vars": None,
        "trust_document": True,
        "remote_library_source": False,
    }
    stash.update(overrides)
    return stash


@pytest.mark.parametrize(("overrides", "expected_setting"), UNREPLAYABLE_STASH_SETTINGS)
def test_last_refuses_a_stashed_setting_electricity_cannot_replay(
    tmp_path: Path, electricity_binary: Path, overrides: dict, expected_setting: str,
) -> None:
    """issue #760: a stash either engine wrote must read back the same
    in the other -- but `cof run` can stash settings (a run-library's
    own `service_profile`/`remote_library_source`, or any flag
    electricity's own CLI doesn't have) that electricity has no way to
    apply. Dropping one silently would replay a materially different
    run, so `--last` refuses up front instead, naming the setting, and
    runs nothing at all."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    stash_dir = home_dir / ".config" / "circuitry"
    stash_dir.mkdir(parents=True)
    stash_dir.joinpath("last-run.json").write_text(
        json.dumps(_minimal_stash(**overrides)), encoding="utf-8",
    )
    case_dir = tmp_path / "case"
    _write_last_stash_case(case_dir, LAST_STASH_ORCHESTRATION)

    result = _run("electricity", electricity_binary, case_dir, home_dir, ["--last"])
    assert result.returncode == 1, (result.stdout, result.stderr)
    assert result.stdout.startswith("Error: the last run used ")
    assert expected_setting in result.stdout, result.stdout
    assert not (case_dir / "out.json").exists()
