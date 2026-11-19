"""Registers a `faulthandler` dump-every-thread's-stack handler on
SIGUSR1 for every `cof run` child `tests/cli/_signal_test_support.py`'s
`_sandboxed_env` starts (#558): `_diagnose_and_fail` sends that signal,
instead of SIGABRT, to a child stuck past its timeout, so the dump still
happens but macOS's ReportCrash never sees a crash signal to write a
report for.

This directory is only ever reached by putting it first on a child's
PYTHONPATH, which means it is also the first `sitecustomize` module
Python's `site` machinery finds — normally that import skips any other
`sitecustomize.py` later on `sys.path` entirely. This one chains to that
next one itself, so a child started this way still gets whatever that
other `sitecustomize.py` does (on a Homebrew Python install, same-version
guards and `sys.path`/`sys.executable` fixups).

Must never print anything on its default path: a normal `cof run`
child's stdout/stderr is compared byte-for-byte by some of this suite's
own tests. A caller that can't read the child's own stderr at all (osi
owns it, piping it to its own log instead of leaving it to the test)
can still get the dump: setting ``CIRCUITRY_TEST_SIGUSR1_DUMP_FILE``
writes it there instead of to stderr.
"""

from __future__ import annotations

import importlib.util
import os
import signal
import sys
from importlib.machinery import PathFinder

if hasattr(signal, "SIGUSR1"):
    import faulthandler

    _dump_file_path = os.environ.get("CIRCUITRY_TEST_SIGUSR1_DUMP_FILE")
    if _dump_file_path:
        # Kept open for the life of the process: faulthandler writes to
        # this file object whenever SIGUSR1 arrives, not just once here.
        _dump_file = open(_dump_file_path, "a", encoding="utf-8")  # noqa: SIM115
        faulthandler.register(signal.SIGUSR1, file=_dump_file, all_threads=True)
    else:
        faulthandler.register(signal.SIGUSR1, all_threads=True)

_this_dir = os.path.dirname(os.path.abspath(__file__))
_rest_of_path = [p for p in sys.path if os.path.abspath(p or os.curdir) != _this_dir]
_next_spec = PathFinder.find_spec("sitecustomize", path=_rest_of_path)
if _next_spec is not None and _next_spec.loader is not None:
    _next_module = importlib.util.module_from_spec(_next_spec)
    _next_spec.loader.exec_module(_next_module)
