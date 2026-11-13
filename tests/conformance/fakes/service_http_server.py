#!/usr/bin/env python3
"""A self-bounding HTTP server (issue #721's conformance, #661): answers
`GET` with a plain 200, and exits on its own after 45s. A `service`'s own
leader is *meant* to outlive the run that started it -- the engine that
ran `start` typically exits almost immediately after, reparenting this
process to init, same as a real `cof run`/`electricity run` invocation
always does -- so unlike a plain foreground-command fake, this one is
never bounded by its parent changing, only by the fixed deadline.

Usage: ``service_http_server.py <port>``
"""

from __future__ import annotations

import http.server
import os
import sys
import threading
import time


def main() -> int:
    port = int(sys.argv[1])

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"ok")

        def log_message(self, *args: object) -> None:
            pass

    def watchdog() -> None:
        time.sleep(45)
        os._exit(0)

    threading.Thread(target=watchdog, daemon=True).start()
    server = http.server.HTTPServer(("127.0.0.1", port), Handler)
    server.timeout = 0.2
    server.serve_forever(poll_interval=0.2)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
