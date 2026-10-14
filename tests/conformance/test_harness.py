"""Unit tests for `harness.load_case`'s `case.json` validation and for
`harness._sandboxed_env`'s hermeticity."""

from __future__ import annotations

import json
from pathlib import Path

import pytest
import yaml

from . import harness
from .normalize import apply_third_party_text, redact_runtime_strings


def _write_case(tmp_path: Path, metadata: dict) -> Path:
    case_dir = tmp_path / "case"
    case_dir.mkdir()
    (case_dir / "case.json").write_text(json.dumps(metadata), encoding="utf-8")
    return case_dir


def test_load_case_fills_in_defaults(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"spec_case": "C0", "expect": "success"})
    metadata = harness.load_case(case_dir)
    assert metadata["orchestration"] == "orchestration.yml"
    assert metadata["engines"] == ["python"]
    assert metadata["cli_args"] == []


def test_load_case_rejects_unknown_key(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "engine": ["python"]})
    with pytest.raises(ValueError, match="unknown key"):
        harness.load_case(case_dir)


def test_load_case_requires_expect(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"spec_case": "C0"})
    with pytest.raises(ValueError, match="missing the required 'expect' key"):
        harness.load_case(case_dir)


def test_load_case_rejects_invalid_expect(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "Success"})
    with pytest.raises(ValueError, match="'expect' must be one of"):
        harness.load_case(case_dir)


def test_load_case_rejects_invalid_error_compare(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "failure", "error_compare": "Exact"})
    with pytest.raises(ValueError, match="'error_compare' must be one of"):
        harness.load_case(case_dir)


def test_load_case_rejects_invalid_engines(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "engines": ["electicity"]})
    with pytest.raises(ValueError, match="'engines' must be"):
        harness.load_case(case_dir)


def test_load_case_rejects_empty_engines(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "engines": []})
    with pytest.raises(ValueError, match="'engines' must be"):
        harness.load_case(case_dir)


def test_load_case_rejects_non_bool_sort_warnings(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "sort_warnings": "true"})
    with pytest.raises(ValueError, match="'sort_warnings' must be a bool"):
        harness.load_case(case_dir)


def test_load_case_defaults_sort_warnings_to_false(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success"})
    metadata = harness.load_case(case_dir)
    assert metadata["sort_warnings"] is False


def test_load_case_defaults_profile_and_compare_run_warnings(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success"})
    metadata = harness.load_case(case_dir)
    assert metadata["profile"] is None
    assert metadata["compare_run_warnings"] is False


def test_load_case_rejects_non_bool_compare_run_warnings(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "compare_run_warnings": "yes"})
    with pytest.raises(ValueError, match="'compare_run_warnings' must be a bool"):
        harness.load_case(case_dir)


def test_load_case_rejects_a_profile_path_shaped_wrong(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "profile": "p.yml"})
    with pytest.raises(ValueError, match="'profile' must look like"):
        harness.load_case(case_dir)


def test_load_case_rejects_a_profile_path_that_does_not_exist(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "profile": "profiles/p.yml"})
    with pytest.raises(ValueError, match="which doesn't exist"):
        harness.load_case(case_dir)


def test_load_case_rejects_a_profile_path_with_both_suffixes_present(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "profile": "profiles/p.yml"})
    profiles_dir = case_dir / "profiles"
    profiles_dir.mkdir()
    (profiles_dir / "p.yml").write_text("model: m\n", encoding="utf-8")
    (profiles_dir / "p.yaml").write_text("model: m\n", encoding="utf-8")
    with pytest.raises(ValueError, match="also exists"):
        harness.load_case(case_dir)


def test_load_case_accepts_a_well_formed_profile_path(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "profile": "profiles/p.yml"})
    profiles_dir = case_dir / "profiles"
    profiles_dir.mkdir()
    (profiles_dir / "p.yml").write_text("model: m\n", encoding="utf-8")
    metadata = harness.load_case(case_dir)
    assert metadata["profile"] == "profiles/p.yml"


def test_profile_name_from_path_strips_the_profiles_prefix_and_suffix() -> None:
    assert harness.profile_name_from_path("profiles/p.yml") == "p"
    assert harness.profile_name_from_path("profiles/p.yaml") == "p"


def test_run_warning_lines_keeps_only_the_warning_colon_prefix() -> None:
    stderr = (
        "WARNING: Unknown environment variable\n"
        "Warning: adapter 'scripted' unavailable; effects [] will skip\n"
        "ERROR: something else\n"
    )
    assert harness.run_warning_lines(stderr) == [
        "Warning: adapter 'scripted' unavailable; effects [] will skip"
    ]


def test_run_warning_lines_empty_stderr_is_an_empty_list() -> None:
    assert harness.run_warning_lines("") == []


def test_load_case_rejects_an_exact_credential_env_key(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path, {"expect": "success", "env": {"OPENAI_API_KEY": "sk-leaked"}}
    )
    with pytest.raises(ValueError, match="look like a credential"):
        harness.load_case(case_dir)


def test_load_case_rejects_a_circuitry_env_key(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path, {"expect": "success", "env": {"CIRCUITRY_MODEL": "x"}}
    )
    with pytest.raises(ValueError, match="look like a credential"):
        harness.load_case(case_dir)


def test_load_case_accepts_a_synthetic_name_merely_containing_api_key(
    tmp_path: Path,
) -> None:
    # tools-adapters-plugins.md §7 case 12's own pattern: a name that
    # merely *contains* "API_KEY" (to exercise `env_vars`' own secret-
    # masking logic) is not itself one of `_sandboxed_env`'s exact
    # `CREDENTIAL_ENV_VARS` names, and stays allowed.
    case_dir = _write_case(
        tmp_path,
        {"expect": "success", "env": {"D1_CASE12_MY_API_KEY_SUFFIX": "masked-value"}},
    )
    metadata = harness.load_case(case_dir)
    assert metadata["env"] == {"D1_CASE12_MY_API_KEY_SUFFIX": "masked-value"}


def test_load_case_accepts_an_ordinary_env_key(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path, {"expect": "success", "env": {"D1_BAD_VAR": "plain"}}
    )
    metadata = harness.load_case(case_dir)
    assert metadata["env"] == {"D1_BAD_VAR": "plain"}


def test_warning_lines_keeps_only_prefixed_lines_in_order() -> None:
    stderr = (
        "WARNING: first\n"
        "some other line\n"
        "ERROR: second\n"
        "Warning: lowercase, not a logging line\n"
        "INFO: third\n"
    )
    assert harness.warning_lines(stderr) == [
        "WARNING: first",
        "ERROR: second",
        "INFO: third",
    ]


def test_warning_lines_sorts_when_asked() -> None:
    stderr = "WARNING: b\nWARNING: a\nWARNING: c\n"
    assert harness.warning_lines(stderr, sort=True) == [
        "WARNING: a",
        "WARNING: b",
        "WARNING: c",
    ]


def test_warning_lines_empty_stderr_is_an_empty_list() -> None:
    assert harness.warning_lines("") == []


def test_sandboxed_env_drops_credentials_and_circuitry_vars(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("OPENAI_API_KEY", "sk-leaked")
    monkeypatch.setenv("CIRCUITRY_MODEL", "x")
    monkeypatch.setenv("CIRCUITRY_CONFIG", "/nonexistent")
    case_dir = tmp_path / "case"
    case_dir.mkdir()
    home_dir = tmp_path / "home"

    env = harness._sandboxed_env(case_dir, home_dir)

    assert "OPENAI_API_KEY" not in env
    assert "CIRCUITRY_MODEL" not in env
    assert "CIRCUITRY_CONFIG" not in env
    assert env["HOME"] == str(home_dir)


def test_sandboxed_env_sets_leftover_replies_export_when_requested(tmp_path: Path) -> None:
    case_dir = tmp_path / "case"
    case_dir.mkdir()
    home_dir = tmp_path / "home"
    leftover_path = tmp_path / "leftover.json"

    env = harness._sandboxed_env(case_dir, home_dir, leftover_replies_path=leftover_path)

    assert env[harness.LEFTOVER_REPLIES_EXPORT_ENV_VAR] == str(leftover_path)


def test_credential_env_vars_covers_every_adapter_env_var() -> None:
    # Issue #503: every real adapter's own credential variable (the
    # issue's own "Environment variables" table) must be in the strip
    # list, not just openai/anthropic.
    for name in (
        "GROQ_API_KEY",
        "OPENROUTER_API_KEY",
        "AZURE_OPENAI_API_KEY",
        "REPLICATE_API_TOKEN",
        "WATSONX_API_KEY",
        "WATSONX_PROJECT_ID",
    ):
        assert name in harness.CREDENTIAL_ENV_VARS


def test_sandboxed_env_sets_fake_credentials_after_the_credential_filter(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("OPENAI_API_KEY", "sk-real-leaked")
    case_dir = tmp_path / "case"
    case_dir.mkdir()
    home_dir = tmp_path / "home"

    env = harness._sandboxed_env(
        case_dir,
        home_dir,
        fake_credentials={"OPENAI_API_KEY": "circuitry-test-fake-key"},
    )

    assert env["OPENAI_API_KEY"] == "circuitry-test-fake-key"


def test_sandboxed_env_with_no_fake_credentials_leaves_the_key_stripped(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("OPENAI_API_KEY", "sk-real-leaked")
    case_dir = tmp_path / "case"
    case_dir.mkdir()
    home_dir = tmp_path / "home"

    env = harness._sandboxed_env(case_dir, home_dir)

    assert "OPENAI_API_KEY" not in env


def test_load_case_defaults_fake_credentials_and_third_party_text(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success"})
    metadata = harness.load_case(case_dir)
    assert metadata["fake_credentials"] == {}
    assert metadata["third_party_text"] == []


def test_load_case_accepts_well_formed_fake_credentials(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path,
        {
            "expect": "success",
            "fake_credentials": {"OPENAI_API_KEY": "circuitry-test-fake-key"},
        },
    )
    metadata = harness.load_case(case_dir)
    assert metadata["fake_credentials"] == {"OPENAI_API_KEY": "circuitry-test-fake-key"}


def test_load_case_rejects_a_fake_credentials_key_outside_credential_env_vars(
    tmp_path: Path,
) -> None:
    case_dir = _write_case(
        tmp_path,
        {"expect": "success", "fake_credentials": {"NOT_A_CREDENTIAL": "circuitry-test-fake-x"}},
    )
    with pytest.raises(ValueError, match="not in"):
        harness.load_case(case_dir)


def test_load_case_rejects_a_fake_credentials_value_without_the_marker(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path,
        {"expect": "success", "fake_credentials": {"OPENAI_API_KEY": "sk-not-fake"}},
    )
    with pytest.raises(ValueError, match="must start with"):
        harness.load_case(case_dir)


def test_load_case_rejects_non_dict_fake_credentials(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "fake_credentials": ["x"]})
    with pytest.raises(ValueError, match="'fake_credentials' must be a mapping"):
        harness.load_case(case_dir)


def test_load_case_accepts_well_formed_third_party_text(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path,
        {
            "expect": "success",
            "third_party_text": [{"location": "meta.error", "pattern": ".*"}],
        },
    )
    metadata = harness.load_case(case_dir)
    assert metadata["third_party_text"] == [{"location": "meta.error", "pattern": ".*"}]


def test_load_case_rejects_third_party_text_missing_a_key(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path,
        {"expect": "success", "third_party_text": [{"location": "meta.error"}]},
    )
    with pytest.raises(ValueError, match="'third_party_text' must be a list"):
        harness.load_case(case_dir)


def test_load_case_rejects_third_party_text_with_an_empty_pattern(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path,
        {"expect": "success", "third_party_text": [{"location": "meta.error", "pattern": ""}]},
    )
    with pytest.raises(ValueError, match="'third_party_text' must be a list"):
        harness.load_case(case_dir)


def test_apply_third_party_text_replaces_a_matching_leaf() -> None:
    state = {"meta": {"error": "curl: (7) Failed to connect to host port 1: Connection refused"}}
    specs = [{"location": "meta.error", "pattern": r"curl: \(7\).*"}]
    result = apply_third_party_text(state, specs)
    assert result["meta"]["error"] == "<THIRD_PARTY_TEXT>"
    # The original is untouched -- a deep copy, not an in-place mutation.
    assert state["meta"]["error"] != "<THIRD_PARTY_TEXT>"


def test_apply_third_party_text_raises_when_the_pattern_does_not_fullmatch() -> None:
    state = {"meta": {"error": "something unrelated"}}
    specs = [{"location": "meta.error", "pattern": r"curl: \(7\).*"}]
    with pytest.raises(AssertionError, match="does not fullmatch"):
        apply_third_party_text(state, specs)


def test_apply_third_party_text_indexes_a_list_segment() -> None:
    state = {"attempts": [{"error": "curl: (7) refused"}]}
    specs = [{"location": "attempts.0.error", "pattern": r"curl: \(7\).*"}]
    result = apply_third_party_text(state, specs)
    assert result["attempts"][0]["error"] == "<THIRD_PARTY_TEXT>"


def test_load_case_rejects_malformed_known_divergence(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path,
        {
            "expect": "success",
            "engines": ["python", "electricity"],
            "known_divergence": {"location": "prime.x.value"},
        },
    )
    with pytest.raises(ValueError, match="'known_divergence' must be a mapping"):
        harness.load_case(case_dir)


def test_load_case_rejects_known_divergence_on_a_failure_case(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path,
        {
            "expect": "failure",
            "engines": ["python", "electricity"],
            "known_divergence": {"location": "prime.x.value", "electricity_value": 1},
        },
    )
    with pytest.raises(ValueError, match="only applies to a 'success' case"):
        harness.load_case(case_dir)


def test_load_case_rejects_known_divergence_with_one_engine(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path,
        {
            "expect": "success",
            "engines": ["python"],
            "known_divergence": {"location": "prime.x.value", "electricity_value": 1},
        },
    )
    with pytest.raises(ValueError, match="requires 'engines' to list both"):
        harness.load_case(case_dir)


def test_load_case_accepts_well_formed_known_divergence(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path,
        {
            "expect": "success",
            "engines": ["python", "electricity"],
            "known_divergence": {"location": "prime.x.value", "electricity_value": "other"},
        },
    )
    metadata = harness.load_case(case_dir)
    assert metadata["known_divergence"] == {
        "location": "prime.x.value",
        "electricity_value": "other",
    }


def test_load_case_rejects_non_bool_electricity_preview_ok(tmp_path: Path) -> None:
    case_dir = _write_case(
        tmp_path, {"expect": "success", "electricity_preview_ok": "yes"}
    )
    with pytest.raises(ValueError, match="'electricity_preview_ok' must be a bool"):
        harness.load_case(case_dir)


def test_load_case_rejects_non_bool_sort_http_requests(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "sort_http_requests": "yes"})
    with pytest.raises(ValueError, match="'sort_http_requests' must be a bool"):
        harness.load_case(case_dir)


def test_load_case_rejects_also_pretty_with_mock_http_fixture(tmp_path: Path) -> None:
    """PR #455 review finding P2-4: the --pretty re-run gets neither a
    fresh mock server nor a fresh leftover-replies path, and runs after the
    server has already stopped."""
    case_dir = _write_case(
        tmp_path,
        {
            "expect": "success",
            "also_pretty": True,
            "mock_http_fixture": "mock-http.yaml",
        },
    )
    with pytest.raises(ValueError, match="'also_pretty' cannot combine with"):
        harness.load_case(case_dir)


def test_load_case_rejects_known_divergence_with_also_pretty(tmp_path: Path) -> None:
    """PR #455 review finding P2-4: the --pretty comparison never applies
    the known_divergence override."""
    case_dir = _write_case(
        tmp_path,
        {
            "expect": "success",
            "engines": ["python", "electricity"],
            "also_pretty": True,
            "known_divergence": {"location": "prime.x.value", "electricity_value": 1},
        },
    )
    with pytest.raises(ValueError, match="'known_divergence' cannot combine with"):
        harness.load_case(case_dir)


def test_load_case_accepts_electricity_preview_ok_on_a_failure_case(tmp_path: Path) -> None:
    """Issue #511's own review: a failure case may also declare
    `electricity_preview_ok` -- a document whose real failure electricity
    can't reach yet because the preview refuses it first (e.g. a
    Circuitry-known adapter with no Rust builder), the same way a success
    case can."""
    case_dir = _write_case(
        tmp_path,
        {
            "expect": "failure",
            "engines": ["python", "electricity"],
            "electricity_preview_ok": True,
        },
    )
    metadata = harness.load_case(case_dir)
    assert metadata["electricity_preview_ok"] is True


def test_load_case_rejects_electricity_preview_ok_without_electricity_in_engines(
    tmp_path: Path,
) -> None:
    case_dir = _write_case(
        tmp_path,
        {
            "expect": "success",
            "engines": ["python"],
            "electricity_preview_ok": True,
        },
    )
    with pytest.raises(ValueError, match="'electricity_preview_ok' only applies to"):
        harness.load_case(case_dir)


def test_case_redaction_replacements_only_redacts_host_port_forms(tmp_path: Path) -> None:
    """PR #455 review finding P2-3: a bare port-number replacement could
    also corrupt a run id/UUID or a compact timestamp that happens to
    contain the same digits as a substring. `case_redaction_replacements`
    only ever redacts the port inside an actual `host:port` form."""
    case_dir = tmp_path / "case"
    case_dir.mkdir()
    port = 54321
    replacements = harness.case_redaction_replacements(case_dir, mock_http_port=port)

    uuid_containing_port_digits = f"a1b2c3d4-{port}-4a2b-9c3d-abcdefabcdef"
    assert (
        redact_runtime_strings(uuid_containing_port_digits, replacements)
        == uuid_containing_port_digits
    )
    timestamp_containing_port_digits = f"20260101_12{port}"
    assert (
        redact_runtime_strings(timestamp_containing_port_digits, replacements)
        == timestamp_containing_port_digits
    )

    assert (
        redact_runtime_strings(f"http://127.0.0.1:{port}/v1", replacements)
        == "http://127.0.0.1:<MOCK_PORT>/v1"
    )
    assert (
        redact_runtime_strings(f"localhost:{port}", replacements) == "localhost:<MOCK_PORT>"
    )


def test_materialize_cli_args_substitutes_mock_port() -> None:
    args = harness.materialize_cli_args(
        ["-e", f"url=http://127.0.0.1:{harness.MOCK_HTTP_PORT_PLACEHOLDER}/data"],
        mock_http_port=54321,
    )
    assert args == ["-e", "url=http://127.0.0.1:54321/data"]


def test_materialize_config_substitutes_mock_port(tmp_path: Path) -> None:
    case_dir = tmp_path / "case"
    case_dir.mkdir()
    (case_dir / "config.json").write_text(
        json.dumps(
            {
                "runtime": {
                    "adapters": {
                        "lmstudio": {
                            "base_url": f"http://127.0.0.1:{harness.MOCK_HTTP_PORT_PLACEHOLDER}/v1"
                        }
                    }
                }
            }
        ),
        encoding="utf-8",
    )
    materialized = harness.materialize_config(
        case_dir, "config.json", mock_http_port=54321, tmp_dir=tmp_path
    )
    data = json.loads(materialized.read_text(encoding="utf-8"))
    assert data["runtime"]["adapters"]["lmstudio"]["base_url"] == "http://127.0.0.1:54321/v1"


def test_unused_scripted_reply_fails_the_case(tmp_path: Path) -> None:
    """Issue #450's acceptance criterion: a deliberately unused scripted
    reply fails a case on the Python side. Runs a real `cof run` subprocess
    (the same way `test_python_runner.py` does) with a replies file that
    answers a path the document never asks, and asserts the harness's own
    leftover-replies export reports it -- exactly the check
    `test_python_runner.py::test_case` makes for every case."""
    case_dir = tmp_path / "case"
    case_dir.mkdir()
    (case_dir / "orchestration.yml").write_text(
        yaml.dump(
            {
                "name": "unused-reply",
                "adapter": "scripted",
                "model": "test",
                "effects": [{"type": "prompt", "name": "asked", "template": "hi"}],
            }
        ),
        encoding="utf-8",
    )
    (case_dir / "config.json").write_text(
        json.dumps(
            {"runtime": {"adapters": {"scripted": {"replies_file": "replies.yaml"}}}}
        ),
        encoding="utf-8",
    )
    (case_dir / "replies.yaml").write_text(
        yaml.dump(
            {
                "prime.asked": [{"text": "answered"}],
                "prime.never_asked": [{"text": "nobody wants this"}],
            }
        ),
        encoding="utf-8",
    )
    metadata = {
        "orchestration": "orchestration.yml",
        "config": "config.json",
        "cli_args": [],
        "timeout_seconds": harness.DEFAULT_TIMEOUT_SECONDS,
    }

    home_dir = tmp_path / "home"
    home_dir.mkdir()
    out_path = tmp_path / "out.json"
    leftover_replies_path = tmp_path / "leftover.json"

    result = harness.run_case(
        case_dir,
        metadata,
        out_path=out_path,
        home_dir=home_dir,
        leftover_replies_path=leftover_replies_path,
    )

    assert result.returncode == 0, result.stderr
    # The actual check `test_python_runner.py`/`test_electricity_runner.py`/
    # the generator all make for every case -- proving it fails a case that
    # leaves a reply unused (not just that the export reports one): without
    # `assert_no_leftover_replies`'s own leftover check, this call would
    # pass silently.
    with pytest.raises(AssertionError, match=r"prime\.never_asked"):
        harness.assert_no_leftover_replies(
            leftover_replies_path, case_dir=case_dir, metadata=metadata, case_name="unused-reply"
        )


def test_a_prompt_under_a_false_if_still_reports_its_unused_reply(tmp_path: Path) -> None:
    """PR #455 review finding 1's "missed failures" case: a `provider:
    scripted` prompt sitting under an `if` that never takes that branch at
    run time is still walked statically by preflight (`cli.runtime_shim.
    preflight` checks every adapter reference in the document, regardless
    of any conditional around it), so its instance still registers and
    reports the reply nothing ever consumed -- the run itself never
    dispatches the prompt at all."""
    case_dir = tmp_path / "case"
    case_dir.mkdir()
    (case_dir / "orchestration.yml").write_text(
        yaml.dump(
            {
                "name": "false-if-scripted",
                "adapter": "scripted",
                "model": "test",
                "effects": [
                    {
                        "type": "if",
                        "name": "gate",
                        "if": {"mode": "cel", "expr": "false"},
                        "then": [
                            {"type": "prompt", "name": "asked", "template": "hi"}
                        ],
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    (case_dir / "config.json").write_text(
        json.dumps(
            {"runtime": {"adapters": {"scripted": {"replies_file": "replies.yaml"}}}}
        ),
        encoding="utf-8",
    )
    (case_dir / "replies.yaml").write_text(
        yaml.dump({"prime.gate.asked": [{"text": "never consumed"}]}),
        encoding="utf-8",
    )
    metadata = {
        "orchestration": "orchestration.yml",
        "config": "config.json",
        "cli_args": [],
        "timeout_seconds": harness.DEFAULT_TIMEOUT_SECONDS,
    }

    home_dir = tmp_path / "home"
    home_dir.mkdir()
    out_path = tmp_path / "out.json"
    leftover_replies_path = tmp_path / "leftover.json"

    result = harness.run_case(
        case_dir,
        metadata,
        out_path=out_path,
        home_dir=home_dir,
        leftover_replies_path=leftover_replies_path,
    )

    assert result.returncode == 0, result.stderr
    with pytest.raises(AssertionError, match=r"prime\.gate\.asked"):
        harness.assert_no_leftover_replies(
            leftover_replies_path,
            case_dir=case_dir,
            metadata=metadata,
            case_name="false-if-scripted",
        )


def test_read_leftover_replies_reports_everything_when_export_is_absent(
    tmp_path: Path,
) -> None:
    """An absent export file means nothing was consumed -- either nothing in
    the run ever loaded the replies file, or (lane F1's own electricity
    engine, once it runs this far) the engine doesn't implement the export
    at all. Either way `read_leftover_replies` must report every reply the
    file configures, never `{}`."""
    replies_path = tmp_path / "replies.yaml"
    replies_path.write_text(
        yaml.dump({"prime.a": [{"text": "x"}, {"text": "y"}], "prime.b": [{"text": "z"}]}),
        encoding="utf-8",
    )
    missing_export_path = tmp_path / "leftover.json"

    leftover = harness.read_leftover_replies(missing_export_path, replies_file=replies_path)

    assert leftover == {"prime.a": 2, "prime.b": 1}


def test_read_leftover_replies_absent_export_and_no_replies_file_is_empty(
    tmp_path: Path,
) -> None:
    """A case whose config doesn't name a scripted replies file at all has
    nothing to check -- an absent export there means exactly `{}`, not
    every case's worth of imaginary replies."""
    missing_export_path = tmp_path / "leftover.json"

    assert harness.read_leftover_replies(missing_export_path, replies_file=None) == {}
