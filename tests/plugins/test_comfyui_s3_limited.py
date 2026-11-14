"""A limited document never saves a file through `comfyui` or `s3` (#604):
both are tagged `network` only, but `comfyui`'s `image_output` (default
`path`) and `s3`'s `mode: get` with `path` write locally, the same gap
`http`'s `save_to` closed (#602).
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

import pytest

from circuitry.plugins.comfyui import (
    reject_limited_image_output,
)
from circuitry.plugins.comfyui import (
    static_param_errors as comfyui_param_errors,
)
from circuitry.plugins.s3_tool import (
    reject_limited_get_path,
)
from circuitry.plugins.s3_tool import (
    static_param_errors as s3_param_errors,
)

_CREDENTIAL_ENV_VARS = ("OPENAI_API_KEY", "CYBERDINER_TOKEN", "CYBERDINER_EXPO_URL")


# ---------------------------------------------------------------------------
# comfyui: static_param_errors / reject_limited_image_output.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "params",
    [
        {},
        {"image_output": "path"},
        {"image_output": "PATH"},
        {"image_dir": "./out"},
        {"image_output": {"from": "input.mode"}},
        {"image_output": "BASE64"},
        {"image_output": " base64"},
    ],
)
def test_comfyui_static_param_errors_rejects_local_output_when_limited(
    params: dict[str, object],
) -> None:
    assert comfyui_param_errors(params, trusted=False)


@pytest.mark.parametrize("image_output", ["base64", "url"])
def test_comfyui_static_param_errors_allows_non_writing_output_when_limited(
    image_output: str,
) -> None:
    assert comfyui_param_errors({"image_output": image_output}, trusted=False) == []


def test_comfyui_static_param_errors_allows_anything_when_trusted() -> None:
    assert comfyui_param_errors({}, trusted=True) == []
    assert comfyui_param_errors({"image_output": "path", "image_dir": "/tmp/x"}, trusted=True) == []


def test_comfyui_reject_limited_image_output_raises_for_default_path() -> None:
    with pytest.raises(ValueError, match="only a document run by path"):
        reject_limited_image_output({})


def test_comfyui_reject_limited_image_output_raises_for_image_dir_alone() -> None:
    with pytest.raises(ValueError, match="image_dir"):
        reject_limited_image_output({"image_output": "base64", "image_dir": "./out"})


def test_comfyui_reject_limited_image_output_allows_base64_or_url() -> None:
    reject_limited_image_output({"image_output": "base64"})
    reject_limited_image_output({"image_output": "url"})


def test_comfyui_reject_limited_image_output_rejects_case_variants() -> None:
    with pytest.raises(ValueError, match="only a document run by path"):
        reject_limited_image_output({"image_output": "BASE64"})
    with pytest.raises(ValueError, match="only a document run by path"):
        reject_limited_image_output({"image_output": " base64"})


# ---------------------------------------------------------------------------
# s3: static_param_errors / reject_limited_get_path.
# ---------------------------------------------------------------------------


def test_s3_static_param_errors_rejects_get_with_path_when_limited() -> None:
    errors = s3_param_errors({"mode": "get", "bucket": "b", "key": "k", "path": "x"}, trusted=False)
    assert errors
    assert "only a document run by path" in errors[0]


@pytest.mark.parametrize(
    "params",
    [
        {"mode": "list", "bucket": "b"},
        {"mode": "get", "bucket": "b", "key": "k"},
        {"mode": "put", "bucket": "b", "key": "k", "path": "x"},
        {"bucket": "b", "path": "x"},
    ],
)
def test_s3_static_param_errors_allows_non_writing_params_when_limited(
    params: dict[str, object],
) -> None:
    assert s3_param_errors(params, trusted=False) == []


def test_s3_static_param_errors_allows_anything_when_trusted() -> None:
    assert s3_param_errors({"mode": "get", "bucket": "b", "key": "k", "path": "x"}, trusted=True) == []


def test_s3_reject_limited_get_path_raises() -> None:
    with pytest.raises(ValueError, match="only a document run by path"):
        reject_limited_get_path({"mode": "get", "bucket": "b", "key": "k", "path": "x"})


def test_s3_reject_limited_get_path_allows_put_with_path() -> None:
    reject_limited_get_path({"mode": "put", "bucket": "b", "key": "k", "path": "x"})


def test_s3_reject_limited_get_path_allows_get_without_path() -> None:
    reject_limited_get_path({"mode": "get", "bucket": "b", "key": "k"})


# ---------------------------------------------------------------------------
# End to end through cof run-library: refused before any request.
# ---------------------------------------------------------------------------


def _cof(cwd: Path, *args: str) -> object:
    import subprocess

    home = cwd / "home"
    home.mkdir(exist_ok=True)
    env = {key: value for key, value in os.environ.items() if key not in _CREDENTIAL_ENV_VARS}
    env.update(HOME=str(home), COLUMNS="200")
    return subprocess.run(
        [sys.executable, "-m", "circuitry.cli.app", *args],
        capture_output=True,
        text=True,
        env=env,
        cwd=cwd,
        timeout=120,
        check=False,
    )


def _library_config(tmp_path: Path, folder: Path) -> Path:
    config = tmp_path / "config.json"
    config.write_text(
        json.dumps(
            {
                "runtime": {
                    "library": {
                        "sources": [{"type": "folder", "name": "local", "path": str(folder)}]
                    }
                }
            }
        ),
        encoding="utf-8",
    )
    return config


def test_a_limited_documents_comfyui_effect_is_refused_before_it_runs(tmp_path: Path) -> None:
    folder = tmp_path / "lib"
    folder.mkdir()
    (folder / "shared_doc.yml").write_text(
        """
effects:
  - type: tool
    name: generate
    provider: comfyui
    prompt: "a red apple"
    model: flux
    params:
      image_dir: ./out
""",
        encoding="utf-8",
    )
    config = _library_config(tmp_path, folder)
    proc = _cof(tmp_path, "run", "shared_doc", "--config", str(config), "--skip-preflight")
    assert proc.returncode != 0
    assert "only a document run by path" in proc.stdout + proc.stderr
    assert not (tmp_path / "out").exists()


def test_a_limited_documents_s3_effect_is_refused_before_it_runs(tmp_path: Path) -> None:
    folder = tmp_path / "lib"
    folder.mkdir()
    (folder / "shared_doc.yml").write_text(
        """
effects:
  - type: tool
    name: fetch
    provider: s3
    params:
      mode: get
      bucket: my-bucket
      key: my-key
      path: ./downloaded.bin
""",
        encoding="utf-8",
    )
    config = _library_config(tmp_path, folder)
    proc = _cof(tmp_path, "run", "shared_doc", "--config", str(config), "--skip-preflight")
    assert proc.returncode != 0
    assert "only a document run by path" in proc.stdout + proc.stderr
    assert not (tmp_path / "downloaded.bin").exists()


def test_a_limited_documents_comfyui_params_json_override_is_refused_at_dispatch(
    tmp_path: Path,
) -> None:
    """A literal ``image_output: base64`` passes `cof check`; `params_json`
    overriding it to ``path`` only comes to light once the runtime merges
    `params_json` over `params`, so this is caught by the dispatch-time
    gate, not the static one."""
    folder = tmp_path / "lib"
    folder.mkdir()
    (folder / "shared_doc.yml").write_text(
        """
effects:
  - type: tool
    name: generate
    provider: comfyui
    prompt: "a red apple"
    model: flux
    params:
      image_output: base64
    params_json: '{"image_output": "path"}'
""",
        encoding="utf-8",
    )
    config = _library_config(tmp_path, folder)
    proc = _cof(tmp_path, "run", "shared_doc", "--config", str(config), "--skip-preflight")
    assert proc.returncode != 0
    assert "only a document run by path" in proc.stdout + proc.stderr
    assert not (tmp_path / "output").exists()


def test_a_limited_documents_s3_params_json_override_is_refused_at_dispatch(
    tmp_path: Path,
) -> None:
    """A literal ``mode: get`` without ``path`` passes `cof check`;
    `params_json` adding ``path`` only comes to light once the runtime
    merges `params_json` over `params`, so this is caught by the
    dispatch-time gate, not the static one."""
    folder = tmp_path / "lib"
    folder.mkdir()
    (folder / "shared_doc.yml").write_text(
        """
effects:
  - type: tool
    name: fetch
    provider: s3
    params:
      mode: get
      bucket: my-bucket
      key: my-key
    params_json: '{"path": "./downloaded.bin"}'
""",
        encoding="utf-8",
    )
    config = _library_config(tmp_path, folder)
    proc = _cof(tmp_path, "run", "shared_doc", "--config", str(config), "--skip-preflight")
    assert proc.returncode != 0
    assert "only a document run by path" in proc.stdout + proc.stderr
    assert not (tmp_path / "downloaded.bin").exists()
