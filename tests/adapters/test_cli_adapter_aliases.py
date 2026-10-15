"""Aliases of the `claude_code` and `pi` adapters (#579): a
`runtime.adapters.<alias>` entry that sets `adapter` runs its base CLI with the
alias's own settings and `env`. Driven through real prompt effects against fake
CLI scripts; no test runs a real CLI or calls a real model."""

from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any

import pytest
import typer.testing
import yaml
from agent_cli_test_support import claude_result, pi_events, read_record, write_fake_cli

from circuitry.adapters.claude_code import ClaudeCodeAdapter
from circuitry.adapters.factory import build_adapter, configured_timeout_seconds
from circuitry.adapters.pi import PiAdapter
from circuitry.api import run_orchestration
from circuitry.cli.app import app
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.redaction import REDACTED, redact

MARKER = "alias-secret-marker-7f3a"

runner = typer.testing.CliRunner()


def _doc(tmp_path: Path, effects: list[dict[str, Any]], **top: Any) -> Path:
    doc = tmp_path / "orch.yml"
    doc.write_text(yaml.safe_dump({**top, "effects": effects}), encoding="utf-8")
    return doc


def _prompt(name: str, provider: str | None = None) -> dict[str, Any]:
    effect: dict[str, Any] = {"type": "prompt", "name": name, "template": f"say {name}"}
    if provider is not None:
        effect["provider"] = provider
    return effect


def _runtime(**adapters: dict[str, Any]) -> dict[str, Any]:
    return {"adapters": adapters}


def _run(doc: Path, runtime: dict[str, Any], **kwargs: Any) -> Any:
    config = CircuitryConfig(default_adapter="claude_code", default_model="m", runtime=runtime)
    return run_orchestration(
        orchestration_path=doc, config=config, raise_on_error=False, **kwargs
    )


@pytest.fixture
def fake_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    home = tmp_path / "home"
    home.mkdir()
    monkeypatch.setenv("HOME", str(home))
    return home


# ---------- resolution ----------


def test_an_alias_builds_its_base_with_its_own_settings_under_its_own_name() -> None:
    runtime = _runtime(
        claude_code={"binary": "base-claude", "default_model": "base-model"},
        claude_alt={
            "adapter": "claude_code",
            "binary": "alt-claude",
            "default_model": "alt-model",
            "unset_env": ["X"],
            "extra_args": ["--flag"],
            "env": {"A": "1"},
        },
        pi_alt={"adapter": "pi", "thinking": "low", "binary": "alt-pi"},
    )
    alt = build_adapter(adapter_name="claude_alt", runtime=runtime)
    assert isinstance(alt, ClaudeCodeAdapter)
    assert (alt.name, alt.binary, alt.default_model) == ("claude_alt", "alt-claude", "alt-model")
    assert alt.unset_env == ("X",) and alt.extra_args == ("--flag",) and alt.env == (("A", "1"),)
    pi_alt = build_adapter(adapter_name="pi_alt", runtime=runtime)
    assert isinstance(pi_alt, PiAdapter)
    assert (pi_alt.name, pi_alt.thinking, pi_alt.binary) == ("pi_alt", "low", "alt-pi")
    base = build_adapter(adapter_name="claude_code", runtime=runtime)
    assert isinstance(base, ClaudeCodeAdapter)
    assert (base.name, base.binary, base.default_model, base.env) == (
        "claude_code", "base-claude", "base-model", ()
    )  # fmt: skip


def test_an_alias_does_not_inherit_the_bases_settings() -> None:
    runtime = _runtime(
        claude_code={"binary": "base-claude", "unset_env": []},
        claude_alt={"adapter": "claude_code"},
    )
    alt = build_adapter(adapter_name="claude_alt", runtime=runtime)
    assert isinstance(alt, ClaudeCodeAdapter)
    assert alt.binary == "claude" and alt.unset_env != ()


def test_an_entry_without_adapter_keeps_its_behaviour() -> None:
    with pytest.raises(ValueError, match="Unknown adapter: 'claude_alt'"):
        build_adapter(adapter_name="claude_alt", runtime=_runtime(claude_alt={"binary": "x"}))


def test_the_adapters_repr_does_not_show_env_values() -> None:
    runtime = _runtime(a={"adapter": "pi", "env": {"V": MARKER}})
    assert MARKER not in repr(build_adapter(adapter_name="a", runtime=runtime))


# ---------- the child's environment ----------


def test_two_aliases_and_the_base_each_get_their_own_environment(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, fake_home: Path
) -> None:
    fakes = {}
    for label in ("base", "alpha", "beta"):
        path = tmp_path / f"{label}.json"
        fakes[label] = (
            write_fake_cli(tmp_path, f"fake-{label}", stdout=claude_result(result=label)),
            path,
        )
    monkeypatch.setenv("FAKE_CLI_RECORD", str(fakes["base"][1]))
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-base")
    monkeypatch.setenv("CLAUDECODE", "1")
    monkeypatch.setenv("KEEP_ME", "kept")
    runtime = _runtime(
        claude_code={"binary": str(fakes["base"][0])},
        claude_alpha={
            "adapter": "claude_code",
            "binary": str(fakes["alpha"][0]),
            "env": {
                "FAKE_CLI_RECORD": str(fakes["alpha"][1]),
                "ALIAS_PROBE": "alpha",
                "CONFIG_DIR": "~/.claude-alpha",
                "CLAUDECODE": "from-env",
            },
            "unset_env": ["KEEP_ME"],
        },
        claude_beta={
            "adapter": "claude_code",
            "binary": str(fakes["beta"][0]),
            "env": {
                "FAKE_CLI_RECORD": str(fakes["beta"][1]),
                "ALIAS_PROBE": "beta",
                "ANTHROPIC_API_KEY": "sk-from-env",
            },
        },
    )
    doc = _doc(
        tmp_path,
        [_prompt("one", "claude_alpha"), _prompt("two", "claude_beta:other-model"), _prompt("three")],
    )
    before = dict(os.environ)

    result = _run(doc, runtime)

    assert result.ok, result.error
    prime = result.state["prime"]
    assert [prime[n]["meta"]["adapter"] for n in ("one", "two", "three")] == [
        "claude_alpha", "claude_beta", "claude_code"
    ]  # fmt: skip
    assert [prime[n]["value"] for n in ("one", "two", "three")] == ["alpha", "beta", "base"]
    alpha = read_record(fakes["alpha"][1])["env"]
    beta = read_record(fakes["beta"][1])["env"]
    base = read_record(fakes["base"][1])["env"]
    assert alpha["ALIAS_PROBE"] == "alpha" and beta["ALIAS_PROBE"] == "beta"
    assert "ALIAS_PROBE" not in base and "CONFIG_DIR" not in base
    assert alpha["CONFIG_DIR"] == str(fake_home / ".claude-alpha")
    # env is applied after the removals: it wins over unset_env and the
    # always-removed parent-session variables
    assert beta["ANTHROPIC_API_KEY"] == "sk-from-env"
    assert alpha["CLAUDECODE"] == "from-env"
    assert "CLAUDECODE" not in beta and "CLAUDECODE" not in base
    assert "KEEP_ME" not in alpha and beta["KEEP_ME"] == "kept" and base["KEEP_ME"] == "kept"
    # alpha's own unset_env replaces the default list, base keeps the default
    assert alpha["ANTHROPIC_API_KEY"] == "sk-base" and "ANTHROPIC_API_KEY" not in base
    assert dict(os.environ) == before, "cof's own environment is unchanged"
    assert "ALIAS_PROBE" not in os.environ


def test_env_on_the_built_in_adapters_reaches_the_child(tmp_path: Path) -> None:
    fake = write_fake_cli(tmp_path, "fake-pi", stdout=pi_events("ok"))
    path = tmp_path / "rec.json"
    runtime = _runtime(
        pi={"binary": str(fake), "env": {"FAKE_CLI_RECORD": str(path), "PI_PROBE": "yes"}}
    )
    config = CircuitryConfig(default_adapter="pi", default_model="m", runtime=runtime)
    result = run_orchestration(
        orchestration_path=_doc(tmp_path, [_prompt("a")]), config=config, raise_on_error=False
    )
    assert result.ok, result.error
    assert read_record(path)["env"]["PI_PROBE"] == "yes"


def test_only_a_leading_tilde_slash_is_expanded(fake_home: Path) -> None:
    runtime = _runtime(
        a={"adapter": "pi", "env": {"A": "~/x", "B": "~", "C": "a/~/b", "D": "~other/x", "E": "$HOME/x"}}
    )
    adapter = build_adapter(adapter_name="a", runtime=runtime)
    assert isinstance(adapter, PiAdapter)
    assert dict(adapter.env) == {
        "A": f"{fake_home}/x", "B": "~", "C": "a/~/b", "D": "~other/x", "E": "$HOME/x"
    }  # fmt: skip


# ---------- config errors ----------


@pytest.mark.parametrize(
    ("entry", "message"),
    [
        (
            {"adapter": "nonesuch"},
            "runtime.adapters.bad.adapter must be claude_code or pi, got 'nonesuch'",
        ),
        (
            {"adapter": "openai"},
            "runtime.adapters.bad.adapter must be claude_code or pi, got 'openai'",
        ),
        ({"adapter": 5}, "runtime.adapters.bad.adapter must be claude_code or pi, got 5"),
        (
            {"adapter": "other"},
            (
                "runtime.adapters.bad.adapter must be claude_code or pi, got 'other', "
                "which is itself an alias"
            ),
        ),
        (
            {"adapter": "pi", "env": "A=1"},
            "runtime.adapters.bad.env must be a mapping of variable names to strings, got str",
        ),
        (
            {"adapter": "pi", "env": {"A": 1}},
            "runtime.adapters.bad.env.A must be a string, got int",
        ),
        (
            {"adapter": "pi", "env": {"1A": "x"}},
            (
                "runtime.adapters.bad.env has an invalid variable name '1A': "
                "a name matches [A-Za-z_][A-Za-z0-9_]*"
            ),
        ),
        (
            {"adapter": "pi", "env": {"A-B": "x"}},
            (
                "runtime.adapters.bad.env has an invalid variable name 'A-B': "
                "a name matches [A-Za-z_][A-Za-z0-9_]*"
            ),
        ),
        (
            {"adapter": "pi", "unset_env": "X"},
            "runtime.adapters.bad.unset_env must be a list of strings, got 'X'",
        ),
    ],
)
def test_a_bad_alias_is_a_config_error_naming_the_alias(
    entry: dict[str, Any], message: str
) -> None:
    runtime = _runtime(bad=entry, other={"adapter": "pi"})
    with pytest.raises(ValueError) as excinfo:
        build_adapter(adapter_name="bad", runtime=runtime)
    assert str(excinfo.value) == message


@pytest.mark.parametrize("name", ["openai", "ollama", "claude_code", "pi", "scripted"])
def test_an_alias_named_like_a_built_in_adapter_is_a_config_error(name: str) -> None:
    runtime = _runtime(**{name: {"adapter": "pi"}})
    with pytest.raises(ValueError) as excinfo:
        build_adapter(adapter_name=name, runtime=runtime)
    assert str(excinfo.value) == (
        f"runtime.adapters.{name}.adapter is set, but {name!r} is a built-in adapter; "
        "an alias needs a name of its own"
    )


def test_an_env_error_never_shows_the_value() -> None:
    for env in ({"BAD NAME": MARKER}, {"OK": [MARKER]}, {"OK": {"k": MARKER}}):
        with pytest.raises(ValueError) as excinfo:
            build_adapter(
                adapter_name="a", runtime=_runtime(a={"adapter": "pi", "env": env})
            )
        assert MARKER not in str(excinfo.value)


def test_a_bad_alias_fails_the_run_at_preflight_naming_it(tmp_path: Path) -> None:
    runtime = _runtime(claude_alt={"adapter": "claude_code", "env": {"A": 1}})
    result = _run(_doc(tmp_path, [_prompt("a", "claude_alt")]), runtime)
    assert not result.ok
    assert "adapter:claude_alt" in str(result.error)
    assert "runtime.adapters.claude_alt.env.A must be a string, got int" in str(result.error)


# ---------- check and doctor ----------


def test_check_reports_the_aliass_own_binary_naming_the_alias(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("PATH", "")
    config = tmp_path / "config.json"
    config.write_text(
        json.dumps(
            {
                "default_model": "m",
                "runtime": _runtime(
                    claude_alt={"adapter": "claude_code", "binary": "claude-alt-bin"},
                    pi_alt={"adapter": "pi"},
                ),
            }
        ),
        encoding="utf-8",
    )
    doc = _doc(tmp_path, [_prompt("a", "claude_alt"), _prompt("b", "pi_alt")])
    result = runner.invoke(app, ["check", str(doc), "--config", str(config)])
    assert result.exit_code != 0
    assert "adapter:claude_alt" in result.output and "binary:claude-alt-bin" in result.output
    assert "runtime.adapters.claude_alt.binary" in result.output
    assert "adapter:pi_alt" in result.output and "binary:pi" in result.output


def test_a_skippable_alias_with_a_missing_binary_is_a_warning_not_an_error(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("PATH", "")
    config = tmp_path / "config.json"
    config.write_text(
        json.dumps(
            {
                "default_model": "m",
                "runtime": _runtime(claude_alt={"adapter": "claude_code"}),
            }
        ),
        encoding="utf-8",
    )
    effect = {**_prompt("a", "claude_alt"), "on_error": "skip"}
    result = runner.invoke(
        app, ["check", str(_doc(tmp_path, [effect])), "--config", str(config)]
    )
    assert result.exit_code == 0, result.output
    assert "claude_alt" in result.output and "will skip" in result.output


def test_doctor_checks_the_aliass_binary_naming_the_alias(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("PATH", "")
    monkeypatch.setenv("CIRCUITRY_ENABLED_TOOLS", "")
    monkeypatch.setenv("CIRCUITRY_ENABLED_PLUGINS", "")
    config = tmp_path / "config.json"
    config.write_text(
        json.dumps(
            {
                "default_adapter": "claude_alt",
                "runtime": _runtime(
                    claude_alt={
                        "adapter": "claude_code",
                        "binary": "claude-alt-bin",
                        "env": {"CLAUDE_CONFIG_DIR": MARKER},
                    }
                ),
            }
        ),
        encoding="utf-8",
    )
    result = runner.invoke(app, ["doctor", "--config", str(config)], env={"COLUMNS": "400"})
    assert "claude_alt" in result.output
    assert "binary:claude-alt-bin" in result.output
    assert MARKER not in result.output


def test_doctor_reports_a_bad_alias_as_an_error_naming_it(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("CIRCUITRY_ENABLED_TOOLS", "")
    monkeypatch.setenv("CIRCUITRY_ENABLED_PLUGINS", "")
    config = tmp_path / "config.json"
    config.write_text(
        json.dumps({"runtime": _runtime(claude_alt={"adapter": "openai"})}), encoding="utf-8"
    )
    result = runner.invoke(
        app, ["doctor", "--config", str(config)], env={"COLUMNS": "400"}
    )
    assert result.exit_code != 0
    assert "runtime.adapters.claude_alt.adapter must be claude_code or pi, got 'openai'" in (
        result.output
    )


# ---------- secrecy ----------


def test_env_values_never_reach_state_events_or_error_text(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    ok_fake = write_fake_cli(tmp_path, "fake-ok", stdout=claude_result(result="fine"))
    bad_fake = write_fake_cli(
        tmp_path, "fake-bad", stdout=claude_result(is_error=True, result="login required"),
        exit_code=1,
    )  # fmt: skip
    config = tmp_path / "config.json"
    config.write_text(
        json.dumps(
            {
                "default_adapter": "claude_ok",
                "default_model": "m",
                "runtime": _runtime(
                    claude_ok={
                        "adapter": "claude_code",
                        "binary": str(ok_fake),
                        "env": {"CLAUDE_CONFIG_DIR": MARKER},
                    },
                    claude_bad={
                        "adapter": "claude_code",
                        "binary": str(bad_fake),
                        "env": {"CLAUDE_CONFIG_DIR": MARKER + "-bad"},
                    },
                ),
            }
        ),
        encoding="utf-8",
    )
    doc = _doc(
        tmp_path,
        [{**_prompt("good"), "provider": "claude_ok"}, {**_prompt("bad", "claude_bad"), "on_error": "continue"}],
    )
    out = tmp_path / "out.json"
    events = tmp_path / "events.jsonl"
    live = tmp_path / "live.json"
    result = runner.invoke(
        app,
        ["run", str(doc), "--config", str(config), "--out", str(out), "--events", str(events),
         "--live-state", str(live)],
    )  # fmt: skip
    assert result.exit_code == 0, result.output
    state = json.loads(out.read_text(encoding="utf-8"))
    failed = state["prime"]["bad"]["meta"]["fallback_attempts"][0]
    assert failed["adapter"] == "claude_bad" and "login required" in failed["error"]
    for text in (out.read_text(encoding="utf-8"), events.read_text(encoding="utf-8"),
                 live.read_text(encoding="utf-8"), result.output):  # fmt: skip
        assert MARKER not in text
    snapshot = state["runtime"]["effective_settings"]["runtime"]["adapters"]
    assert snapshot["claude_ok"]["env"] == {"CLAUDE_CONFIG_DIR": REDACTED}
    assert snapshot["claude_ok"]["binary"] == str(ok_fake)


def test_a_failed_run_does_not_echo_env_values(tmp_path: Path) -> None:
    runtime = _runtime(a={"adapter": "pi", "binary": "/nonexistent/pi", "env": {"V": MARKER}})
    result = _run(_doc(tmp_path, [_prompt("a", "a")]), runtime, out_path=tmp_path / "o.json")
    assert not result.ok
    assert MARKER not in str(result.error)
    assert MARKER not in (tmp_path / "o.json").read_text(encoding="utf-8")


def test_redact_blanks_env_values_under_adapters_and_keeps_the_names() -> None:
    value = {"adapters": {"a": {"adapter": "pi", "env": {"X": MARKER}, "binary": "pi"}}}
    assert redact(value) == {
        "adapters": {"a": {"adapter": "pi", "env": {"X": REDACTED}, "binary": "pi"}}
    }


# ---------- trust ----------


def test_a_limited_documents_alias_is_ignored_so_the_effect_fails_as_an_unknown_adapter(
    tmp_path: Path,
) -> None:
    fake = write_fake_cli(tmp_path, "fake-claude", stdout=claude_result(result="hi"))
    doc = _doc(
        tmp_path,
        [_prompt("a", "claude_alt")],
        runtime=_runtime(claude_alt={"adapter": "claude_code", "binary": str(fake)}),
    )
    result = _run(doc, {}, trust_document=False)
    assert not result.ok
    assert "Unknown adapter: 'claude_alt'" in str(result.error)
    assert any("Ignored runtime.adapters from the orchestration" in w for w in result.warnings)


def test_a_by_path_documents_alias_runs_and_is_listed_as_an_applied_host_setting(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = write_fake_cli(tmp_path, "fake-claude", stdout=claude_result(result="hi"))
    path = tmp_path / "rec.json"
    doc = _doc(
        tmp_path,
        [_prompt("a", "claude_alt")],
        runtime=_runtime(
            claude_alt={
                "adapter": "claude_code",
                "binary": str(fake),
                "env": {"FAKE_CLI_RECORD": str(path), "CLAUDE_CONFIG_DIR": MARKER},
            }
        ),
    )
    result = _run(doc, {}, trust_document=True)
    assert result.ok, result.error
    node = result.state["prime"]["a"]
    assert node["meta"]["adapter"] == "claude_alt" and node["value"] == "hi"
    assert read_record(path)["env"]["CLAUDE_CONFIG_DIR"] == MARKER
    notices = [w for w in result.warnings if w.startswith("Applied host settings")]
    assert len(notices) == 1
    assert "runtime.adapters.claude_alt" in notices[0] and MARKER not in notices[0]
    assert MARKER not in json.dumps(result.state)


# ---------- selecting an alias ----------


def test_an_alias_is_the_default_adapter_and_its_timeout_is_its_own(tmp_path: Path) -> None:
    fake = write_fake_cli(tmp_path, "fake-pi", stdout=pi_events("ok"))
    path = tmp_path / "rec.json"
    runtime = _runtime(
        pi_alt={
            "adapter": "pi",
            "binary": str(fake),
            "timeout_seconds": 77,
            "env": {"FAKE_CLI_RECORD": str(path)},
        }
    )
    config = CircuitryConfig(default_adapter="pi_alt", default_model="m", runtime=runtime)
    result = run_orchestration(
        orchestration_path=_doc(tmp_path, [_prompt("a")]), config=config, raise_on_error=False
    )
    assert result.ok, result.error
    assert result.state["prime"]["a"]["meta"]["adapter"] == "pi_alt"
    assert read_record(path)["argv"][0] == "-p"
    assert configured_timeout_seconds("pi_alt", runtime) == 77
    assert configured_timeout_seconds("pi", runtime) is None
