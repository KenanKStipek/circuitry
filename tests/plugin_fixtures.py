from __future__ import annotations

from typing import Any


class RecordingPlugin:
    name = "recording-plugin"

    def __init__(self) -> None:
        self.effect_completes: list[tuple[str, dict[str, Any]]] = []

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        runtime = state.setdefault("runtime", {})
        runtime["plugin_marker"] = {
            "started": True,
            "run_id": context.run_id,
            "orchestration_path": str(context.orchestration_path),
        }

    def on_run_success(self, *, state: dict[str, Any], context: Any) -> None:
        runtime = state.setdefault("runtime", {})
        marker = runtime.setdefault("plugin_marker", {})
        marker["succeeded"] = True
        marker["run_id_on_success"] = context.run_id

    def on_run_failure(self, *, state: dict[str, Any], context: Any, error: str) -> None:
        runtime = state.setdefault("runtime", {})
        marker = runtime.setdefault("plugin_marker", {})
        marker["failed"] = True
        marker["error"] = error
        marker["run_id_on_failure"] = context.run_id

    def on_effect_complete(
        self,
        *,
        state: dict[str, Any],
        context: Any,
        effect_path: str,
        effect_result: dict[str, Any],
    ) -> None:
        del context
        self.effect_completes.append((effect_path, dict(effect_result)))
        runtime = state.setdefault("runtime", {})
        marker = runtime.setdefault("plugin_marker", {})
        record = marker.setdefault("effect_paths", [])
        record.append(effect_path)


class FailingPlugin:
    name = "failing-plugin"

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        del state, context
        raise RuntimeError("plugin start exploded")

    def on_run_success(self, *, state: dict[str, Any], context: Any) -> None:
        del state, context
        raise RuntimeError("plugin success exploded")

    def on_run_failure(self, *, state: dict[str, Any], context: Any, error: str) -> None:
        del state, context, error
        raise RuntimeError("plugin failure exploded")

    def on_effect_complete(
        self,
        *,
        state: dict[str, Any],
        context: Any,
        effect_path: str,
        effect_result: dict[str, Any],
    ) -> None:
        del state, context, effect_path, effect_result
        raise RuntimeError("plugin effect_complete exploded")


class InvalidPlugin:
    name = "invalid-plugin"


def make_recording_plugin() -> RecordingPlugin:
    return RecordingPlugin()


def make_failing_plugin() -> FailingPlugin:
    return FailingPlugin()


def make_invalid_plugin() -> InvalidPlugin:
    return InvalidPlugin()
