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


class LifecyclePlugin:
    """Records the per-effect lifecycle as an ordered ``kind:path`` log.

    Both halves of the pair land in one list so a test can assert ordering
    and balance, not just membership. ``RecordingPlugin`` above deliberately
    stays complete-only — it is the back-compat fixture.
    """

    name = "lifecycle-plugin"

    def _log(self, state: dict[str, Any]) -> list[str]:
        runtime = state.setdefault("runtime", {})
        marker = runtime.setdefault("lifecycle_marker", {})
        events: list[str] = marker.setdefault("events", [])
        return events

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        del context
        self._log(state)

    def on_run_success(self, *, state: dict[str, Any], context: Any) -> None:
        del state, context

    def on_run_failure(self, *, state: dict[str, Any], context: Any, error: str) -> None:
        del state, context, error

    def on_effect_start(
        self,
        *,
        state: dict[str, Any],
        context: Any,
        effect_path: str,
        effect_node: dict[str, Any],
    ) -> None:
        del context
        self._log(state).append(f"start:{effect_path}")
        meta = effect_node.get("meta")
        if isinstance(meta, dict) and "complexity" in meta:
            runtime = state.setdefault("runtime", {})
            scores = runtime.setdefault("lifecycle_marker", {}).setdefault("scores", {})
            scores[effect_path] = meta["complexity"]

    def on_effect_complete(
        self,
        *,
        state: dict[str, Any],
        context: Any,
        effect_path: str,
        effect_result: dict[str, Any],
    ) -> None:
        del context, effect_result
        self._log(state).append(f"complete:{effect_path}")


class StartOnlyPlugin:
    """Implements the start half only — the inverse back-compat case."""

    name = "start-only-plugin"

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        del state, context

    def on_run_success(self, *, state: dict[str, Any], context: Any) -> None:
        del state, context

    def on_run_failure(self, *, state: dict[str, Any], context: Any, error: str) -> None:
        del state, context, error

    def on_effect_start(
        self,
        *,
        state: dict[str, Any],
        context: Any,
        effect_path: str,
        effect_node: dict[str, Any],
    ) -> None:
        del context, effect_node
        runtime = state.setdefault("runtime", {})
        marker = runtime.setdefault("start_only_marker", {})
        marker.setdefault("effect_paths", []).append(effect_path)


class InvalidPlugin:
    name = "invalid-plugin"


def make_recording_plugin() -> RecordingPlugin:
    return RecordingPlugin()


def make_failing_plugin() -> FailingPlugin:
    return FailingPlugin()


def make_invalid_plugin() -> InvalidPlugin:
    return InvalidPlugin()


def make_lifecycle_plugin() -> LifecyclePlugin:
    return LifecyclePlugin()


def make_start_only_plugin() -> StartOnlyPlugin:
    return StartOnlyPlugin()
