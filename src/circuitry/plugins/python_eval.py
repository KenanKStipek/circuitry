"""Sandboxed Python evaluation tool plugin via RestrictedPython.

Optional dep: ``RestrictedPython``. Install with
``pip install circuitry-cof[python_eval]``.

Runs a small Python expression or statement block under
RestrictedPython's compile_restricted, with a curated builtins map and
no access to file/network/process APIs.

Params:
  - ``code`` (required): Python source to execute.
  - ``inputs`` (optional, dict): variables made available to the code.
    Names must be valid identifiers and not start with ``_``. Mirrored
    into both globals and locals, so they're visible inside comprehension
    bodies too (see note below).
  - ``mode`` (optional, default ``"eval"``):
    * ``"eval"`` — evaluates a single expression; ``value`` = its result.
    * ``"exec"`` — executes a statement block; ``value`` = the
      ``result`` variable from the local namespace, or None if absent.

Subscript assignment (``x[k] = v``) and augmented assignment of names
(``n += 1``) are supported: RestrictedPython rewrites these to calls
against ``_write_``/``_inplacevar_`` guards, which this plugin wires up.
``_write_`` (``full_write_guard``) permits item assignment/deletion on
plain ``list``/``dict`` and wraps everything else, so ``x.attr = v``
virtually always raises ``TypeError`` — ``list``/``dict`` have no
settable attributes either, so attribute assignment isn't really usable
from sandboxed code. Augmented assignment of subscripts/attributes
(``x[k] += v``) is rejected at compile time by RestrictedPython itself,
not just Names. ``_inplacevar_`` only uses a real in-place operator
(mutating the target) for ``list``/``dict`` — the same types
``full_write_guard`` treats as safe; for every other type it falls back
to the plain binary operator, so ``x += y`` can't call a target's
``__iadd__``/``__ior__``/etc. to mutate it in place and bypass
``_write_``.

Note: ``eval``/``exec`` with separate globals/locals dicts make comprehension
and generator-expression bodies a nested scope that only sees globals, not
the enclosing locals — a plain CPython quirk, not specific to
RestrictedPython. Without mirroring ``inputs`` into globals,
``[x for x in range(int(w))]`` would raise ``NameError: name 'w' is not
defined`` even though ``w`` is a top-level input. Only ``inputs`` are
mirrored this way — names defined by the code itself (e.g. ``n = 3``) are
still invisible inside a comprehension/generator-expression body, same as
plain CPython.

AC C.5: payloads outside the sandbox (``__import__``, attribute access
starting with ``_``) are rejected at compile time before any side effect.
Plain ``import`` statements compile (the syntax itself isn't sandboxed)
but fail at runtime with ``ImportError`` because ``__import__`` isn't in
the sandboxed builtins.

The compile + eval/exec step runs in a forked child process so the
effect's ``timeout_ms`` budget can be enforced from outside, and so a
cancelled run (Ctrl-C/SIGTERM/SIGHUP, #357 follow-up) kills it at once
instead of waiting out that same budget: the parent registers the child
with ``core.cancellation.get_token().track_process`` for exactly as long
as it is blocked waiting on the result, so ``request()`` (called from the
signal handler, on whichever thread actually holds the cancellation
token — a tree-flow branch running this on a worker thread never sees a
signal directly) can kill it from there, by pid only, never its process
group (it shares cof's own). A tight ``while True: pass`` loop (or
anything else that never returns control) is killed on overrun instead
of hanging the run forever either way. The parent and
child talk over an explicit ``Pipe`` (not a ``SimpleQueue``): the parent
closes its copy of the write end right after starting the child, and
reads with a ``poll()``/``recv()`` deadline *before* joining. Both parts
matter — a ``SimpleQueue`` keeps the parent's own write-end fd open
forever, so a plain blocking ``get()`` never sees EOF (and so never
returns) if the child dies without writing (crash, OOM-kill, or a
CPU-limit race below); and reading only after ``join()`` deadlocks on
any result bigger than the pipe's OS buffer (tens of KiB), since nothing
is draining the pipe while the child's write blocks. The child also sets
``RLIMIT_CPU`` a few seconds *above* the wall-clock budget (POSIX only)
so the wall-clock deadline — with its clearer error message — is what
fires for a CPU-bound loop, not a race between the two; and ``RLIMIT_AS``
relative to the child's own memory usage at fork time (read from
``/proc/self/status``, Linux only — macOS does not enforce ``RLIMIT_AS``
at all) rather than an absolute number, since an absolute cap could
already be below what the parent (and therefore the forked child) has
mapped before the child's own code runs a single line. Both are a
backstop against CPU-bound or memory-bomb code even if something
upstream fails to join the child. ``fork`` (not ``spawn``) is required:
``inputs`` may hold arbitrary live Python objects (closures,
locally-defined classes — see
``TestWriteGuard.test_write_to_frozen_dataclass_rejected``), and only
``fork`` gives the child the same memory instead of needing to pickle
them across a process boundary.
"""

from __future__ import annotations

import importlib.util
import keyword
import math
import multiprocessing
import operator
import signal
from dataclasses import dataclass
from typing import Any, Literal

from ..core.cancellation import get_token
from ..preflight import CheckResult
from .base import ToolResult

try:
    import resource
except ImportError:  # Windows: no POSIX resource limits; fork is also unavailable there.
    resource = None  # type: ignore[assignment]

# Virtual-address-space headroom for the sandboxed child, added on top of
# whatever the child's own baseline usage already is at fork time (Linux
# only; macOS doesn't enforce RLIMIT_AS at all, see module docstring). An
# absolute cap would fail trivial code whenever the parent's own address
# space (worker threads, heavy optional deps already imported elsewhere in
# the process) exceeds it before the child's code runs a single line.
_MEMORY_HEADROOM_BYTES = 1024 * 1024 * 1024  # 1 GiB

# RLIMIT_CPU is set this many seconds above the wall-clock budget so the
# wall-clock deadline (clearer error, always fires) wins the race against
# the CPU limit for CPU-bound code, instead of SIGXCPU landing microseconds
# before the parent's own join() deadline and leaving a child that exited
# without ever writing a result.
_CPU_LIMIT_MARGIN_SECONDS = 5

try:
    _FORK_CONTEXT: multiprocessing.context.ForkContext | None = multiprocessing.get_context("fork")
except ValueError:
    _FORK_CONTEXT = None

# RestrictedPython rewrites `x += y` (etc.) to `x = _inplacevar_('+=', x, y)`
# for Name targets — augmented assignment of attributes/subscripts is
# rejected at compile time instead, so this only needs to cover Name
# rebinding. Keys match the operator strings RestrictedPython's transformer
# emits (`IOPERATOR_TO_STR`). Each entry is (in-place fn, plain binary fn):
# the in-place fn is only used for types `full_write_guard` treats as safe
# (`list`/`dict`) — anything else falls back to the plain operator so
# `x += y` can't reach a target's `__iadd__`/`__ior__`/etc. to mutate it
# in place and bypass `_write_` (mirrors Zope's `protected_inplacevar`).
_INPLACE_OPS: dict[str, tuple[Any, Any]] = {
    "+=": (operator.iadd, operator.add),
    "-=": (operator.isub, operator.sub),
    "*=": (operator.imul, operator.mul),
    "/=": (operator.itruediv, operator.truediv),
    "//=": (operator.ifloordiv, operator.floordiv),
    "%=": (operator.imod, operator.mod),
    "**=": (operator.ipow, operator.pow),
    "<<=": (operator.ilshift, operator.lshift),
    ">>=": (operator.irshift, operator.rshift),
    "|=": (operator.ior, operator.or_),
    "^=": (operator.ixor, operator.xor),
    "&=": (operator.iand, operator.and_),
    "@=": (operator.imatmul, operator.matmul),
}

# Types `full_write_guard` treats as safe to mutate directly (see
# RestrictedPython.Guards._full_write_guard's `safetypes`).
_INPLACE_SAFE_TYPES = (list, dict)


def _inplacevar(op: str, x: Any, y: Any) -> Any:
    try:
        inplace_fn, binary_fn = _INPLACE_OPS[op]
    except KeyError:
        raise TypeError(f"python_eval: unsupported augmented assignment {op!r}") from None
    func = inplace_fn if type(x) in _INPLACE_SAFE_TYPES else binary_fn
    return func(x, y)


# Tiny safe-builtins set — math + string handling only.
_SAFE_BUILTINS = {
    "abs": abs, "all": all, "any": any, "bool": bool, "dict": dict,
    "divmod": divmod, "enumerate": enumerate, "filter": filter,
    "float": float, "frozenset": frozenset, "hash": hash, "hex": hex,
    "int": int, "isinstance": isinstance, "issubclass": issubclass,
    "iter": iter, "len": len, "list": list, "map": map, "max": max,
    "min": min, "next": next, "oct": oct, "ord": ord, "pow": pow,
    "range": range, "repr": repr, "reversed": reversed, "round": round,
    "set": set, "slice": slice, "sorted": sorted, "str": str, "sum": sum,
    "tuple": tuple, "zip": zip,
    "True": True, "False": False, "None": None,
}


def _validate_input_names(inputs: dict[str, Any]) -> None:
    for k in inputs:
        if not isinstance(k, str):
            raise ValueError(
                f"python_eval: input keys must be strings, got {type(k).__name__}"
            )
        if not k.isidentifier() or keyword.iskeyword(k):
            raise ValueError(
                f"python_eval: input name {k!r} is not a valid identifier."
            )
        if k.startswith("_"):
            raise ValueError(
                f"python_eval: input name {k!r} cannot start with underscore."
            )


def _current_vm_size_bytes() -> int | None:
    """The calling process's own virtual memory size, in bytes.

    Read from ``/proc/self/status`` (Linux only; returns ``None``
    anywhere else, including macOS, where ``RLIMIT_AS`` isn't enforced
    anyway). Used to float the child's memory cap relative to what's
    already mapped at fork time rather than an absolute number that
    could already be exceeded.
    """
    try:
        with open("/proc/self/status", encoding="ascii") as f:
            for line in f:
                if line.startswith("VmSize:"):
                    return int(line.split()[1]) * 1024
    except (OSError, ValueError, IndexError):
        return None
    return None


def _apply_resource_limits(cpu_seconds: int) -> None:
    """Best-effort CPU/memory caps for the sandboxed child (POSIX only).

    Both calls are wrapped individually: a platform that rejects one
    (macOS doesn't actually enforce ``RLIMIT_AS``, see module docstring)
    shouldn't lose the other.
    """
    if resource is None:
        return
    try:
        resource.setrlimit(resource.RLIMIT_CPU, (cpu_seconds, cpu_seconds))
    except (ValueError, OSError):
        pass
    base_vm = _current_vm_size_bytes()
    if base_vm is not None:
        limit = base_vm + _MEMORY_HEADROOM_BYTES
        try:
            resource.setrlimit(resource.RLIMIT_AS, (limit, limit))
        except (ValueError, OSError):
            pass


def _put_result(result_conn: Any, status: str, payload: Any) -> None:
    """Send ``(status, payload)`` over *result_conn*, a ``Connection``'s
    write end. ``send()`` pickles synchronously before writing, so a
    payload that can't cross the process boundary (an exception from a
    sandboxed-defined class, an unpicklable result) is caught here rather
    than vanishing silently.
    """
    try:
        result_conn.send((status, payload))
    except Exception:
        if status == "ok":
            result_conn.send(("error", RuntimeError(f"python_eval: result is not picklable: {payload!r}")))
        else:
            result_conn.send(("error", RuntimeError(f"{type(payload).__name__}: {payload}")))


def _run_sandboxed(
    code: str,
    mode: Literal["eval", "exec"],
    inputs: dict[str, Any],
    cpu_seconds: int,
    result_conn: Any,
) -> None:
    """Child-process entry point. Compiles and runs *code* exactly as the
    in-process version used to, then sends the outcome back over
    *result_conn* as a ``(status, payload)`` pair. ``status`` is ``"ok"``
    (payload is the result value) or ``"error"`` (payload is the exception
    to re-raise, unchanged, in the parent). *result_conn* is closed before
    returning either way, so the parent's read reliably sees EOF once this
    function is done, instead of relying on process exit alone.

    Resets SIGINT/SIGTERM/SIGHUP to ``SIG_DFL`` first: this is a forked
    child, sharing the parent's own process group, so a terminal Ctrl-C or
    hangup reaches it too, and it would otherwise inherit `cli.interrupts`'
    handlers along with its own *copies* of the parent's
    ``CancellationToken`` tracked-process sets -- a ``request()`` run from
    inside the child would have nothing useful to do (the parent kills this
    same child independently via :meth:`CancellationToken.track_process`)
    and could crash (:func:`kill_tracked_process`'s ``is_alive()`` raises
    outside a child's own ``multiprocessing`` bookkeeping) (#357 review).
    A signal already ``SIG_IGN`` on entry is left that way rather than
    reset to ``SIG_DFL``: under `nohup`, the parent's SIGHUP (and an
    embedder that ignores SIGINT) is ``SIG_IGN`` precisely so a hangup
    cannot kill it, and this child shares that process group -- resetting
    an inherited ``SIG_IGN`` to ``SIG_DFL`` would make the same hangup
    kill this step's child when the run itself should carry on (#357
    review finding 2).
    """
    for sig in (signal.SIGINT, signal.SIGTERM, getattr(signal, "SIGHUP", None)):
        if sig is not None and signal.getsignal(sig) is not signal.SIG_IGN:
            signal.signal(sig, signal.SIG_DFL)
    try:
        _run_sandboxed_inner(code, mode, inputs, cpu_seconds, result_conn)
    finally:
        result_conn.close()


def _run_sandboxed_inner(
    code: str,
    mode: Literal["eval", "exec"],
    inputs: dict[str, Any],
    cpu_seconds: int,
    result_conn: Any,
) -> None:
    _apply_resource_limits(cpu_seconds)

    try:
        # Already imported in the parent before forking (see `execute`),
        # so this is a `sys.modules` lookup, not a fresh disk import —
        # the fork inherited the parent's loaded module.
        from RestrictedPython import (  # type: ignore[import-not-found]
            compile_restricted,
            safe_globals,
        )
        from RestrictedPython.Eval import (  # type: ignore[import-not-found]
            default_guarded_getitem,
        )
        from RestrictedPython.Guards import (  # type: ignore[import-not-found]
            full_write_guard,
            guarded_iter_unpack_sequence,
            guarded_unpack_sequence,
            safer_getattr,
        )
    except ImportError as exc:
        _put_result(
            result_conn,
            "error",
            RuntimeError(
                "python_eval: RestrictedPython not installed. "
                f"Install with: pip install RestrictedPython ({exc})"
            ),
        )
        return

    # Build the evaluation environment. ``safe_globals`` contains
    # RestrictedPython's runtime helpers; we extend it with our
    # curated builtins.
    env_globals: dict[str, Any] = dict(safe_globals)
    env_globals["__builtins__"] = dict(_SAFE_BUILTINS)
    env_globals["_getitem_"] = default_guarded_getitem
    env_globals["_getattr_"] = safer_getattr
    env_globals["_getiter_"] = iter
    env_globals["_iter_unpack_sequence_"] = guarded_iter_unpack_sequence
    env_globals["_unpack_sequence_"] = guarded_unpack_sequence
    env_globals["_write_"] = full_write_guard
    env_globals["_inplacevar_"] = _inplacevar
    # `eval`/`exec` with separate globals/locals make comprehension
    # bodies a nested scope that only sees globals — mirror inputs into
    # both so names from `inputs` resolve the same way whether they're
    # used at the top level or inside a comprehension.
    env_globals.update(inputs)
    env_locals = dict(inputs)

    try:
        compiled = compile_restricted(code, filename="<python_eval>", mode=mode)
    except SyntaxError as exc:
        # RestrictedPython raises SyntaxError for sandbox violations
        # (forbidden imports, dunder access, etc).
        _put_result(
            result_conn, "error", PermissionError(f"python_eval: rejected by sandbox: {exc}")
        )
        return

    try:
        if mode == "eval":
            value: Any = eval(compiled, env_globals, env_locals)  # noqa: S307
        else:
            exec(compiled, env_globals, env_locals)  # noqa: S102
            value = env_locals.get("result")
    except BaseException as exc:  # re-raised as-is in the parent
        _put_result(result_conn, "error", exc)
        return

    _put_result(result_conn, "ok", value)


@dataclass(frozen=True)
class PythonEvalPlugin:
    name: str = "python_eval"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        # Validate params first so callers get a clean ValueError even
        # when RestrictedPython isn't installed or fork is unavailable.
        code = params.get("code")
        if not isinstance(code, str) or not code.strip():
            raise ValueError("python_eval requires params['code'].")
        mode_raw = str(params.get("mode") or "eval").lower()
        mode: Literal["eval", "exec"]
        if mode_raw == "eval":
            mode = "eval"
        elif mode_raw == "exec":
            mode = "exec"
        else:
            raise ValueError(f"python_eval: mode must be eval|exec, got {mode_raw!r}")
        inputs = params.get("inputs") or {}
        if not isinstance(inputs, dict):
            raise ValueError("python_eval: params['inputs'] must be a dict.")
        _validate_input_names(inputs)

        try:
            # A real import, not just find_spec: forking after this means
            # the child inherits an already-loaded module instead of
            # re-importing it from disk on every call, and an installed-
            # but-broken package fails here with a clear ImportError
            # instead of surfacing confusingly inside the child.
            import RestrictedPython  # noqa: F401  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "python_eval: RestrictedPython not installed. "
                "Install with: pip install RestrictedPython"
            ) from exc
        if _FORK_CONTEXT is None:
            raise RuntimeError(
                "python_eval: this platform has no 'fork' multiprocessing start "
                "method, which the sandboxed child process requires."
            )

        wall_seconds = max(1, math.ceil(timeout_seconds))
        cpu_seconds = wall_seconds + _CPU_LIMIT_MARGIN_SECONDS
        read_conn, write_conn = _FORK_CONTEXT.Pipe(duplex=False)
        proc = _FORK_CONTEXT.Process(
            target=_run_sandboxed,
            args=(code, mode, inputs, cpu_seconds, write_conn),
            daemon=True,
        )
        try:
            proc.start()
        except Exception as exc:
            read_conn.close()
            write_conn.close()
            raise RuntimeError(
                f"python_eval: failed to start the sandboxed process: {exc}"
            ) from exc
        # The child has its own copy of write_conn (forking doesn't close
        # anything); the parent must close its copy too, or the pipe never
        # reports EOF — and poll()/recv() below would block forever — if
        # the child dies without ever sending a result.
        write_conn.close()

        # Registered only for the stretch where the parent is actually
        # blocked waiting on it -- a cancelled run's signal handler
        # (whichever thread holds the token; this call itself may be on a
        # tree-flow branch's own worker thread, which a signal never
        # reaches directly) kills this process the moment cancellation is
        # requested rather than waiting out `wall_seconds` (#357 follow-up).
        # Killing the child closes its copy of `write_conn`, which is what
        # unblocks `poll()`/`recv()` below -- the existing timeout/EOF
        # handling after it covers the rest.
        with get_token().track_process(proc):
            # Poll-then-recv, not join()-then-recv: draining the pipe while
            # the child is still writing is what lets a result bigger than
            # the OS pipe buffer (tens of KiB) get through instead of
            # deadlocking the child's write for the whole budget.
            if not read_conn.poll(wall_seconds):
                read_conn.close()
                proc.terminate()
                proc.join(2)
                if proc.is_alive():
                    proc.kill()
                    proc.join()
                raise RuntimeError(f"python_eval: exceeded timeout of {wall_seconds}s")

            try:
                status, payload = read_conn.recv()
            except EOFError as exc:
                read_conn.close()
                proc.join(2)
                if proc.is_alive():
                    proc.kill()
                    proc.join()
                raise RuntimeError(
                    "python_eval: sandboxed process exited unexpectedly "
                    f"(exit code {proc.exitcode})."
                ) from exc

        read_conn.close()
        # Bounded, not unbounded: a live callable passed in via `inputs`
        # could start a non-daemon thread in the child, which would keep
        # the process alive indefinitely even after it has sent a result.
        proc.join(2)
        if proc.is_alive():
            proc.kill()
            proc.join()

        if status == "error":
            raise payload

        return ToolResult(
            value=payload,
            raw={"mode": mode},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        if importlib.util.find_spec("RestrictedPython") is None:
            return CheckResult(
                ok=False,
                missing=["library:RestrictedPython"],
                message="pip install RestrictedPython",
            )
        return CheckResult(ok=True, missing=[])
