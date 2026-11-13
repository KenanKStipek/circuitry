#!/usr/bin/env python3
"""A self-bounding process that ignores `SIGTERM` (issue #721's own
follow-up, #661): prints "started", then on `SIGTERM` prints "ignored
SIGTERM" and keeps running -- only `SIGKILL` stops it -- so a `stop`
against it holds the state directory's lock through its whole
`grace_ms`, the same way a process that never answers `SIGTERM` would.
It exits on its own after a fixed deadline regardless, so nothing it
starts can ever outlive a test for long.

Usage: ``service_ignores_sigterm.py``
"""

from __future__ import annotations

import os
import signal
import threading
import time


def main() -> int:
    def on_term(signum: int, frame: object) -> None:
        print("ignored SIGTERM", flush=True)

    signal.signal(signal.SIGTERM, on_term)

    def watchdog() -> None:
        time.sleep(30)
        os._exit(0)

    threading.Thread(target=watchdog, daemon=True).start()
    print("started", flush=True)
    while True:
        time.sleep(0.2)


if __name__ == "__main__":
    raise SystemExit(main())
