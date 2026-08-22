from __future__ import annotations

import json
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from .config import CircuitryConfig
from .orchestration_loader import ORCHESTRATION_SUFFIXES, load_orchestration_file


@dataclass(frozen=True)
class SharedOrchestrationAsset:
    asset_id: str
    version: str
    source: str
    file_path: Path
    metadata: dict[str, Any]
    orchestration: dict[str, Any]


@dataclass(frozen=True)
class ServiceProfile:
    name: str
    default_adapter: str | None
    default_model: str | None
    plugins: list[str]
    runtime_overrides: dict[str, Any]


def fetch_shared_orchestration(
    *,
    cfg: CircuitryConfig,
    asset_id: str,
    version: str | None,
    auth_token: str | None,
) -> SharedOrchestrationAsset:
    library_cfg = _library_config(cfg)
    _validate_auth(library_cfg=library_cfg, auth_token=auth_token)

    backend = str(library_cfg.get("backend") or "filesystem").strip().lower()
    if backend != "filesystem":
        raise ValueError(
            f"Unsupported shared library backend: {backend!r}. "
            "Supported backends: filesystem."
        )

    local_root_raw = str(library_cfg.get("local_root") or "").strip()
    if not local_root_raw:
        raise ValueError(
            "runtime.library.local_root is required for filesystem backend"
        )

    local_root = Path(local_root_raw)
    asset_path = local_root / asset_id
    if not asset_path.exists() or not asset_path.is_dir():
        raise FileNotFoundError(
            f"Shared library asset not found: {asset_id!r} under {local_root}"
        )

    resolved_version, resolved_file = _resolve_asset_version(
        asset_path=asset_path, requested_version=version
    )

    orch = load_orchestration_file(resolved_file)
    metadata = _load_metadata_sidecar(resolved_file)
    source = f"filesystem:{local_root}"
    envelope: dict[str, Any] = {
        "asset_id": asset_id,
        "version": resolved_version,
        "source": source,
        "path": str(resolved_file),
        "retrieved_at": datetime.now(timezone.utc).isoformat(),
        "metadata": metadata,
    }

    return SharedOrchestrationAsset(
        asset_id=asset_id,
        version=resolved_version,
        source=source,
        file_path=resolved_file,
        metadata=envelope,
        orchestration=orch,
    )


def resolve_service_profile(
    *, cfg: CircuitryConfig, profile_name: str | None
) -> ServiceProfile | None:
    if not profile_name:
        return None

    library_cfg = _library_config(cfg)
    profiles = library_cfg.get("service_profiles")
    if not isinstance(profiles, dict):
        raise ValueError(
            "No service profiles configured. Define runtime.library.service_profiles."
        )

    raw = profiles.get(profile_name)
    if not isinstance(raw, dict):
        available = ", ".join(sorted(str(k) for k in profiles))
        raise ValueError(
            f"Unknown service profile: {profile_name!r}. Available: {available}"
        )

    default_adapter_raw = raw.get("default_adapter")
    default_model_raw = raw.get("default_model")
    runtime_overrides_raw = raw.get("runtime") or {}
    plugins_raw = raw.get("plugins") or []

    if runtime_overrides_raw and not isinstance(runtime_overrides_raw, dict):
        raise ValueError(f"Service profile {profile_name!r} runtime must be an object.")
    if plugins_raw and not isinstance(plugins_raw, list):
        raise ValueError(f"Service profile {profile_name!r} plugins must be a list.")

    return ServiceProfile(
        name=profile_name,
        default_adapter=(
            str(default_adapter_raw).strip()
            if default_adapter_raw is not None
            else None
        ),
        default_model=(
            str(default_model_raw).strip() if default_model_raw is not None else None
        ),
        plugins=[str(p) for p in plugins_raw],
        runtime_overrides=dict(runtime_overrides_raw),
    )


def apply_service_profile(
    *,
    cfg: CircuitryConfig,
    profile: ServiceProfile | None,
) -> CircuitryConfig:
    if profile is None:
        return cfg

    merged_runtime = _merge_dict(cfg.runtime, profile.runtime_overrides)
    merged_plugins = _dedupe_plugins(cfg.plugins + profile.plugins)

    return CircuitryConfig(
        default_model=profile.default_model or cfg.default_model,
        default_adapter=profile.default_adapter or cfg.default_adapter,
        plugins=merged_plugins,
        runtime=merged_runtime,
    )


def _library_config(cfg: CircuitryConfig) -> dict[str, Any]:
    runtime = cfg.runtime or {}
    library = runtime.get("library")
    if not isinstance(library, dict):
        raise ValueError(
            "Shared library is not configured. Set runtime.library in config.json"
        )
    return library


def _validate_auth(*, library_cfg: dict[str, Any], auth_token: str | None) -> None:
    required_token = str(library_cfg.get("auth_token") or "").strip()
    if not required_token:
        return
    provided = (auth_token or "").strip()
    if provided != required_token:
        raise PermissionError(
            "Unauthorized shared library access. Provide a valid auth token."
        )


def _resolve_asset_version(
    *, asset_path: Path, requested_version: str | None
) -> tuple[str, Path]:
    candidates: dict[str, Path] = {}
    for file_path in asset_path.iterdir():
        if not file_path.is_file() or file_path.suffix.lower() not in ORCHESTRATION_SUFFIXES:
            continue
        version = file_path.stem
        existing = candidates.get(version)
        if existing is not None and file_path.suffix.lower() == ".json":
            # A .json file sharing a stem with an existing candidate is a
            # metadata sidecar, not an orchestration.  Skip it.
            continue
        candidates[version] = file_path

    if not candidates:
        raise FileNotFoundError(
            f"No orchestration versions found for shared asset at {asset_path}"
        )

    if requested_version:
        resolved = candidates.get(requested_version)
        if resolved is None:
            versions = ", ".join(sorted(candidates.keys()))
            raise FileNotFoundError(
                f"Version {requested_version!r} not found for shared asset "
                f"{asset_path.name!r}. Available versions: {versions}"
            )
        return (requested_version, resolved)

    latest_version = max(candidates.keys(), key=_version_sort_key)
    return (latest_version, candidates[latest_version])


def _version_sort_key(version: str) -> tuple[Any, ...]:
    parts = version.replace("-", ".").split(".")
    key: list[Any] = [int(part) if part.isdigit() else part for part in parts]
    return tuple(key)


def _load_metadata_sidecar(orchestration_file: Path) -> dict[str, Any]:
    if orchestration_file.suffix.lower() == ".json":
        return {}
    metadata_file = orchestration_file.with_suffix(".json")
    if not metadata_file.exists():
        return {}

    loaded = json.loads(metadata_file.read_text(encoding="utf-8"))
    if not isinstance(loaded, dict):
        raise ValueError(f"Library metadata file must contain object: {metadata_file}")
    return loaded


def _merge_dict(base: dict[str, Any], overlay: dict[str, Any]) -> dict[str, Any]:
    result: dict[str, Any] = dict(base)
    for key, value in overlay.items():
        current = result.get(key)
        if isinstance(current, dict) and isinstance(value, dict):
            result[key] = _merge_dict(current, value)
        else:
            result[key] = value
    return result


def _dedupe_plugins(plugins: list[str]) -> list[str]:
    seen: set[str] = set()
    ordered: list[str] = []
    for plugin in plugins:
        if plugin not in seen:
            seen.add(plugin)
            ordered.append(plugin)
    return ordered
