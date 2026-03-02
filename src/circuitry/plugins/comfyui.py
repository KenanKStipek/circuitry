from __future__ import annotations

import base64
import json
import os
import shlex
import subprocess
import time
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any

from .base import ToolResult


@dataclass(frozen=True)
class ComfyUIPlugin:
    name: str = "comfyui"
    base_url: str = "http://localhost:8188"
    default_model: str = ""
    default_image_output: str = "path"
    image_dir: str = "./output/images"
    poll_interval: float = 2.0

    def _curl_json(
        self,
        *,
        url: str,
        method: str = "GET",
        payload: dict[str, Any] | None = None,
        timeout_seconds: int = 30,
    ) -> dict[str, Any]:
        cmd = [
            "curl",
            "--silent",
            "--show-error",
            "--fail-with-body",
            "--max-time",
            str(int(timeout_seconds)),
        ]

        if method.upper() == "POST":
            cmd += [
                "-H",
                "Content-Type: application/json",
                "-d",
                json.dumps(payload or {}),
            ]

        cmd.append(url)

        try:
            proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
        except FileNotFoundError as e:
            raise RuntimeError("curl is not installed or not on PATH") from e

        if proc.returncode != 0:
            cmd_str = " ".join(shlex.quote(c) for c in cmd)
            err = (proc.stderr or proc.stdout or "").strip()
            raise RuntimeError(
                f"curl failed (exit {proc.returncode}). cmd={cmd_str}. error={err}"
            )

        try:
            return json.loads(proc.stdout)
        except json.JSONDecodeError as e:
            raise RuntimeError(
                f"curl returned non-JSON response: {proc.stdout[:200]}"
            ) from e

    def _curl_bytes(self, *, url: str, timeout_seconds: int = 60) -> bytes:
        cmd = [
            "curl",
            "--silent",
            "--show-error",
            "--fail-with-body",
            "--max-time",
            str(int(timeout_seconds)),
            url,
        ]

        try:
            proc = subprocess.run(cmd, capture_output=True, check=False)
        except FileNotFoundError as e:
            raise RuntimeError("curl is not installed or not on PATH") from e

        if proc.returncode != 0:
            cmd_str = " ".join(shlex.quote(c) for c in cmd)
            err = (proc.stderr or b"").decode(errors="replace").strip()
            raise RuntimeError(
                f"curl failed (exit {proc.returncode}). cmd={cmd_str}. error={err}"
            )

        return proc.stdout

    def _seed(self, params: dict[str, Any]) -> int:
        s = params.get("seed")
        if s is not None and isinstance(s, int) and s >= 0:
            return s
        return int(time.time() * 1000) % (2**32)

    def _upload_image(self, *, image_path: str, timeout_seconds: int = 30) -> str:
        """Upload a local image to ComfyUI's input folder. Returns the uploaded filename."""
        base = self.base_url.rstrip("/")
        cmd = [
            "curl",
            "--silent",
            "--show-error",
            "--fail-with-body",
            "--max-time",
            str(int(timeout_seconds)),
            "-X", "POST",
            "-F", f"image=@{image_path}",
            "-F", "type=input",
            "-F", "overwrite=true",
            f"{base}/upload/image",
        ]
        try:
            proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
        except FileNotFoundError as e:
            raise RuntimeError("curl is not installed or not on PATH") from e

        if proc.returncode != 0:
            cmd_str = " ".join(shlex.quote(c) for c in cmd)
            err = (proc.stderr or proc.stdout or "").strip()
            raise RuntimeError(
                f"curl failed uploading image (exit {proc.returncode}). cmd={cmd_str}. error={err}"
            )

        try:
            result = json.loads(proc.stdout)
        except json.JSONDecodeError as e:
            raise RuntimeError(
                f"ComfyUI /upload/image returned non-JSON: {proc.stdout[:200]}"
            ) from e

        name = result.get("name")
        if not name:
            raise RuntimeError(
                f"ComfyUI /upload/image response missing 'name': {result}"
            )
        return name

    def _build_workflow(
        self, *, checkpoint: str, prompt: str, params: dict[str, Any]
    ) -> dict[str, Any]:
        return {
            "4": {
                "class_type": "CheckpointLoaderSimple",
                "inputs": {"ckpt_name": checkpoint},
            },
            "5": {
                "class_type": "EmptyLatentImage",
                "inputs": {
                    "width": int(params.get("width", 512)),
                    "height": int(params.get("height", 512)),
                    "batch_size": 1,
                },
            },
            "6": {
                "class_type": "CLIPTextEncode",
                "inputs": {"text": prompt, "clip": ["4", 1]},
            },
            "7": {
                "class_type": "CLIPTextEncode",
                "inputs": {
                    "text": params.get("negative_prompt", ""),
                    "clip": ["4", 1],
                },
            },
            "3": {
                "class_type": "KSampler",
                "inputs": {
                    "model": ["4", 0],
                    "positive": ["6", 0],
                    "negative": ["7", 0],
                    "latent_image": ["5", 0],
                    "seed": self._seed(params),
                    "steps": int(params.get("steps", 20)),
                    "cfg": float(params.get("cfg", 7.0)),
                    "sampler_name": params.get("sampler_name", "euler"),
                    "scheduler": params.get("scheduler", "normal"),
                    "denoise": float(params.get("denoise", 1.0)),
                },
            },
            "8": {
                "class_type": "VAEDecode",
                "inputs": {"samples": ["3", 0], "vae": ["4", 2]},
            },
            "9": {
                "class_type": "SaveImage",
                "inputs": {"filename_prefix": "circuitry", "images": ["8", 0]},
            },
        }

    def _build_workflow_img2img(
        self,
        *,
        checkpoint: str,
        prompt: str,
        params: dict[str, Any],
        uploaded_filename: str,
    ) -> dict[str, Any]:
        """img2img workflow: load reference image → VAEEncode → KSampler with denoise < 1."""
        return {
            "4": {
                "class_type": "CheckpointLoaderSimple",
                "inputs": {"ckpt_name": checkpoint},
            },
            # Load the uploaded reference image
            "10": {
                "class_type": "LoadImage",
                "inputs": {"image": uploaded_filename, "upload": "image"},
            },
            # Encode reference image into latent space
            "11": {
                "class_type": "VAEEncode",
                "inputs": {"pixels": ["10", 0], "vae": ["4", 2]},
            },
            "6": {
                "class_type": "CLIPTextEncode",
                "inputs": {"text": prompt, "clip": ["4", 1]},
            },
            "7": {
                "class_type": "CLIPTextEncode",
                "inputs": {
                    "text": params.get("negative_prompt", ""),
                    "clip": ["4", 1],
                },
            },
            "3": {
                "class_type": "KSampler",
                "inputs": {
                    "model": ["4", 0],
                    "positive": ["6", 0],
                    "negative": ["7", 0],
                    "latent_image": ["11", 0],
                    "seed": self._seed(params),
                    "steps": int(params.get("steps", 20)),
                    "cfg": float(params.get("cfg", 7.0)),
                    "sampler_name": params.get("sampler_name", "euler"),
                    "scheduler": params.get("scheduler", "normal"),
                    "denoise": float(params.get("denoise", 0.80)),
                },
            },
            "8": {
                "class_type": "VAEDecode",
                "inputs": {"samples": ["3", 0], "vae": ["4", 2]},
            },
            "9": {
                "class_type": "SaveImage",
                "inputs": {"filename_prefix": "circuitry", "images": ["8", 0]},
            },
        }

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        prompt_text = params.get("prompt")
        if not prompt_text:
            raise ValueError("ComfyUIPlugin requires 'prompt' in params.")

        checkpoint = params.get("model") or self.default_model
        if not checkpoint:
            raise RuntimeError(
                "ComfyUIPlugin requires a checkpoint name via params['model'] "
                "or 'default_model' in plugin config."
            )

        base = self.base_url.rstrip("/")
        reference_image: str | None = params.get("reference_image")
        if params.get("workflow"):
            workflow = params["workflow"]
        elif reference_image:
            uploaded_filename = self._upload_image(image_path=reference_image)
            workflow = self._build_workflow_img2img(
                checkpoint=checkpoint,
                prompt=prompt_text,
                params=params,
                uploaded_filename=uploaded_filename,
            )
        else:
            workflow = self._build_workflow(
                checkpoint=checkpoint, prompt=prompt_text, params=params
            )

        # Queue the prompt
        client_id = str(uuid.uuid4())
        queue_resp = self._curl_json(
            url=f"{base}/prompt",
            method="POST",
            payload={"prompt": workflow, "client_id": client_id},
            timeout_seconds=30,
        )
        prompt_id: str = queue_resp["prompt_id"]

        # Poll /history until complete
        deadline = time.monotonic() + timeout_seconds
        history: dict[str, Any] = {}
        while time.monotonic() < deadline:
            history = self._curl_json(
                url=f"{base}/history/{prompt_id}", timeout_seconds=10
            )
            if prompt_id in history:
                break
            time.sleep(self.poll_interval)
        else:
            raise RuntimeError(
                f"ComfyUI prompt {prompt_id!r} did not complete within {timeout_seconds}s"
            )

        # Find the first SaveImage output
        outputs = history[prompt_id].get("outputs", {})
        img_info: dict[str, Any] | None = None
        for node_output in outputs.values():
            images = node_output.get("images")
            if images:
                img_info = images[0]
                break

        if img_info is None:
            raise RuntimeError(
                f"ComfyUI prompt {prompt_id!r} completed but produced no image output"
            )

        # Download image bytes
        view_url = (
            f"{base}/view"
            f"?filename={img_info['filename']}"
            f"&subfolder={img_info.get('subfolder', '')}"
            f"&type={img_info.get('type', 'output')}"
        )
        image_bytes = self._curl_bytes(
            url=view_url, timeout_seconds=min(60, timeout_seconds)
        )

        # Determine output format
        image_output = (
            params.get("image_output")
            or self.default_image_output
            or "path"
        )
        image_dir = params.get("image_dir") or self.image_dir or "./output/images"

        if image_output == "base64":
            value: Any = base64.b64encode(image_bytes).decode()
        elif image_output == "url":
            value = view_url
        else:  # path (default)
            os.makedirs(image_dir, exist_ok=True)
            timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S")
            filename = f"comfyui_{timestamp}.png"
            filepath = os.path.join(image_dir, filename)
            with open(filepath, "wb") as f:
                f.write(image_bytes)
            value = filepath

        return ToolResult(
            value=value,
            raw=history[prompt_id],
            stdout=None,
            stderr=None,
            exit_code=None,
        )
