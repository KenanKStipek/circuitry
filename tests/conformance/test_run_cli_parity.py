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
