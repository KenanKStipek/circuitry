"""One on-disk read per file, per load/digest/gate/compile cycle (#405).

Each surface below owns its own cycle: `cof run`'s own top-level gate is
covered by `test_prompt_composition.py`'s
`test_run_uses_the_prompt_file_bytes_its_own_consent_gate_hashed`; this
module covers the remaining cycle owners -- `cof trust`, `cof check`, one
`use` dispatch of a child, and `cof run --resume`'s change check together
with the run it starts -- plus the 1 MiB prompt-file limit a shared cache
must never bypass.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any
from unittest.mock import MagicMock

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run, validate
from circuitry.core.prompt_files import MAX_PROMPT_FILE_BYTES

runner = CliRunner()


def _write(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def _mock_adapter(response: str = "mock reply") -> MagicMock:
    adapter = MagicMock()
    adapter.name = "mock"
    result = MagicMock()
    result.text = response
    result.raw = {}
    result.tokens_sent = 1
    result.tokens_received = 1
    adapter.generate.return_value = result
    return adapter


def _count_reads(monkeypatch: pytest.MonkeyPatch, *names: str) -> dict[str, int]:
    """Patches ``Path.open`` and ``builtins.open`` to count file-opens by
    name, for every name in *names* -- every other file opens normally,
    uncounted.

    ``Path.read_text``/``Path.read_bytes`` open through ``Path.open`` on
    every supported Python, while what ``Path.open`` itself calls differs
    (``io.open`` at call time on 3.11+, an accessor bound at import time
    on 3.10), so ``Path.open`` is the one chokepoint for both ``Path``
    methods; a bare ``open(path)`` goes through ``builtins.open``.
    ``io.open`` stays unpatched, so a ``Path`` read is never counted
    twice.
    """
    import builtins

    counts = dict.fromkeys(names, 0)
    real_path_open = Path.open
    real_builtin_open = builtins.open

    def count(file: object) -> None:
        try:
            name = Path(file).name  # type: ignore[arg-type]
        except TypeError:
            name = None
        if name in counts:
            counts[name] += 1

    def fake_path_open(self: Path, *args: Any, **kwargs: Any) -> Any:
        count(self)
        return real_path_open(self, *args, **kwargs)

    def fake_builtin_open(file: Any, *args: Any, **kwargs: Any) -> Any:
        count(file)
        return real_builtin_open(file, *args, **kwargs)

    monkeypatch.setattr(Path, "open", fake_path_open)
    monkeypatch.setattr(builtins, "open", fake_builtin_open)
    return counts


def test_cof_check_reads_the_document_and_its_prompt_file_once(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`cof check`'s own load/compile cycle (#405): the orchestration YAML
    and a prompt file it references must each be read from disk exactly
    once, not once for the document's own parse, again for preflight
    (only run here because a config is passed, exactly like a real `cof
    check`), and again for the compile step's `{file: ...}` resolution.
    """
    _write(tmp_path / "brief.md", "Say hi.")
    orch_path = _write(
        tmp_path / "chain.yml",
        "prompts:\n  brief: {file: brief.md}\n"
        "effects:\n  - type: yield\n    name: y\n    template: '{{> brief}}'\n",
    )
    counts = _count_reads(monkeypatch, "chain.yml", "brief.md")

    report = validate(orch_path, config=CircuitryConfig())

    assert report["ok"], report["errors"]
    assert counts == {"chain.yml": 1, "brief.md": 1}


def test_use_dispatch_reads_a_childs_prompt_file_once(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """One `use: path:` dispatch (#405): the consent pre-check and the
    actual load-before-compile both reach into the same child, through
    the one cache this dispatch owns, so its prompt file is read once
    for the whole dispatch, not once per internal call.
    """
    _write(tmp_path / "brief.md", "Say hi.")
    _write(
        tmp_path / "child.yml",
        "prompts:\n  brief: {file: brief.md}\n"
        "effects:\n  - type: yield\n    name: y\n    template: '{{> brief}}'\n",
    )
    orch_path = _write(
        tmp_path / "root.yml",
        "effects:\n  - type: use\n    name: child\n    path: child.yml\n",
    )
    counts = _count_reads(monkeypatch, "brief.md")

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=_mock_adapter(),
        )
    )

    assert result.ok, result.error
    assert counts == {"brief.md": 1}
    assert result.state["prime"]["child"]["y"]["value"] == "Say hi."


def test_cof_trust_reads_the_document_and_its_prompt_file_once(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`cof trust`'s own load/digest cycle (#405): the capabilities it
    shows and the digest it records consent under must come from the
    same read of the document and of every `{file: ...}` prompt source
    it names.
    """
    from circuitry.cli.trust import _trust_document

    _write(tmp_path / "brief.md", "Say hi.")
    orch_path = _write(
        tmp_path / "chain.yml",
        "prompts:\n  brief: {file: brief.md}\n"
        "effects:\n  - type: tool\n    name: sh\n    provider: shell\n"
        "    params: {command: 'echo {{> brief}}'}\n",
    )
    counts = _count_reads(monkeypatch, "chain.yml", "brief.md")

    _trust_document(orch_path, yes=True, store=tmp_path / "trust.json", config=None)

    assert counts == {"chain.yml": 1, "brief.md": 1}


def test_resume_change_check_and_the_run_it_starts_share_one_read(tmp_path: Path) -> None:
    """`cof run --resume`'s change check, together with the run it starts,
    is one cycle (#405): the document this check reads to decide the run
    may proceed is the exact same bytes that run then loads, digests and
    compiles -- never a second, independent read that could by then see
    different bytes.

    ``--skip-preflight`` keeps this test to that one cycle: preflight's
    own adapter/model probing re-reads the document on its own, entirely
    outside the consent/digest/compile cache this test is about, and
    always has.
    """
    _write(tmp_path / "brief.md", "Say hi.")
    orch_path = _write(
        tmp_path / "chain.yml",
        "prompts:\n  brief: {file: brief.md}\n"
        "effects:\n  - type: yield\n    name: y\n    template: '{{> brief}}'\n",
    )
    out = tmp_path / "run.json"

    first = runner.invoke(app, ["run", str(orch_path), "--out", str(out), "--skip-preflight"])
    assert first.exit_code == 0, first.stdout

    with pytest.MonkeyPatch.context() as mp:
        counts = _count_reads(mp, "chain.yml")
        resumed = runner.invoke(
            app,
            [
                "run",
                str(orch_path),
                "--state",
                str(out),
                "--resume",
                "x",
                "--skip-preflight",
            ],
        )

    assert resumed.exit_code == 0, resumed.stdout
    assert counts == {"chain.yml": 1}, (
        f"chain.yml was read {counts['chain.yml']} times across the resume cycle"
    )


def test_resume_legacy_fallback_reads_the_document_and_its_prompt_file_once(
    tmp_path: Path,
) -> None:
    """The legacy-hash fallback (#405) is part of the same cycle as the
    change check it falls back from: a saved state carrying a pre-#407
    hash makes `_resolve_resume_state` fall back to
    `core.resume._legacy_document_sha256` -- which must read the exact
    same bytes `document_content_digest` just did, from the same
    `candidate_file_cache`, not a second read of either the document or
    its referenced prompt file.
    """
    from circuitry.core.resume import _legacy_document_sha256

    _write(tmp_path / "brief.md", "Say hi.")
    orch_path = _write(
        tmp_path / "chain.yml",
        "prompts:\n  brief: {file: brief.md}\n"
        "effects:\n  - type: yield\n    name: y\n    template: '{{> brief}}'\n",
    )
    out = tmp_path / "run.json"

    first = runner.invoke(app, ["run", str(orch_path), "--out", str(out), "--skip-preflight"])
    assert first.exit_code == 0, first.stdout

    import json

    state = json.loads(out.read_text(encoding="utf-8"))
    legacy_hash = _legacy_document_sha256(orch_path)
    # The two algorithms must actually diverge here (a referenced prompt
    # file is exactly where they do) -- otherwise this fixture would
    # never exercise the fallback at all.
    assert legacy_hash != state["runtime"]["last_run"]["document_hash"]
    state["runtime"]["last_run"]["document_hash"] = legacy_hash
    out.write_text(json.dumps(state), encoding="utf-8")

    with pytest.MonkeyPatch.context() as mp:
        counts = _count_reads(mp, "chain.yml", "brief.md")
        resumed = runner.invoke(
            app,
            [
                "run",
                str(orch_path),
                "--state",
                str(out),
                "--resume",
                "x",
                "--skip-preflight",
            ],
        )

    assert resumed.exit_code == 0, resumed.stdout
    assert counts == {"chain.yml": 1, "brief.md": 1}, (
        f"read {counts} times across the resume cycle's legacy fallback"
    )


def test_run_rejects_a_prompt_file_over_the_byte_limit_even_once_cached(
    tmp_path: Path,
) -> None:
    """The 1 MiB prompt-file limit (#405): a shared file-read cache must
    never let an earlier, unchecked read (the consent digest's own
    best-effort walk) carry an over-limit file's bytes past the
    existence/regular-file/size checks `resolve_prompt_file` makes --
    those must run regardless of whether this cycle already has the
    bytes cached.
    """
    oversized = tmp_path / "huge.md"
    oversized.write_bytes(b"x" * (MAX_PROMPT_FILE_BYTES + 1))
    orch_path = _write(
        tmp_path / "chain.yml",
        "prompts:\n  huge: {file: huge.md}\n"
        "effects:\n  - type: yield\n    name: y\n    template: '{{> huge}}'\n",
    )

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=_mock_adapter(),
        )
    )

    assert not result.ok
    assert "byte limit" in (result.error or "")
