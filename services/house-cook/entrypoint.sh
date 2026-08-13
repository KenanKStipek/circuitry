#!/usr/bin/env bash
# house-cook entrypoint: authenticate once (device flow — the user code is
# printed to THESE LOGS; approve it in the CyberDiner web app), then serve.
set -uo pipefail

echo "[house-cook] starting; tier subscriptions: ${TIER_SUBSCRIPTIONS:-cheap}"

while true; do
  # If a credential exists, run serves immediately. If not, run fails fast;
  # fall through to login, which prints the device code and polls for approval.
  cookd run
  status=$?
  echo "[house-cook] cookd run exited ($status) — attempting login (watch these logs for the device code)"
  cookd login || true
  echo "[house-cook] retrying run in 10s"
  sleep 10
done
