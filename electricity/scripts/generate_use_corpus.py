#!/usr/bin/env python3
"""Generate the golden corpus for lane I of issue #448
(`electricity-vm::exec::use_`, issue #469) by running Circuitry's own
real `core/use.py` functions against synthetic inputs.

`check_interface_inputs`'s own default/require/coerce matrix is already
golden-tested end to end against the top-level `-e` path by
`generate_compiler_load_corpus.py` (the same Rust function,
`electricity_compiler::apply_declared_inputs`, now also `pub` and
reused directly by `exec::use_`'s own interface check) -- this corpus
does not re-derive that whole matrix, only a couple of `UseRuntime.
execute` end-to-end cases that exercise it *through* a `use` effect's
own rendered-inputs path, which is a different namespace than the CLI
`-e` one.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI, `-c electricity/scripts/generator-constraints.txt`).
Usage: python3 generate_use_corpus.py [--check]
"""

from __future__ import annotations

import json
import struct
import sys
import tempfile
from pathlib import Path
from typing import Any

from circuitry.core.expect import ExpectDef
from circuitry.core.outputs import normalize_outputs
from circuitry.core.prompt import RetryPolicyDef
from circuitry.core.store import Store
from circuitry.core.use import (
    UseDefinition,
    UseRuntime,
    _clean_yaml_fences,
    _collect_child_errors,
    _graft_child_record,
    _relative_child_path,
    _render_inputs,
    _resolve_dot_path,
    _unresolved_references,
    _validate_inline_yaml,
)

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-vm"
    / "tests"
    / "golden"
    / "use_corpus.json"
)


# ---------------------------------------------------------------------
# Tagged value encoding (matches generate_tool_execute_corpus.py's)
# ---------------------------------------------------------------------


def encode(value: object) -> dict:
    if value is None:
        return {"t": "none"}
    if isinstance(value, bool):
        return {"t": "bool", "v": value}
    if isinstance(value, int):
        return {"t": "int", "v": str(value)}
    if isinstance(value, float):
        bits = struct.unpack("<Q", struct.pack("<d", value))[0]
        return {"t": "float", "bits": format(bits, "016x")}
    if isinstance(value, str):
        return {"t": "str", "v": value}
    if isinstance(value, list):
        return {"t": "list", "v": [encode(item) for item in value]}
    if isinstance(value, dict):
        return {"t": "dict", "v": [[encode(k), encode(v)] for k, v in value.items()]}
    raise TypeError(f"no tagged encoding for {type(value)!r}")


def _normalize_timestamps(value: object) -> object:
    """Same placeholder substitution as `generate_tool_execute_corpus.py`'s
    own copy -- keeps this generator's own real wall-clock timestamps out
    of the committed corpus."""
    if isinstance(value, dict):
        return {
            k: (
                "<TIMESTAMP>"
                if k in ("created_at", "completed_at") and isinstance(v, str)
                else _normalize_timestamps(v)
            )
            for k, v in value.items()
        }
    if isinstance(value, list):
        return [_normalize_timestamps(v) for v in value]
    return value


def _normalize_temp_paths(value: object, *replacements: str) -> object:
    """Replaces every occurrence of *replacements* (the generator's own
    fresh temp-directory path, longest first) with `<temp>` -- a
    `meta.resolved_path`/`meta.error` leaf naming a file this generator
    wrote under `tempfile.mkdtemp()` is otherwise a fresh, incomparable
    path on every run."""
    ordered = sorted(replacements, key=len, reverse=True)

    def _sub(text: str) -> str:
        for old in ordered:
            text = text.replace(old, "<temp>")
        return text

    if isinstance(value, str):
        return _sub(value)
    if isinstance(value, dict):
        return {k: _normalize_temp_paths(v, *replacements) for k, v in value.items()}
    if isinstance(value, list):
        return [_normalize_temp_paths(v, *replacements) for v in value]
    return value


# ---------------------------------------------------------------------
# Pure-function cases: _clean_yaml_fences, _validate_inline_yaml,
# the inline identity/digest, _resolve_dot_path, _collect_child_errors,
# _relative_child_path, _render_inputs/_unresolved_references,
# normalize_outputs.
# ---------------------------------------------------------------------


def clean_yaml_fences_cases() -> list[dict]:
    samples = [
        "```yaml\neffects: []\n```",
        "---\neffects: []\n---",
        "effects:\n  - type: tool\n```\n",
        "no fences at all",
        "```\n```\n",
    ]
    return [{"input": text, "output": _clean_yaml_fences(text)} for text in samples]


def validate_inline_yaml_cases() -> list[dict]:
    samples = {
        "valid": "effects:\n  - type: tool\n    name: step\n    provider: json\n    params: {mode: parse, input: '1'}\n",
        "not_a_mapping": "- 1\n- 2\n",
        "missing_effects_key": "interface: {}\n",
        "yaml_parse_error": "effects: [\n",
        "structural_error": "effects:\n  - type: tool\n    name: step\n    provider: json\n",
    }
    cases = []
    for name, text in samples.items():
        ok, errors = _validate_inline_yaml(text)
        cases.append({"case": name, "input": text, "ok": ok, "errors": errors})
    return cases


def inline_identity_cases() -> list[dict]:
    import hashlib

    samples = [
        "effects: []\n",
        "effects:\n  - type: tool\n    name: step\n    provider: json\n    params: {mode: parse, input: '1'}\n",
    ]
    cases = []
    for text in samples:
        digest = hashlib.sha256(text.encode("utf-8")).hexdigest()
        cases.append(
            {
                "cleaned": text,
                "digest": digest,
                "identity": f"inline:{digest[:16]}",
            }
        )
    return cases


def resolve_dot_path_cases() -> list[dict]:
    state = {"prime": {"step": {"value": "hi", "meta": {}}, "value": None, "meta": {}}}
    paths = ["prime.step.value", "prime.missing", "prime.step.meta.missing.deeper"]
    return [
        {"state": encode(state), "path": path, "result": encode(_resolve_dot_path(state, path))}
        for path in paths
    ]


def collect_child_errors_cases() -> list[dict]:
    samples = [
        {"value": None, "meta": {"error": None}},
        {
            "value": None,
            "meta": {"error": None},
            "step": {"value": None, "meta": {"error": "boom"}},
        },
        {
            "value": None,
            "meta": {"error": None},
            "outer": {
                "value": None,
                "meta": {"error": None},
                "inner": {"value": None, "meta": {"error": "nested boom"}},
            },
        },
    ]
    return [{"node": encode(node), "errors": _collect_child_errors(node)} for node in samples]


def relative_child_path_cases() -> list[dict]:
    paths = ["prime", "prime.step", "prime.outer.inner", "other"]
    return [{"path": path, "result": _relative_child_path(path)} for path in paths]


def render_inputs_cases() -> list[dict]:
    ctx = {"input": {"name": "Ada", "items": [10, 20, 30]}}
    samples = [
        {"literal": "{{input.name}}", "ref": {"from": "input.name"}},
        {"missing_ref": {"from": "input.missing"}},
        {"list_index": {"from": "input.items.1"}},
        {"nested_literal": {"a": {"b": "{{not rendered}}"}}},
    ]
    cases = []
    for inputs in samples:
        rendered = _render_inputs(inputs, ctx)
        unresolved = _unresolved_references(inputs, rendered)
        cases.append(
            {
                "inputs": encode(inputs),
                "ctx": encode(ctx),
                "rendered": encode(rendered),
                "unresolved": unresolved,
            }
        )
    return cases


def normalize_outputs_cases() -> list[dict]:
    samples: list[Any] = [
        None,
        {},
        {"result": "prime.step.value"},
        {"result": {"path": "prime.step.value"}},
        {"result": ""},
        {"result": {}},
        {"result": 5},
        "not a dict",
    ]
    cases = []
    for raw in samples:
        try:
            result = normalize_outputs(raw, context="Use effect 'x': child interface.outputs")
            cases.append({"raw": encode(raw), "raised": False, "result": result, "error": None})
        except ValueError as exc:
            cases.append({"raw": encode(raw), "raised": True, "result": None, "error": str(exc)})
    return cases


def graft_child_record_cases() -> list[dict]:
    node_before = {"value": {"result": "mapped"}, "meta": {}}
    child_state = {
        "prime": {
            "value": None,
            "meta": {},
            "step": {"value": "hi", "meta": {}},
        }
    }
    node = dict(node_before)
    _graft_child_record(node, child_state)
    return [
        {
            "node_before": encode(node_before),
            "child_state": encode(child_state),
            "node_after": encode(node),
        }
    ]


# ---------------------------------------------------------------------
# End-to-end UseRuntime.execute cases.
# ---------------------------------------------------------------------


def write_child(tmp: Path, name: str, text: str) -> Path:
    path = tmp / name
    path.write_text(text, encoding="utf-8")
    return path


def run_use_case(
    *,
    name: str,
    tmp: Path,
    defn_kwargs: dict[str, Any],
    ctx: dict[str, Any] | None = None,
    runtime_config: dict[str, Any] | None = None,
) -> dict:
    defn = UseDefinition(name=name, **defn_kwargs)
    store = Store(state={})
    runtime_config = dict(runtime_config or {})
    runtime_config.setdefault("_orchestration_dir", str(tmp))
    rt = UseRuntime(defn, adapter=None, model=None, runtime_config=runtime_config)
    error: str | None = None
    try:
        rt.execute(store=store, ctx=ctx if ctx is not None else store.state)
    except Exception as exc:  # recording whatever UseRuntime raises
        error = str(exc)
    node = _normalize_timestamps(store.state.get(name))
    return {
        "case": name,
        "raised": error is not None,
        "error": error,
        "node": encode(node),
    }


def end_to_end_cases() -> list[dict]:
    # A context manager, not a bare `tempfile.mkdtemp()`: every file this
    # generator writes under *tmp* is scratch, cleaned up on the way out
    # regardless of how `run_use_case` below exits.
    with tempfile.TemporaryDirectory(prefix="use-corpus-") as tmp_name:
        return _end_to_end_cases(Path(tmp_name))


def _end_to_end_cases(tmp: Path) -> list[dict]:
    cases: list[dict] = []
    # Both the as-given path and its `resolve()`d form: macOS's `/tmp`
    # is itself a symlink to `/private/tmp`, and `UseRuntime`'s own
    # cycle identity canonicalizes through it (`Path(identity).
    # resolve()`) -- an error text built from that identity carries
    # the resolved form even though every file this generator wrote
    # (and `_orchestration_dir` itself) uses the as-given one.
    temp_prefixes: list[str] = [str(tmp), str(tmp.resolve())]

    write_child(
        tmp,
        "child_basic.yml",
        "effects:\n"
        "  - type: tool\n"
        "    name: step\n"
        "    provider: json\n"
        "    params: {mode: stringify, input: hello-from-child}\n",
    )
    write_child(
        tmp,
        "child_with_interface.yml",
        "interface:\n"
        "  inputs:\n"
        "    name: {type: string, required: true}\n"
        "    times: {type: integer, default: 1}\n"
        "  outputs:\n"
        "    greeting: {path: prime.step.value}\n"
        "effects:\n"
        "  - type: tool\n"
        "    name: step\n"
        "    provider: json\n"
        "    params: {mode: stringify, input: {from: input.name}}\n",
    )
    write_child(
        tmp,
        "cycle_a.yml",
        "effects:\n  - type: use\n    name: to_b\n    path: cycle_b.yml\n",
    )
    write_child(
        tmp,
        "cycle_b.yml",
        "effects:\n  - type: use\n    name: to_a\n    path: cycle_a.yml\n",
    )
    write_child(
        tmp,
        "child_always_fails_expect.yml",
        "effects:\n"
        "  - type: tool\n"
        "    name: step\n"
        "    provider: json\n"
        "    params: {mode: stringify, input: ok}\n",
    )

    cases.append(
        run_use_case(
            name="path_full_passthrough",
            tmp=tmp,
            defn_kwargs={"path": "child_basic.yml"},
        )
    )
    cases.append(
        run_use_case(
            name="path_explicit_outputs",
            tmp=tmp,
            defn_kwargs={
                "path": "child_basic.yml",
                "outputs": {"greeting": "prime.step.value"},
            },
        )
    )
    cases.append(
        run_use_case(
            name="path_record_children",
            tmp=tmp,
            defn_kwargs={
                "path": "child_basic.yml",
                "outputs": {"greeting": "prime.step.value"},
            },
            runtime_config={"state": {"record_children": True}},
        )
    )
    cases.append(
        run_use_case(
            name="path_auto_outputs_from_interface",
            tmp=tmp,
            defn_kwargs={
                "path": "child_with_interface.yml",
                "inputs": {"name": "Ada"},
            },
        )
    )
    cases.append(
        run_use_case(
            name="path_interface_error_not_retried",
            tmp=tmp,
            defn_kwargs={
                "path": "child_with_interface.yml",
                "retries": RetryPolicyDef(max_attempts=3, backoff_ms=0),
            },
        )
    )
    cases.append(
        run_use_case(
            name="path_cycle",
            tmp=tmp,
            defn_kwargs={"path": "cycle_a.yml"},
        )
    )
    cases.append(
        run_use_case(
            name="retried_failing_expect_then_fails",
            tmp=tmp,
            defn_kwargs={
                "path": "child_always_fails_expect.yml",
                "retries": RetryPolicyDef(max_attempts=2, backoff_ms=0),
                "expect": ExpectDef(mode="cel", expr="false"),
            },
        )
    )
    cases.append(
        run_use_case(
            name="on_error_skip_absorbs_failure",
            tmp=tmp,
            defn_kwargs={
                "path": "child_always_fails_expect.yml",
                "on_error": "skip",
                "expect": ExpectDef(mode="cel", expr="false"),
            },
        )
    )
    cases.append(
        run_use_case(
            name="inline_quirk_q3",
            tmp=tmp,
            defn_kwargs={
                "inline": (
                    "effects:\n"
                    "  - type: tool\n"
                    "    name: step\n"
                    "    provider: json\n"
                    "    params: {mode: stringify, input: '{{input.v}}'}\n"
                ),
                "inputs": {"v": "X"},
            },
            ctx={"input": {"v": "Y"}},
        )
    )
    # Review finding 1 on PR #472: `meta.expect` must be written on a
    # *passing* attempt too, not only a failing one -- the corpus
    # already had `retried_failing_expect_then_fails` for the failure
    # half; this is the missing pass half, over the same child.
    cases.append(
        run_use_case(
            name="expect_cel_passes",
            tmp=tmp,
            defn_kwargs={
                "path": "child_basic.yml",
                "outputs": {"greeting": "prime.step.value"},
                "expect": ExpectDef(mode="cel", expr="value != null"),
            },
        )
    )
    # Review finding 2: the inline validation-failure text end to end --
    # `validate: true` wraps in "Inline orchestration validation
    # failed:\n  - ..." (and sets `meta.validation_errors`); `validate:
    # false` gives the bare message, no wrapper, no `validation_errors`.
    cases.append(
        run_use_case(
            name="inline_validation_failure_wrapped",
            tmp=tmp,
            defn_kwargs={
                "inline": "- just-a-list\n",
            },
        )
    )
    cases.append(
        run_use_case(
            name="inline_validation_failure_unwrapped",
            tmp=tmp,
            defn_kwargs={
                "inline": "- just-a-list\n",
                "validate": False,
            },
        )
    )

    return [
        {
            "case": c["case"],
            "raised": c["raised"],
            "error": _normalize_temp_paths(c["error"], *temp_prefixes),
            "node": _normalize_temp_paths(c["node"], *temp_prefixes),
        }
        for c in cases
    ]


def build_corpus() -> dict:
    return {
        "clean_yaml_fences_cases": clean_yaml_fences_cases(),
        "validate_inline_yaml_cases": validate_inline_yaml_cases(),
        "inline_identity_cases": inline_identity_cases(),
        "resolve_dot_path_cases": resolve_dot_path_cases(),
        "collect_child_errors_cases": collect_child_errors_cases(),
        "relative_child_path_cases": relative_child_path_cases(),
        "render_inputs_cases": render_inputs_cases(),
        "normalize_outputs_cases": normalize_outputs_cases(),
        "graft_child_record_cases": graft_child_record_cases(),
        "end_to_end_cases": end_to_end_cases(),
    }


def render(corpus: dict) -> str:
    return json.dumps(corpus, indent=2, ensure_ascii=False, sort_keys=True) + "\n"


def write_or_check(path: Path, text: str, *, check: bool) -> bool:
    path.parent.mkdir(parents=True, exist_ok=True)
    if check:
        current = path.read_text() if path.exists() else ""
        if current != text:
            print(f"{path} is stale; run without --check to regenerate", file=sys.stderr)
            return False
        return True
    path.write_text(text)
    print(f"wrote {path}")
    return True


def main() -> int:
    check = "--check" in sys.argv[1:]
    text = render(build_corpus())
    return 0 if write_or_check(OUTPUT, text, check=check) else 1


if __name__ == "__main__":
    raise SystemExit(main())
