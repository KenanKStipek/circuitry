from __future__ import annotations

import json
import logging
import threading
import time
from collections.abc import Callable
from copy import deepcopy
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from uuid import uuid4

from ..adapters import Adapter, build_adapter
from ..adapters.factory import ADAPTER_REGISTRY, configured_timeout_seconds
from ..allowlist_gate import AllowlistError, install_allowlists, require_adapter
from ..capability_gate import install_capability_ceiling
from ..core.compiler import (
    apply_effect_overrides,
    compile_orchestration,
    unknown_concurrency_group_errors,
)
from ..core.concurrency import RUNTIME_CONFIG_KEY as _CONCURRENCY_LIMITER_KEY
from ..core.concurrency import RunConcurrencyLimiter
from ..core.document_check import structural_errors, unknown_key_warnings
from ..core.dynamic import DynamicRuntime
from ..core.interface_inputs import check_interface_inputs
from ..core.resume import document_sha256
from ..core.runtime_plugins import (
    PLUGIN_CONTRACT_VERSION,
    PluginContext,
    RuntimePlugin,
    invoke_plugins,
    load_plugins,
)
from ..core.saved_state import compact_last_aliases, link_last_refs
from ..core.state_ns import migrate_legacy_state
from ..core.store import Store, build_persistence_backend
from ..core.store.persistence import PersistenceBackend
from ..plugins.factory import build_plugin
from ..preflight import CheckResult, call_check
from .allowlist import (
    check_allowlist,
    collect_adapter_usages,
    hard_effect_names,
    is_hard_adapter_dependency,
    profile_provider_denials,
    skippable_effect_names,
    walk_orchestration_refs,
)
from .config import CircuitryConfig, trust_store_path
from .document_consent import ConsentPrompt, enforce_consent
from .effective_settings import (
    EffectiveSettings,
    _merge_runtime,
    _split_orchestration_runtime,
    orchestration_host_setting_warnings,
    resolve_effective_settings,
)
from .live_state import LiveStateMirror
from .orchestration_loader import ORCHESTRATION_SUFFIXES, load_orchestration_file
from .profiles import (
    ProfileSettings,
    load_profile,
    profile_from_record,
    validate_profile_routing_pins,
)
from .redaction import redact

logger = logging.getLogger(__name__)

try:
    import jsonschema as _jsonschema
except ImportError:
    _jsonschema = None  # type: ignore[assignment]

_SCHEMA_PATH = Path(__file__).parent.parent / "schema" / "orchestration.schema.json"


def _load_schema() -> dict[str, Any] | None:
    if _jsonschema is None:
        return None
    try:
        return json.loads(_SCHEMA_PATH.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None


def load_schema() -> dict[str, Any] | None:
    """The orchestration JSON schema, or ``None`` when it cannot be used.

    Public entry point for callers that validate against the same schema
    ``validate`` does (the TUI's Validate view) without reaching for a private.
    """
    return _load_schema()


@dataclass(frozen=True)
class RunRequest:
    orchestration_path: Path
    state_path: Path | None
    out_path: Path | None
    dry_run: bool
    validate_only: bool
    initial_state: dict[str, Any] | None = None
    shared_library_metadata: dict[str, Any] | None = None
    verbose: bool = False
    # The caller already knows whether this run is interactive (a TTY,
    # neither --quiet nor --json) — run() itself never checks stdout, so
    # every non-CLI caller (SDK, MCP, the scheduler) defaults to no
    # progress line rather than one that assumes a terminal exists. See
    # cli.app's run commands, the only built-in caller that passes True.
    show_loop_progress: bool = False
    config: CircuitryConfig | None = None
    live_state_path: Path | None = None
    adapter: Adapter | None = None
    state_observer: Callable[[dict[str, Any]], None] | None = None
    # Per-effect completion notifications: ``(effect_path, effect_node)``,
    # the same payload runtime plugins receive from ``on_effect_complete``.
    effect_observer: Callable[[str, dict[str, Any]], None] | None = None
    # The counterpart, fired before each effect dispatches: same
    # ``(effect_path, effect_node)`` payload runtime plugins receive from
    # ``on_effect_start``.
    effect_start_observer: Callable[[str, dict[str, Any]], None] | None = None
    # Fired once, before any of its branches start, by a ``flow: tree`` loop
    # or a parallel ``dynamic`` — ``(effect_path, branch_count)``. MCP's
    # RunManager uses this to wait for a real per-run "settle" signal instead
    # of a fixed debounce window a scheduling delay can race (#237).
    concurrent_dispatch_observer: Callable[[str, int], None] | None = None
    # Fired once per branch of a dispatch announced via
    # ``concurrent_dispatch_observer``, as soon as that branch's own
    # execution genuinely finishes — ``(effect_path,)``. Lets MCP's
    # RunManager lower how many settle points it's still owed by a branch
    # that will never produce one (#237).
    branch_settled_observer: Callable[[str], None] | None = None
    skip_preflight: bool = False
    # Caller-level overrides, ranked above the orchestration's own
    # ``adapter``/``model`` (the ``cli`` tier of resolve_effective_settings).
    # ``adapter_override`` is ignored when ``adapter`` supplies an instance —
    # that already pins the transport.
    adapter_override: str | None = None
    model_override: str | None = None
    # Per-run overrides for the three `runtime.complexity` switches — the
    # `cli` tier of `resolve_effective_settings`, same rank as
    # `adapter_override`/`model_override`. `None` means "leave the resolved
    # config alone"; `True`/`False` force the switch on/off for this run.
    # `routing_override is False` additionally strips any profile per-effect
    # `routing` pin before the orchestration compiles — a run-level
    # `--no-routing` beats a finer-grained profile pin, see `run()` below.
    scoring_override: bool | None = None
    routing_override: bool | None = None
    decompose_override: bool | None = None
    profile_name: str | None = None
    # A recorded `runtime.effective_settings.profile` mapping ({name, content})
    # to reconstruct and apply instead of discovering a profile file by name.
    # Mutually exclusive with `profile_name`; raises if both are set.
    profile_record: dict[str, Any] | None = None
    # Directory to persist every triggered decomposition's generated plan to,
    # one file per effect — see `circuitry.cli.decompose_out`. `None` (the
    # default) writes nothing to disk; the plan still lands in
    # `meta.decomposition` either way.
    decompose_out: Path | None = None
    # True only when whoever owns this machine named the document by path
    # (`cof run ./my.yml`, the SDK's `run_orchestration`, a scheduler job): the
    # document's whole `runtime:` block and `plugins:` list then apply, with a
    # notice naming the host settings among them. Library, fetched, generated
    # and network/tool-chosen documents keep the default and stay limited to
    # ORCHESTRATION_RUNTIME_KEYS — see `resolve_effective_settings`.
    trust_document: bool = False
    # Capability consent (#275). Pre-approved capabilities for this run only
    # (never persisted to the consent store) — the scripted/CI escape hatch
    # named in a `DocumentConsentError`'s own message, e.g.
    # `frozenset({"shell", "network"})` for `--allow-capabilities shell,network`.
    allow_capabilities: frozenset[str] | None = None
    # An interactive callback — `(label, capabilities) -> bool` — the CLI
    # supplies to ask the user before a document that needs fresh consent
    # runs; `None` (every non-CLI caller: SDK, MCP, REST, TUI, scheduler)
    # means never prompt, so a missing consent always refuses rather than
    # blocking on input nobody can give (#275 rule 5).
    capability_prompt: ConsentPrompt | None = None
    # True when `orchestration_path` was resolved from a refreshable (today,
    # `github`) library source run by bare name rather than through
    # `run_shared_orchestration`/`cof run-library` — the other half of
    # `gate_whole_document` below, so a remote library source gets the same
    # whole-document capability gate whichever surface reaches it (#275).
    remote_library_source: bool = False
    # `cof run --resume`: when true, this run skips any effect whose node in
    # `initial_state` already finished without error (see `core.resume`),
    # and a named loop in chain flow resumes at its first unfinished pass
    # instead of rerunning every pass. Resolving *which* saved state to pass
    # as `initial_state` (an explicit --state file, the --last stash, or a
    # persistence backend's run-id lookup) and the document-hash/input
    # safety checks are the caller's job (see `cli.app.run_cmd`) — this flag
    # only turns on the engine-level skip behavior once that state is here.
    resume: bool = False
    # The file this resume's state came from (an explicit --state file, or
    # the --last stash's --out), named by `cli.app.run_cmd` — used as the
    # `out` a run writes back to when neither --out nor a profile named one,
    # so a resumed run saves its own progress by default (#270 F10). `None`
    # for a bare run-id resume (no single file the state came from) or any
    # non-resumed run.
    resume_default_out: Path | None = None


@dataclass(frozen=True)
class RunResult:
    ok: bool
    state: dict[str, Any]
    warnings: list[str]
    error: str | None = None
    # Resolved --out path (cli > profile > default) callers should write the
    # final state to, instead of re-deriving precedence themselves.
    out_path: Path | None = None
    # Set when `error` is a Ctrl-C/SIGINT rather than an ordinary failure —
    # same `ok=False` shape (state/out_path are still written the usual way,
    # so the run is resumable), but the CLI exits 130 for it instead of 1.
    interrupted: bool = False


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _effect_meta_nodes(state: dict[str, Any]) -> list[dict[str, Any]]:
    """Every effect's ``meta`` dict in *state*, each counted once.

    Alias-aware like ``tui.execution.sum_tokens``: a named loop exposes its
    final completed pass at both ``iter_<N>`` and ``last`` — the same dict,
    reachable twice — so dedupe by ``id()`` rather than walking every path
    to it.
    """
    metas: list[dict[str, Any]] = []
    seen: set[int] = set()
    stack: list[Any] = [state]
    while stack:
        current = stack.pop()
        if isinstance(current, dict):
            if id(current) in seen:
                continue
            seen.add(id(current))
            meta = current.get("meta")
            if isinstance(meta, dict):
                metas.append(meta)
            for key, value in current.items():
                if key != "meta":
                    stack.append(value)
        elif isinstance(current, list):
            stack.extend(current)
    return metas


def _int_field(meta: dict[str, Any], *keys: str) -> int:
    """The first of *keys* present on *meta* as a real (non-bool) int, else 0.

    Tries ``<field>_total`` before the plain field so a prompt's run total
    (#313: every attempt — failed, retried, fallen-back-from) counts instead
    of just the attempt that finally answered.
    """
    for key in keys:
        value = meta.get(key)
        if isinstance(value, int) and not isinstance(value, bool):
            return value
    return 0


def _run_totals(state: dict[str, Any], *, wall_time_s: float) -> dict[str, Any]:
    """``state.runtime.last_run.totals`` recomputed from a *finished* state
    tree — wall time, effects run, tokens both ways over every attempt, and
    cost where some effect reported one (no adapter does yet, so this is
    ``None`` until one does).

    A pure function of the final state, so it's only correct for effects
    that actually landed there. ``run()`` itself does not use this for its
    own ``last_run.totals`` (see ``_TotalsAccumulator``): a ``use`` child in
    declared-outputs mode (no ``record_children``) never leaves its effects
    in the final state at all, and an unnamed loop overwrites the same node
    every pass, so walking the finished tree undercounts both. Kept as a
    standalone utility for recomputing totals against a state snapshot
    that wasn't accumulated live (e.g. a loaded ``--state`` file).
    """
    effects_run = 0
    tokens_sent = tokens_received = 0
    cost_usd: float | None = None
    for meta in _effect_meta_nodes(state):
        if meta.get("completed_at"):
            effects_run += 1
        tokens_sent += _int_field(meta, "tokens_sent_total", "tokens_sent")
        tokens_received += _int_field(meta, "tokens_received_total", "tokens_received")
        cost = meta.get("cost_usd")
        if isinstance(cost, (int, float)) and not isinstance(cost, bool):
            cost_usd = (cost_usd or 0.0) + cost
    return {
        "wall_time_s": wall_time_s,
        "effects_run": effects_run,
        "tokens_sent": tokens_sent,
        "tokens_received": tokens_received,
        "cost_usd": cost_usd,
    }


class _TotalsAccumulator:
    """Builds ``state.runtime.last_run.totals`` live, from the same
    ``effect_complete`` stream plugins and ``--live-state`` already observe
    (#331 finding 4), instead of walking the finished state tree:

    - A ``use`` child's effects reach this observer namespaced under the
      parent path (see ``use.py``'s ``_namespaced_effect_cb``) even in
      declared-outputs mode, where they never land in the final state at
      all — walking the tree after the fact cannot see them.
    - An unnamed loop's pass overwrites the same state node every
      iteration, so only the last pass would ever be visible to a
      post-hoc walk; each pass's own ``effect_complete`` firing is still
      distinct here.
    """

    def __init__(self) -> None:
        self.effects_run = 0
        self.tokens_sent = 0
        self.tokens_received = 0
        self.cost_usd: float | None = None

    def observe(self, effect_path: str, effect_result: dict[str, Any]) -> None:
        meta = effect_result.get("meta")
        if not isinstance(meta, dict) or not meta.get("completed_at"):
            return
        self.effects_run += 1
        self.tokens_sent += _int_field(meta, "tokens_sent_total", "tokens_sent")
        self.tokens_received += _int_field(
            meta, "tokens_received_total", "tokens_received"
        )
        cost = meta.get("cost_usd")
        if isinstance(cost, (int, float)) and not isinstance(cost, bool):
            self.cost_usd = (self.cost_usd or 0.0) + cost

    def totals(self, *, wall_time_s: float) -> dict[str, Any]:
        return {
            "wall_time_s": wall_time_s,
            "effects_run": self.effects_run,
            "tokens_sent": self.tokens_sent,
            "tokens_received": self.tokens_received,
            "cost_usd": self.cost_usd,
        }


def _load_state(
    path: Path | None, initial_state: dict[str, Any] | None = None
) -> dict[str, Any]:
    # The single choke point where caller state enters a run: whatever the
    # source (--state file, -e inline values, REST/TUI/MCP initial_state),
    # legacy bare root keys are lifted under the `input` namespace here, and
    # a previous run's saved `last` references are relinked to their passes.
    if initial_state is not None:
        # Isolate runtime mutations from caller-owned dictionaries.
        return link_last_refs(migrate_legacy_state(deepcopy(initial_state)))
    if path is None:
        return migrate_legacy_state({})
    if not path.exists():
        raise FileNotFoundError(f"state file not found: {path}")
    return link_last_refs(
        migrate_legacy_state(json.loads(path.read_text(encoding="utf-8")))
    )


def run(req: RunRequest) -> RunResult:
    state: dict[str, Any] = {}
    warnings: list[str] = []
    plugins: list[RuntimePlugin] = []
    run_id: str | None = None
    runtime_config: dict[str, Any] = {}
    # Bound inside the try once the orchestration's runtime.persistence is
    # resolved; kept outside it (like run_id) so the except branch below can
    # still persist a failure snapshot even though it shares this function's
    # one try/except rather than its own.
    persistence: PersistenceBackend | None = None
    # Resolved --out path (cli > profile > default); refined once the
    # profile, if any, is loaded below. Kept outside the try's happy path so
    # a failure before that point still reports the caller's own --out.
    resolved_out: Path | None = req.out_path
    # Every store of this run shares one lock; the --live-state mirror
    # serialises under it and writes the file after releasing it.
    store_lock = threading.RLock()
    live_mirror: LiveStateMirror | None = None
    # Wall time for `state.runtime.last_run.totals` — monotonic, not the
    # `started_at`/`completed_at` ISO timestamps (which a system clock
    # adjustment mid-run could skew).
    _run_t0 = time.monotonic()
    # Fed every effect_complete event below, success or failure — defined
    # before the try so a failure before that wiring still reports zeroed,
    # accurate totals rather than raising in the except block.
    totals_accumulator = _TotalsAccumulator()

    try:
        state = _load_state(req.state_path, req.initial_state)
        cfg = req.config or CircuitryConfig()
        # A skipped (untrusted) project config, first, so a failing run
        # still says which settings it ran without.
        warnings.extend(cfg.resolution_warnings())
        orch = load_orchestration_file(req.orchestration_path)

        allowlist_errors = check_allowlist(
            orch=orch, config=cfg, root_path=req.orchestration_path
        )
        if allowlist_errors:
            raise AllowlistError(
                "Allowlist enforcement failed: " + "; ".join(allowlist_errors)
            )

        # Capability consent (#275): gated on the whole document for a
        # `cof fetch`/`cof run-library` asset (`shared_library_metadata` is
        # the signal both the CLI command and `run_shared_orchestration` set)
        # and for a remote library source run by bare name
        # (`remote_library_source`, set by `cof run` when the resolved path
        # came from a `github`-type source); a `use: ref:` child is
        # independently in scope regardless, including one reached from a
        # path-trusted document. Raises/prompts before anything compiles or
        # dispatches, same fail-fast spirit as the allowlist check above.
        capability_ceiling = enforce_consent(
            orch=orch,
            orchestration_path=req.orchestration_path,
            gate_whole_document=(
                req.shared_library_metadata is not None or req.remote_library_source
            ),
            runtime=cfg.runtime,
            store_path=trust_store_path(),
            allow_capabilities=req.allow_capabilities,
            prompt=req.capability_prompt,
        )

        if req.profile_name and req.profile_record is not None:
            raise ValueError(
                "RunRequest.profile_name and RunRequest.profile_record are "
                "mutually exclusive; pass at most one."
            )

        profile: ProfileSettings | None = None
        if req.profile_record is not None:
            profile = profile_from_record(req.profile_record, orch=orch)
        elif req.profile_name:
            profile = load_profile(
                name=req.profile_name,
                orchestration_path=req.orchestration_path,
                orch=orch,
            )
        if profile is not None and profile.inputs:
            # Profile inputs are a lower-priority base layer under
            # whatever the caller already resolved from --state/-e (CLI
            # values win — see cli.app.run_cmd). Both layers live in the
            # `input` namespace.
            input_ns = state.get("input")
            merged = dict(profile.inputs)
            if isinstance(input_ns, dict):
                merged.update(input_ns)
            state["input"] = merged

        effective = resolve_effective_settings(
            cfg=cfg,
            orch=orch,
            cli_model=req.model_override,
            cli_adapter=req.adapter_override,
            cli_out=req.out_path,
            cli_scoring=req.scoring_override,
            cli_routing=req.routing_override,
            cli_decompose=req.decompose_override,
            profile=profile,
            trust_document=req.trust_document,
            document_name=req.orchestration_path.name,
            resume_default_out=req.resume_default_out if req.resume else None,
        )
        warnings.extend(effective.warnings)
        resolved_out = effective.out

        # Built once for the whole run, from the same merged runtime config
        # every effect will see (document `runtime:` key by key over config,
        # per #316) — shared by every tool/prompt leaf dispatched anywhere in
        # the tree, including a `use` child's, via `runtime_config` (#274).
        # Its group names are checked against the compiled document below,
        # once `root_def` exists.
        concurrency_limiter = RunConcurrencyLimiter.from_runtime_config(
            effective.runtime or {}
        )

        if profile is not None and profile.effects:
            # A pinned band name can only be checked once the run's actual
            # routing table is known — the profile alone can't tell, since
            # bands live in the orchestration/config, not the profile. Same
            # fail-fast spirit as the allowlist/schema checks above: before
            # anything compiles or dispatches, not mid-run on the one effect
            # that hits it.
            validate_profile_routing_pins(
                profile.effects,
                routing=effective.complexity.routing,
                profile_name=profile.name,
            )
            profile_denials = profile_provider_denials(
                profile.effects, enabled_adapters=cfg.enabled_adapters
            )
            if profile_denials:
                raise AllowlistError(
                    "Allowlist enforcement failed: " + "; ".join(profile_denials)
                )
        # One shared dict for the whole run: `use` effects append their library
        # pins to it as they resolve, at any nesting depth.
        runtime_config = effective.runtime if effective.runtime is not None else {}
        runtime_config[_CONCURRENCY_LIMITER_KEY] = concurrency_limiter
        # Root directory a `path:`-resolving `use` effect falls back to when
        # its target isn't absolute or cwd-relative — see `UseRuntime`, which
        # rewrites this per-child as composition descends into subdirectories.
        runtime_config["_orchestration_dir"] = str(
            req.orchestration_path.resolve().parent
        )
        # What every nested runtime builds — `use` children, generated plans,
        # profile provider overrides — is checked against these as it is
        # built; the document check above only sees the document's text.
        install_allowlists(
            runtime_config,
            enabled_adapters=cfg.enabled_adapters,
            enabled_tools=cfg.enabled_tools,
        )
        # The ceiling a generated plan (reflector/decompose) inside this
        # document may not exceed (#275 rule 4) — `None` (a path-run or
        # plain library-name document) leaves it unrestricted.
        install_capability_ceiling(runtime_config, capability_ceiling)
        # So a `use: ref:` child only known once a Mustache tag renders
        # (unreachable to the static walk above) still honors this run's
        # own `--allow-capabilities` when `UseRuntime` re-checks it. Always
        # overwritten, never left alone, for the same reason as the ceiling
        # above: a stray `_capability_allow` a trusted document's own
        # `runtime:` block happened to carry must never survive into the
        # shared runtime_config and widen what a `use: ref:` child is
        # allowed without the user's own `--allow-capabilities` (#275).
        runtime_config["_capability_allow"] = (
            sorted(req.allow_capabilities) if req.allow_capabilities else []
        )
        persistence = build_persistence_backend(effective.runtime)
        plugins, plugin_events = _initialize_plugins(
            effective.plugins, allowed=cfg.enabled_plugins
        )

        loaded_from_persistence = False
        if (
            persistence is not None
            and req.initial_state is None
            and req.state_path is None
        ):
            try:
                persisted = persistence.load_latest_state(
                    orchestration_path=str(req.orchestration_path)
                )
                if isinstance(persisted, dict):
                    # Lift-on-hydrate: pre-namespace snapshots get their
                    # bare root keys moved under `input` (logged once).
                    hydrated = link_last_refs(
                        migrate_legacy_state(deepcopy(persisted))
                    )
                    if profile is not None and profile.inputs:
                        # Profile inputs stay the lowest layer: they fill
                        # keys the persisted snapshot doesn't carry rather
                        # than overwriting resumed values.
                        input_ns = hydrated.setdefault("input", {})
                        if isinstance(input_ns, dict):
                            for key, value in profile.inputs.items():
                                input_ns.setdefault(key, value)
                    state = hydrated
                    loaded_from_persistence = True
            except Exception as e:
                state.setdefault("runtime", {})
                state["runtime"]["persistence"] = {
                    "enabled": True,
                    "status": "load_failed",
                    "error": str(e),
                }
                raise RuntimeError(f"Failed to load persisted state: {e}") from e

        # Top-level `interface.inputs`: the same required/type contract
        # `use:` children get, enforced here once so every surface that
        # reaches `run()` (cof run, the SDK, REST, MCP, the scheduler)
        # behaves the same way. Fills in declared `default:`s and coerces
        # declared-typed `-e`/`--state` string values before anything runs.
        input_ns = state.get("input")
        if not isinstance(input_ns, dict):
            input_ns = {}
            state["input"] = input_ns
        check_interface_inputs(
            orch.get("interface") if isinstance(orch, dict) else None,
            input_ns,
            label="",
        )

        state.setdefault("runtime", {})
        run_id = str(uuid4())
        # Assigned here, immediately, rather than right before execution:
        # `on_run_start` plugins and preflight both run before that point, and
        # a run that fails before then (preflight, adapter resolution) still
        # writes `--out` — every one of those should see this run's own fresh
        # id/timestamp next to `runtime.last_run.run_id`, not the previous
        # run's (`--state`/persistence carryover).
        state["_run_id"] = run_id
        state["_timestamp"] = datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S")
        try:
            document_hash = document_sha256(req.orchestration_path)
        except OSError:
            # The document check above already loaded this same file; an
            # unreadable path would have failed there first. Still, a race
            # (the file vanished between then and now) shouldn't block a run
            # over a hash that only matters for a *future* --resume.
            document_hash = None
        state["runtime"]["last_run"] = {
            "run_id": run_id,
            "orchestration_path": str(req.orchestration_path),
            "document_hash": document_hash,
            "dry_run": req.dry_run,
            "validate_only": req.validate_only,
            "verbose": req.verbose,
            "started_at": _now_iso(),
            "completed_at": None,
        }

        # Redact credential-bearing fields before embedding in state, since
        # state is serialized to --out, --json, --live-state, and last-run.json.
        # Live adapter calls keep using the un-redacted `effective.runtime`.
        # The concurrency limiter rides the same dict purely for in-process
        # plumbing (see core.concurrency) and, unlike every other private key
        # already in here, isn't JSON-serializable at all — dropped before
        # this snapshot, never before a live call reads `effective.runtime`.
        _snapshot_runtime = {
            k: v for k, v in effective.runtime.items() if k != _CONCURRENCY_LIMITER_KEY
        }
        state["runtime"]["effective_settings"] = {
            "model": effective.model,
            "adapter": effective.adapter,
            "out": str(effective.out) if effective.out else None,
            "plugins": effective.plugins,
            "runtime": redact(_snapshot_runtime),
            "sources": effective.sources,
        }
        if profile is not None:
            state["runtime"]["effective_settings"]["profile"] = {
                "name": profile.name,
                "content": redact(profile.raw),
            }
        if req.shared_library_metadata is not None:
            state["runtime"]["shared_library"] = req.shared_library_metadata
        state["runtime"]["plugins"] = {
            "contract_version": PLUGIN_CONTRACT_VERSION,
            "configured": list(effective.plugins),
            "loaded": [getattr(p, "name", type(p).__name__) for p in plugins],
            "events": plugin_events,
        }
        if persistence is not None:
            state["runtime"]["persistence"] = {
                "enabled": True,
                "status": "ready",
                "error": None,
                "loaded_from_persistence": loaded_from_persistence,
                "persisted": False,
                "run_id": run_id,
                **persistence.describe(),
            }

        if not req.validate_only:
            start_events = invoke_plugins(
                plugins=plugins,
                hook_name="on_run_start",
                state=state,
                context=PluginContext(
                    run_id=run_id,
                    orchestration_path=req.orchestration_path,
                    dry_run=req.dry_run,
                    validate_only=req.validate_only,
                    runtime_config=effective.runtime,
                    environment=cfg.environment,
                ),
            )
            state["runtime"]["plugins"]["events"].extend(start_events)

        # The structural gate `cof check` applies, then compile YAML -> core
        # definitions, both before adapter/model initialization and before
        # any effect: a document `validate` rejects never reaches its first
        # effect, and fails here like a compile error (run-failure hooks see it).
        document_errors = structural_errors(orch)
        if document_errors:
            raise ValueError(
                "Orchestration validation failed:\n"
                + "\n".join(f"  - {error}" for error in document_errors)
            )
        root_def = compile_orchestration(orch=orch, root_name="prime")

        group_errors = unknown_concurrency_group_errors(
            root_def, concurrency_limiter.group_names
        )
        if group_errors:
            raise ValueError(
                "Orchestration validation failed:\n"
                + "\n".join(f"  - {error}" for error in group_errors)
            )

        # A `use:` cycle is otherwise only caught mid-execution (core/use.py);
        # `validate()`/`cof check` already reject it here, so `validate_only`
        # must too rather than returning ok for a document `cof check` rejects.
        from ..core.cycle_check import detect_cycles

        cycle = detect_cycles(
            orch, root_path=req.orchestration_path, runtime=effective.runtime
        )
        if cycle is not None:
            raise ValueError(f"Cycle: {' → '.join(cycle)}")

        if profile is not None and profile.effects:
            effect_overrides = {
                path: {
                    k: v
                    for k, v in override.items()
                    if k in ("model", "provider", "enabled", "routing")
                }
                for path, override in profile.effects.items()
            }
            if req.routing_override is False:
                # A run-level `--no-routing` outranks a profile's per-effect
                # `routing` pin/opt-out: routing is off for this run, full
                # stop, so a pin naming a band is dropped here rather than
                # reaching the compiled definition and quietly reactivating
                # routing for one effect. (An opt-out — `routing: false` — is
                # already equivalent to "off", so dropping it changes nothing.)
                for override in effect_overrides.values():
                    override.pop("routing", None)
            effect_overrides = {
                path: override for path, override in effect_overrides.items() if override
            }
            if effect_overrides:
                root_def, _ = apply_effect_overrides(root_def, effect_overrides)

        adapter: Adapter
        timeout_seconds = 120
        # Resolve the adapter/model *names* first; the actual adapter object
        # (the factory call that raises on an unknown name) isn't built until
        # after preflight below, so an unknown default adapter fails with
        # preflight's structured message, not the factory's raw ValueError
        # (#235).
        #
        # Before `validate_only` can return, only a *name* is needed (for the
        # image-asset warning below) — `cof check`/`validate_orchestration`
        # never require a resolved adapter to say ok, so this stays as lenient
        # as `effective.adapter` itself; the strict "no adapter resolved"
        # error (`_require_resolved_settings`) is deferred past the
        # `validate_only` return, right before the adapter is actually built.
        needs_real_adapter = _has_prompt_effects(root_def)
        if needs_real_adapter:
            if req.adapter is not None:
                # Caller supplied a fully-constructed adapter (e.g. circuitry-mcp
                # injecting a HostClaudeAdapter wired to per-prompt queues).
                # Skip factory dispatch but still resolve a model — the adapter
                # may pin its own, in which case effective.model can be empty.
                resolved_adapter = req.adapter.name
                resolved_model = effective.model or ""
            else:
                resolved_adapter = effective.adapter or ""
                resolved_model = effective.model or ""
        else:
            resolved_adapter = "_noop"
            resolved_model = effective.model or ""

        # Preflight (Story 1). Resolved but before the run-default adapter is
        # actually built, before any LLM / tool effect runs. ``--skip-preflight``
        # opts out for advanced use; ``--dry-run`` also skips it since the
        # whole point of dry-run is to avoid touching the world. Only runs
        # when a config was supplied (programmatic callers without a config
        # keep default-open behavior). When the caller injected an adapter via
        # RunRequest.adapter we trust them for *that* check only — the adapter
        # may not be buildable from config (host_claude) — but tool and
        # library-ref preflight still run; an injected adapter is not a
        # license to skip everything else (#265 part 4).
        if not req.skip_preflight and not req.dry_run and req.config is not None:
            preflight_results = preflight(
                req.orchestration_path, req.config, skip_adapter_check=req.adapter is not None
            )
            hard_results, soft_results = classify_preflight_results(
                req.orchestration_path, preflight_results
            )
            preflight_errors = format_preflight_errors(hard_results)
            if preflight_errors:
                raise RuntimeError(
                    "Preflight failed: "
                    + "; ".join(preflight_errors)
                    + ". Re-run with --skip-preflight to bypass."
                )
            warnings.extend(format_preflight_warnings(soft_results))
            warnings.extend(
                image_asset_warnings(
                    orch,
                    default_adapter=resolved_adapter,
                    runtime_cfg=effective.runtime,
                )
            )

        if req.validate_only:
            # The same checks `cof check` / api.validate_orchestration run by
            # default have all already passed above — structural check,
            # compile, preflight — plus the same lint warnings; stop here
            # instead of building the adapter or dispatching any effect
            # (#302).
            from ..core.lint import lint_orchestration

            warnings.extend(lint_orchestration(orch))
            warnings.extend(unknown_key_warnings(orch))
            state["runtime"]["last_run"]["completed_at"] = _now_iso()
            return RunResult(ok=True, state=state, warnings=warnings, out_path=resolved_out)

        if needs_real_adapter:
            if req.adapter is not None:
                adapter = req.adapter
            else:
                # The strict "no adapter resolved" check, deferred from
                # above so `validate_only` isn't rejected for a document
                # `cof check` accepts (neither requires one to say ok).
                resolved_adapter, resolved_model = _require_resolved_settings(
                    effective=effective, orchestration_path=req.orchestration_path
                )
                # `--adapter` and the config's `default_adapter` resolve
                # here, after the document check and preflight — gate them
                # at the build.
                require_adapter(resolved_adapter, cfg.enabled_adapters)
                adapter = build_adapter(
                    adapter_name=resolved_adapter, runtime=effective.runtime or {}
                )
            timeout_seconds = (
                configured_timeout_seconds(resolved_adapter, effective.runtime) or 120
            )
        else:
            adapter = _NoOpAdapter()

        # Execute using core runtime against Store
        callbacks: list[Callable[[dict[str, Any]], None]] = []
        if req.live_state_path is not None:
            live_mirror = LiveStateMirror(req.live_state_path, store_lock=store_lock)
            callbacks.append(live_mirror)
        if req.state_observer is not None:
            callbacks.append(req.state_observer)

        on_write: Callable[[dict[str, Any]], None] | None
        if not callbacks:
            on_write = None
        elif len(callbacks) == 1:
            on_write = callbacks[0]
        else:
            def on_write(s: dict[str, Any]) -> None:
                for cb in callbacks:
                    cb(s)

        if on_write is not None:
            # Write initial snapshot so watchers see the pending state
            on_write(state)

        # Story 2: per-effect lifecycle hooks. ``on_effect_start`` fires
        # before an effect dispatches, ``on_effect_complete`` once its
        # node["value"] is finalized; both are skipped silently for plugins
        # that don't implement them. ``req.effect_start_observer`` /
        # ``req.effect_observer`` ride the same hooks, which is how a live UI
        # learns an effect started or landed without diffing whole state
        # snapshots.
        start_observers: list[Callable[[str, dict[str, Any]], None]] = []
        effect_observers: list[Callable[[str, dict[str, Any]], None]] = [
            totals_accumulator.observe
        ]
        if plugins:
            _plugin_ctx = PluginContext(
                run_id=run_id,
                orchestration_path=req.orchestration_path,
                dry_run=req.dry_run,
                validate_only=req.validate_only,
                runtime_config=effective.runtime,
                environment=cfg.environment,
            )

            def _notify_plugins_start(
                effect_path: str, effect_node: dict[str, Any]
            ) -> None:
                invoke_plugins(
                    plugins=plugins,
                    hook_name="on_effect_start",
                    state=state,
                    context=_plugin_ctx,
                    effect_path=effect_path,
                    effect_node=effect_node,
                )

            def _notify_plugins(
                effect_path: str, effect_result: dict[str, Any]
            ) -> None:
                invoke_plugins(
                    plugins=plugins,
                    hook_name="on_effect_complete",
                    state=state,
                    context=_plugin_ctx,
                    effect_path=effect_path,
                    effect_result=effect_result,
                )

            start_observers.append(_notify_plugins_start)
            effect_observers.append(_notify_plugins)
        if req.effect_start_observer is not None:
            start_observers.append(req.effect_start_observer)
        if req.effect_observer is not None:
            effect_observers.append(req.effect_observer)
        if req.decompose_out is not None:
            from .decompose_out import make_decompose_out_observer

            effect_observers.append(
                make_decompose_out_observer(req.decompose_out, run_id)
            )

        store = Store(
            state,
            on_write=on_write,
            effect_complete=_compose_effect_observers(effect_observers),
            effect_start=_compose_effect_observers(start_observers),
            concurrent_dispatch=req.concurrent_dispatch_observer,
            branch_settled=req.branch_settled_observer,
            _lock=store_lock,
        )

        runtime = DynamicRuntime(
            root_def,
            adapter=adapter,
            model=resolved_model,
            # The one thing the runtime cannot work out for itself: whether
            # `resolved_model` was pinned with `--model`/a profile or merely
            # inherited from the orchestration or config. The complexity
            # router defers to the first and overrides the second.
            model_locked=effective.model_locked,
            runtime_config=runtime_config,
            dry_run=req.dry_run,
            timeout_seconds=timeout_seconds,
            verbose=req.verbose,
            resume=req.resume,
            progress_display=req.show_loop_progress,
        )
        runtime.execute(store=store)

        _record_library_pins(state, runtime_config)
        state["runtime"]["last_run"]["completed_at"] = _now_iso()
        state["runtime"]["last_run"]["totals"] = totals_accumulator.totals(
            wall_time_s=time.monotonic() - _run_t0
        )

        success_events = invoke_plugins(
            plugins=plugins,
            hook_name="on_run_success",
            state=state,
            context=PluginContext(
                run_id=run_id,
                orchestration_path=req.orchestration_path,
                dry_run=req.dry_run,
                validate_only=req.validate_only,
                runtime_config=effective.runtime,
                environment=cfg.environment,
            ),
        )
        state["runtime"]["plugins"]["events"].extend(success_events)

        if persistence is not None:
            try:
                persistence.save_run_snapshot(
                    orchestration_path=str(req.orchestration_path),
                    run_id=run_id,
                    ok=True,
                    error=None,
                    # Each loop's `last` as a sibling reference, not a full
                    # copy of its final pass (#236, mirroring `--out`); the
                    # load side above already relinks it back.
                    state=compact_last_aliases(state),
                )
                state["runtime"]["persistence"]["status"] = "persisted"
                state["runtime"]["persistence"]["persisted"] = True
            except Exception as e:
                state["runtime"]["persistence"]["status"] = "save_failed"
                state["runtime"]["persistence"]["error"] = str(e)
                raise RuntimeError(f"Failed to persist runtime state: {e}") from e

        return RunResult(ok=True, state=state, warnings=warnings, out_path=resolved_out)

    except (Exception, KeyboardInterrupt) as e:
        # Ctrl-C/SIGINT during a long effect dispatch reaches here exactly
        # like any other failure (KeyboardInterrupt isn't an Exception
        # subclass, hence the explicit tuple): the same cleanup records
        # what finished, writes the usual failure snapshot/--out, and the
        # error below just says why, so an interrupted run is resumable
        # the same way a crashed one is (#270 F6).
        interrupted = isinstance(e, KeyboardInterrupt)
        error_message = "Interrupted (Ctrl-C/SIGINT)" if interrupted else str(e)
        try:
            # Pins resolved before the failure still describe what this run
            # reached for — keep them for the post-mortem.
            _record_library_pins(state, runtime_config)
            if run_id is None:
                run_id = str(uuid4())
            # Written before on_run_failure runs (not after, as success's
            # on_run_success could previously assume) so a failure-hook
            # plugin sees the same totals a success-hook plugin would
            # (#331 finding 11).
            last_run_node = state.setdefault("runtime", {}).setdefault("last_run", {})
            last_run_node["completed_at"] = _now_iso()
            last_run_node["totals"] = totals_accumulator.totals(
                wall_time_s=time.monotonic() - _run_t0
            )
            plugins_meta = state.setdefault("runtime", {}).setdefault("plugins", {})
            if isinstance(plugins_meta, dict):
                configured = plugins_meta.get("configured")
                if not isinstance(configured, list):
                    plugins_meta["configured"] = []
                loaded = plugins_meta.get("loaded")
                if not isinstance(loaded, list):
                    plugins_meta["loaded"] = []
                events = plugins_meta.get("events")
                if not isinstance(events, list):
                    plugins_meta["events"] = []
                failure_events = invoke_plugins(
                    plugins=plugins,
                    hook_name="on_run_failure",
                    state=state,
                    context=PluginContext(
                        run_id=run_id,
                        orchestration_path=req.orchestration_path,
                        dry_run=req.dry_run,
                        validate_only=req.validate_only,
                        runtime_config=runtime_config,
                        environment=(
                            req.config.environment if req.config is not None else "dev"
                        ),
                    ),
                    error=error_message,
                )
                plugins_meta["events"].extend(failure_events)
            persistence_node = state.setdefault("runtime", {}).get("persistence")
            if isinstance(persistence_node, dict):
                if not persistence_node.get("status"):
                    persistence_node["status"] = "failed"
                persistence_node["error"] = error_message
            already_broken = (
                isinstance(persistence_node, dict)
                and persistence_node.get("status") == "load_failed"
            )
            if persistence is not None and not already_broken:
                # A failed run is exactly the one `--resume <run-id>` most
                # needs to find — a snapshot that only ever saved on
                # success made run-id resume of a failed run impossible.
                # Skipped when this same run already failed to *load* from
                # this backend: it's demonstrably unreachable, so a second
                # call would only clobber that diagnosis with a less useful
                # "save_failed".
                try:
                    persistence.save_run_snapshot(
                        orchestration_path=str(req.orchestration_path),
                        run_id=run_id,
                        ok=False,
                        error=error_message,
                        state=state,
                    )
                    if isinstance(persistence_node, dict):
                        persistence_node["status"] = "persisted"
                        persistence_node["persisted"] = True
                except Exception as persist_exc:
                    if isinstance(persistence_node, dict):
                        persistence_node["status"] = "save_failed"
                        persistence_node["error"] = str(persist_exc)
        except Exception:
            logger.exception("Error during error-handling cleanup")
        return RunResult(
            ok=False,
            state=state,
            warnings=warnings,
            error=error_message,
            out_path=resolved_out,
            interrupted=interrupted,
        )
    finally:
        # The final flush, success or failure: everything recorded after the
        # last effect included, so the mirror ends equal to --out. `warnings`
        # is the same list every already-built RunResult above holds, so
        # appending to it here still reaches whichever one is about to be
        # returned.
        if live_mirror is not None and live_mirror.close(state):
            warnings.append(
                f"Could not keep --live-state {req.live_state_path} in sync with "
                "the run; see the log for details."
            )


def _compose_effect_observers(
    observers: list[Callable[[str, dict[str, Any]], None]],
) -> Callable[[str, dict[str, Any]], None] | None:
    """Fold per-effect observers into the single callback ``Store`` takes."""
    if not observers:
        return None
    if len(observers) == 1:
        return observers[0]

    def fan_out(effect_path: str, effect_node: dict[str, Any]) -> None:
        for observer in observers:
            observer(effect_path, effect_node)

    return fan_out


def _record_library_pins(
    state: dict[str, Any], runtime_config: dict[str, Any] | None
) -> None:
    """Copy the run's `use ref:` pins into `runtime.library_refs`.

    Each pin carries source, ref, resolved path, and — for SHA-pinned remote
    sources — the commit and cache directory it was served from, which is what
    a later offline re-run reproduces.
    """
    from ..core.library_ref import STATE_KEY, collect_pins

    pins = collect_pins(runtime_config)
    if not pins:
        return
    state.setdefault("runtime", {})[STATE_KEY] = pins


def validate(
    orchestration_path: Path,
    *,
    config: CircuitryConfig | None = None,
    skip_preflight: bool = False,
    trust_document: bool = False,
    skip_adapter_check: bool = False,
) -> dict[str, Any]:
    # *trust_document*: the caller named this file by path, as `cof check`
    # does — see RunRequest.trust_document.
    # *skip_adapter_check*: passed straight through to `preflight()`. A
    # caller that always injects its own adapter regardless of the
    # document's own `adapter:` (MCP's HostClaudeAdapter) sets this so a
    # document that validates here also runs — see `preflight`'s own
    # docstring for exactly what stays checked (#265 part 4/9).
    # A skipped (untrusted) project config: the checks below ran without it.
    config_warnings = config.resolution_warnings() if config is not None else []
    text = orchestration_path.read_text(encoding="utf-8").strip()
    if not text:
        return {"ok": False, "errors": ["Orchestration file is empty."], "warnings": config_warnings}

    # Advisory lint (deprecated aliases, type-keyword effect names). Populated
    # as soon as the document parses and carried through every exit below —
    # warnings never change ``ok``, so a file can be valid and still noisy.
    lint_warnings: list[str] = list(config_warnings)

    try:
        orch = load_orchestration_file(orchestration_path)

        from ..core.lint import lint_orchestration
        lint_warnings = [
            *config_warnings,
            *lint_orchestration(orch),
            *unknown_key_warnings(orch),
        ]
        # Host settings the document sets are dropped at run time, or applied
        # with a notice when it is trusted; say which here too, whether or not
        # the rest of the file is valid.
        lint_warnings += orchestration_host_setting_warnings(
            orch,
            config or CircuitryConfig(),
            trust_document=trust_document,
            document_name=orchestration_path.name,
        )

        document_errors = structural_errors(orch)
        if document_errors:
            return {
                "ok": False,
                "errors": document_errors,
                "warnings": lint_warnings,
            }

        # Allowlist gate. Skipped when caller supplies no config — keeps
        # programmatic callers (tests, MCP server, internal scripts)
        # default-open. The CLI resolves a config from disk + env vars and
        # passes it explicitly so AC 0.2 (env-var enforcement) holds.
        if config is not None:
            allowlist_errors = check_allowlist(
                orch=orch, config=config, root_path=orchestration_path
            )
            if allowlist_errors:
                return {
                    "ok": False,
                    "errors": allowlist_errors,
                    "warnings": lint_warnings,
                }

        root_def = compile_orchestration(orch=orch, root_name="prime")

        # Same merge `run()` applies (document `runtime:` key by key over
        # config, trusted document keeps its whole block) — just enough to
        # know the run-wide cap/named groups `cof check` would actually run
        # with, without resolving model/adapter/profile too (#274).
        trusted = trust_document or (config is not None and config.trust_orchestration_runtime)
        orch_runtime_raw = orch.get("runtime")
        orch_runtime = orch_runtime_raw if isinstance(orch_runtime_raw, dict) else {}
        accepted_orch_runtime = (
            dict(orch_runtime)
            if trusted
            else _split_orchestration_runtime(orch_runtime, trusted=False)[0]
        )
        merged_runtime = _merge_runtime(
            (config.runtime if config is not None else {}) or {}, accepted_orch_runtime
        )
        from ..core.concurrency import parse_concurrency_groups, parse_max_concurrency

        _, mc_errors = parse_max_concurrency(merged_runtime.get("max_concurrency"))
        groups, group_cfg_errors = parse_concurrency_groups(
            merged_runtime.get("concurrency_groups")
        )
        concurrency_config_errors = [*mc_errors, *group_cfg_errors]
        if concurrency_config_errors:
            return {
                "ok": False,
                "errors": concurrency_config_errors,
                "warnings": lint_warnings,
            }
        group_name_errors = unknown_concurrency_group_errors(root_def, frozenset(groups))
        if group_name_errors:
            return {
                "ok": False,
                "errors": group_name_errors,
                "warnings": lint_warnings,
            }

        document_adapter = orch.get("adapter")
        lint_warnings += image_asset_warnings(
            orch,
            default_adapter=(
                document_adapter.strip()
                if isinstance(document_adapter, str) and document_adapter.strip()
                else (config.default_adapter if config is not None else None)
            ),
            runtime_cfg=config.runtime if config is not None else None,
        )

        from ..core.cycle_check import detect_cycles
        cycle = detect_cycles(
            orch,
            root_path=orchestration_path,
            runtime=(config.runtime if config is not None else None),
        )
        if cycle is not None:
            return {
                "ok": False,
                "errors": [f"Cycle: {' → '.join(cycle)}"],
                "warnings": lint_warnings,
            }

        # Preflight gate (Story 1). Same gating policy as allowlist — only
        # runs when the caller supplied a config so programmatic callers
        # without an environment-resolved config don't hit network probes.
        # ``skip_preflight`` lets offline / structure-only contexts (CI
        # smoke tests, ``cof check --skip-preflight``) bypass it.
        if config is not None and not skip_preflight:
            preflight_results = preflight(
                orchestration_path, config, skip_adapter_check=skip_adapter_check
            )
            hard_results, soft_results = classify_preflight_results(
                orchestration_path, preflight_results
            )
            preflight_errors = format_preflight_errors(hard_results)
            if preflight_errors:
                return {
                    "ok": False,
                    "errors": preflight_errors,
                    "warnings": lint_warnings,
                }
            lint_warnings = [*lint_warnings, *format_preflight_warnings(soft_results)]

        return {"ok": True, "errors": [], "warnings": lint_warnings}
    except Exception as e:
        return {"ok": False, "errors": [str(e)], "warnings": lint_warnings}


def preflight(
    orchestration_path: Path,
    config: CircuitryConfig,
    *,
    skip_adapter_check: bool = False,
) -> list[tuple[str, CheckResult]]:
    """Walk an orchestration's referenced extensions and call ``check()`` on
    each. Returns an ordered list of ``(label, CheckResult)`` tuples.

    Labels are prefixed by category for actionable error messages:
      * ``adapter:<name>``
      * ``tool:<name>``
      * ``runtime_plugin:<name>``
      * ``library_ref:<ref>``

    Adapters that can only be built at runtime (host_claude needs a
    request_handler) are reported as ok with a deferred message; preflight
    cannot exercise them outside the MCP context.

    *skip_adapter_check*: the caller injected an already-built adapter (e.g.
    MCP's ``HostClaudeAdapter``), which may not be buildable from config at
    all — only the document-level ``adapter:`` check is skipped; tool and
    library-ref preflight still run, so a broken tool config or a bad
    library ref is still caught before any effect runs (#265 part 4). A
    per-effect ``provider:`` (or ``provider_fallbacks``) adapter is still
    checked even then: the injected adapter only ever stands in for the
    document-level default, never for a prompt effect's own named provider,
    which is built from config and actually called at run time regardless
    (``PromptRuntime._resolve_adapter``) (#265 part 9).
    """
    orch = load_orchestration_file(orchestration_path)
    adapter_refs, tool_refs = walk_orchestration_refs(orch)
    checked_adapter_refs = (
        walk_orchestration_refs(orch, include_document_adapter=False)[0]
        if skip_adapter_check
        else adapter_refs
    )
    runtime_cfg = config.runtime or {}
    results: list[tuple[str, CheckResult]] = []

    # `use ref:` entries served by a source that was never refreshed fail here,
    # before any effect runs, carrying the `cof library refresh` command that
    # fixes them. Resolvable-but-missing refs are left to the run's own error.
    from ..core.library_ref import check_use_refs

    for ref, message in check_use_refs(
        orch, root_path=orchestration_path, runtime=runtime_cfg
    ):
        results.append(
            (f"library_ref:{ref}", CheckResult(ok=False, missing=[], message=message))
        )

    for adapter_name in sorted(checked_adapter_refs):
        try:
            adapter = build_adapter(adapter_name=adapter_name, runtime=runtime_cfg)
        except RuntimeError as exc:
            # Adapters that require runtime injection (e.g. host_claude) raise
            # RuntimeError at factory time. Treat as deferred — preflight
            # cannot exercise them, but they are not a failure here.
            results.append(
                (
                    f"adapter:{adapter_name}",
                    CheckResult(
                        ok=True,
                        missing=[],
                        message=f"deferred (runtime-injected): {exc}",
                    ),
                )
            )
            continue
        except ValueError as exc:
            results.append(
                (
                    f"adapter:{adapter_name}",
                    CheckResult(ok=False, missing=[], message=str(exc)),
                )
            )
            continue
        results.append((f"adapter:{adapter_name}", call_check(adapter)))

    for tool_name in sorted(tool_refs):
        try:
            tool = build_plugin(plugin_name=tool_name, runtime=runtime_cfg)
        except (RuntimeError, ValueError) as exc:
            results.append(
                (
                    f"tool:{tool_name}",
                    CheckResult(ok=False, missing=[], message=str(exc)),
                )
            )
            continue
        results.append((f"tool:{tool_name}", call_check(tool)))

    if config.plugins:
        # Plugin load failures are non-fatal warnings — surfaced via the
        # ``runtime.plugins.events`` log, not preflight. Preflight only
        # exercises ``check()`` on plugins that loaded cleanly.
        for load_result in load_plugins(
            list(config.plugins), allowed=config.enabled_plugins
        ):
            if load_result.plugin is None:
                continue
            name = getattr(load_result.plugin, "name", load_result.plugin_id)
            results.append(
                (f"runtime_plugin:{name}", call_check(load_result.plugin))
            )

    return results


def image_asset_warnings(
    orch: dict[str, Any],
    *,
    default_adapter: str | None,
    runtime_cfg: dict[str, Any] | None = None,
) -> list[str]:
    """One warning per prompt whose image ``assets`` go to an adapter that
    cannot send images (see :func:`circuitry.adapters.base.adapter_accepts_images`).

    An effect's adapters are its ``provider:`` (else ``default_adapter``) and
    each of its ``provider_fallbacks``. Walks the same containers as
    :func:`walk_orchestration_refs` and, like it, not into ``use`` children.
    An unknown adapter name is left to the checks that report it.
    """
    from ..adapters.base import adapter_accepts_images
    from .allowlist import _provider_token_to_adapter

    warnings: list[str] = []

    def takes_images(adapter_name: str) -> bool | None:
        try:
            adapter = build_adapter(adapter_name=adapter_name, runtime=runtime_cfg or {})
        except ValueError:
            return None
        except RuntimeError:
            # Runtime-injected (host_claude): it relays text only.
            return False
        return adapter_accepts_images(adapter)

    def walk(effects: Any) -> None:
        if not isinstance(effects, list):
            return
        for effect in effects:
            if not isinstance(effect, dict):
                continue
            etype = effect.get("type")
            if etype == "prompt":
                assets = effect.get("assets")
                if not isinstance(assets, list) or not any(
                    isinstance(a, dict) and a.get("kind") == "image" for a in assets
                ):
                    continue
                primary = effect.get("provider")
                names = [
                    (_provider_token_to_adapter(primary) if isinstance(primary, str) else None)
                    or default_adapter,
                    *(
                        _provider_token_to_adapter(tok)
                        for tok in effect.get("provider_fallbacks") or []
                        if isinstance(tok, str)
                    ),
                ]
                warnings.extend(
                    f"prompt '{effect.get('name')}' has image assets, but adapter "
                    f"'{adapter_name}' cannot send images; it will run without them"
                    for adapter_name in dict.fromkeys(n for n in names if n)
                    if takes_images(adapter_name) is False
                )
            elif etype in ("dynamic", "reflector"):
                walk(effect.get("effects"))
            elif etype in ("if", "conditional"):
                walk(effect.get("then"))
                walk(effect.get("else"))
            elif etype == "loop":
                walk(effect.get("body"))

    if isinstance(orch, dict):
        walk(orch.get("effects"))
    return warnings


def format_preflight_errors(
    results: list[tuple[str, CheckResult]],
) -> list[str]:
    """Render preflight failures as one-line strings for CLI / RunResult."""
    errors: list[str] = []
    for label, r in results:
        if r.ok:
            continue
        parts = [f"{label}: not ready"]
        if r.missing:
            parts.append(f"missing {r.missing}")
        if r.message:
            parts.append(r.message)
        errors.append(" — ".join(parts))
    return errors


def classify_preflight_results(
    orchestration_path: Path,
    results: list[tuple[str, CheckResult]],
) -> tuple[list[tuple[str, CheckResult]], list[tuple[str, CheckResult]]]:
    """Split raw ``preflight()`` results into ``(hard, soft)``.

    A failing ``adapter:<name>`` result is soft — downgraded to a warning —
    when every effect referencing that adapter tolerates failure
    (``on_error: skip``/``continue``): the run degrades gracefully without
    it, so it shouldn't hard-fail the whole orchestration. Everything else
    (already-ok results, tool/runtime_plugin/library_ref results, an unknown
    adapter name, and adapters with at least one non-tolerant usage) stays
    hard: an adapter that doesn't exist is a configuration mistake, not a
    missing credential the run can degrade past.
    """
    orch = load_orchestration_file(orchestration_path)
    usages = collect_adapter_usages(orch)
    hard: list[tuple[str, CheckResult]] = []
    soft: list[tuple[str, CheckResult]] = []
    for label, result in results:
        if result.ok or not label.startswith("adapter:"):
            hard.append((label, result))
            continue
        adapter_name = label.split(":", 1)[1]
        if (
            adapter_name.strip().lower() not in ADAPTER_REGISTRY
            or is_hard_adapter_dependency(adapter_name, usages)
        ):
            # Name the non-skippable effect(s) when we have that detail —
            # the mixed case (one skippable, one not) otherwise reads as an
            # unqualified adapter failure with no clue which effect forces it.
            required_by = hard_effect_names(adapter_name, usages)
            if required_by:
                message = (
                    f"adapter '{adapter_name}' unavailable; "
                    f"required by effects {required_by}"
                )
                if result.message:
                    message += f" ({result.message})"
                named_result = CheckResult(
                    ok=False, missing=result.missing, message=message
                )
                hard.append((label, named_result))
            else:
                hard.append((label, result))
            continue
        effects = skippable_effect_names(adapter_name, usages)
        message = f"adapter '{adapter_name}' unavailable; effects {effects} will skip"
        if result.message:
            message += f" ({result.message})"
        soft.append(
            (label, CheckResult(ok=False, missing=result.missing, message=message))
        )
    return hard, soft


def format_preflight_warnings(
    results: list[tuple[str, CheckResult]],
) -> list[str]:
    """Render soft (skippable) preflight dependencies as one-line warnings."""
    warnings: list[str] = []
    for label, r in results:
        message = r.message or f"{label}: not ready"
        if r.missing:
            message += f" (missing {r.missing})"
        warnings.append(message)
    return warnings


def inspect_orchestration(orchestration_path: Path) -> dict[str, Any]:
    suffix = orchestration_path.suffix.lower()
    summary: dict[str, Any] = {
        "path": str(orchestration_path),
        "format": suffix.lstrip(".") or "unknown",
        "size_bytes": orchestration_path.stat().st_size,
    }

    if suffix in ORCHESTRATION_SUFFIXES:
        data = load_orchestration_file(orchestration_path)
        effects = data.get("effects") or data.get("steps") or []
        effect_names = [
            s.get("name") for s in effects if isinstance(s, dict) and s.get("name")
        ]
        summary["model"] = data.get("model")
        summary["adapter"] = data.get("adapter")
        summary["effects_count"] = len(effects) if isinstance(effects, list) else 0
        summary["effect_names"] = effect_names
        # Backward-compatible aliases.
        summary["steps_count"] = summary["effects_count"]
        summary["step_names"] = effect_names
    else:
        summary["note"] = f"Unsupported format for deep inspection: {suffix}"

    return summary


@dataclass(frozen=True)
class _NoOpAdapter:
    """Stub adapter for tool-only orchestrations that have no prompt effects."""

    name: str = "_noop"

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> Any:
        raise RuntimeError(
            "Attempted to call generate() on a tool-only orchestration. "
            "No prompt effects should be present."
        )

    def check(self) -> CheckResult:
        # Tool-only orchestrations don't exercise an LLM, so the noop
        # adapter's preflight is trivially ready.
        return CheckResult(
            ok=True,
            missing=[],
            message="no-op adapter; no LLM transport.",
        )


def _has_prompt_effects(defn: Any) -> bool:
    """Recursively check if any effect in the tree requires an LLM adapter."""
    from ..core.conditional import ConditionalDefinition
    from ..core.disabled import is_enabled
    from ..core.dynamic import DynamicDefinition
    from ..core.loop import LoopDefinition
    from ..core.prompt import PromptDefinition
    from ..core.reflector import ReflectorDefinition
    from ..core.tool import ToolDefinition

    # A disabled effect never runs, so it never needs an adapter — a profile
    # that switches every prompt off makes the run adapter-free.
    if not is_enabled(defn):
        return False

    if isinstance(defn, PromptDefinition):
        return True
    if isinstance(defn, ToolDefinition):
        # A tool's own expect: {mode: model} calls adapter.generate() the
        # same way a model-mode `if` does (#273) — a tool-only document
        # with one still needs a real adapter, not the no-op.
        return defn.expect is not None and defn.expect.mode == "model"
    if isinstance(defn, DynamicDefinition):
        return any(_has_prompt_effects(e) for e in defn.effects) or any(
            _has_prompt_effects(e) for e in defn.finally_effects
        )
    if isinstance(defn, ConditionalDefinition):
        # `mode: model` is the compiler's default for an `if` condition
        # (core/compiler.py) — it calls generate() itself even when neither
        # branch has its own prompt effect, so it needs a real adapter too
        # (#254).
        if defn.condition.mode == "model":
            return True
        return any(_has_prompt_effects(e) for e in defn.then_effects) or any(
            _has_prompt_effects(e) for e in defn.else_effects
        )
    if isinstance(defn, LoopDefinition):
        if defn.while_def is not None and defn.while_def.mode == "model":
            return True
        return any(_has_prompt_effects(e) for e in defn.body)
    if isinstance(defn, ReflectorDefinition):
        return _has_prompt_effects(defn.inner)

    from ..core.use import UseDefinition

    # Conservatively assume a child orchestration has prompts.
    return isinstance(defn, UseDefinition)


def _require_resolved_settings(
    *, effective: EffectiveSettings, orchestration_path: Path
) -> tuple[str, str]:
    if not effective.adapter:
        raise ValueError(
            "No adapter resolved for orchestration "
            f"{orchestration_path}. Set 'adapter' in the orchestration or "
            "'default_adapter' in config.json. "
            f"(adapter source: {effective.sources.get('adapter')})"
        )
    if not effective.model:
        raise ValueError(
            "No model resolved for orchestration "
            f"{orchestration_path}. Set 'model' in the orchestration or "
            "'default_model' in config.json. "
            f"(model source: {effective.sources.get('model')})"
        )
    return (effective.adapter, effective.model)


def _initialize_plugins(
    plugin_ids: list[str],
    *,
    allowed: list[str] | None = None,
) -> tuple[list[RuntimePlugin], list[dict[str, Any]]]:
    loaded_plugins: list[RuntimePlugin] = []
    events: list[dict[str, Any]] = []

    for result in load_plugins(plugin_ids, allowed=allowed):
        if result.plugin is not None:
            loaded_plugins.append(result.plugin)
            events.append(
                {
                    "plugin": result.plugin_id,
                    "hook": "load",
                    "ok": True,
                    "error": None,
                }
            )
            continue
        events.append(
            {
                "plugin": result.plugin_id,
                "hook": "load",
                "ok": False,
                "error": result.error,
            }
        )

    return (loaded_plugins, events)
