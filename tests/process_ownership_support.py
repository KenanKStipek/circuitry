"""Whether a recorded pid or pgid is still provably the test that recorded
it owns it (#561): macOS reuses process ids quickly on a busy machine, so a
stale id a test captured earlier may already name an unrelated process's
pid or process group by the time its teardown runs. A teardown may signal
an id only once this returns ``True`` for it.
"""

from __future__ import annotations

import subprocess
from collections.abc import Sequence

#: Column index of the id ``by`` selects, in the ``ps`` output below.
_COLUMNS = {"pid": 0, "pgid": 1}


def owned(target: int, tokens: Sequence[str], *, by: str = "pgid") -> bool:
    """Whether some live, non-zombie process whose pid or pgid (selected by
    *by*) is *target* carries one of *tokens* in its command line.

    Built on ``ps -ww`` (always: Linux truncates the command column
    otherwise -- see #554) so a token further in than a narrower width
    would cut it off is still found.
    """
    column = _COLUMNS[by]
    out = subprocess.run(
        ["ps", "-ww", "-eo", "pid,pgid,stat,command"],
        capture_output=True,
        text=True,
        check=False,
    )
    for line in out.stdout.splitlines()[1:]:
        fields = line.split(None, 3)
        if len(fields) < 4:
            continue
        pid_s, pgid_s, stat, command = fields
        if not pid_s.isdigit() or not pgid_s.isdigit():
            continue
        if int(fields[column]) != target:
            continue
        if stat.startswith("Z"):  # a zombie's command is never the real one
            continue
        if any(token in command for token in tokens):
            return True
    return False
