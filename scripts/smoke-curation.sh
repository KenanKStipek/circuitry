#!/usr/bin/env bash
# scripts/smoke-curation.sh — smoke-test every curation orchestration.
#
# Default mode (offline): runs `cof check` against each curation YAML to verify
# it parses and validates against the JSON Schema. No LLM calls. This is the
# mode CI runs.
#
# `--live`: additionally runs `cof run --dry-run` so the compile path executes
# and effective settings resolve. Skips `comic_strip.yml` because it needs
# ComfyUI; pass `CIRCUITRY_RUN_COMFY=1` to include it anyway.
#
# `--only NAME`: run a single orchestration (basename without `.yml`).
#
# Exits non-zero on the first failure.

set -euo pipefail

# Resolve repo root regardless of where the script is invoked from.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BUNDLED_DIR="$REPO_ROOT/src/circuitry/curation"

LIVE=0
ONLY=""

while [ $# -gt 0 ]; do
    case "$1" in
        --live)
            LIVE=1
            shift
            ;;
        --only)
            ONLY="$2"
            shift 2
            ;;
        -h|--help)
            sed -n '2,16p' "$0"
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

if [ ! -d "$BUNDLED_DIR" ]; then
    echo "Curation directory not found: $BUNDLED_DIR" >&2
    exit 1
fi

# Locate cof. Prefer PATH; fall back to repo venv if present.
COF=""
if command -v cof >/dev/null 2>&1; then
    COF="cof"
elif [ -x "$REPO_ROOT/.venv/bin/cof" ]; then
    COF="$REPO_ROOT/.venv/bin/cof"
else
    echo "cof CLI not found on PATH and no .venv/bin/cof present." >&2
    echo "Run 'pip install -e .' from $REPO_ROOT first." >&2
    exit 1
fi

failures=0
checked=0

needs_comfy() {
    case "$(basename "$1" .yml)" in
        comic_strip) return 0 ;;
        _image) return 0 ;;
        *) return 1 ;;
    esac
}

while IFS= read -r -d '' orch; do
    name="$(basename "$orch" .yml)"

    if [ -n "$ONLY" ] && [ "$name" != "$ONLY" ]; then
        continue
    fi

    printf 'check %-40s ' "$name"
    # --skip-preflight: this is a structure-only smoke test; we don't
    # require the binaries / hosts / API keys that the orchestrations
    # would need at run time. preflight readiness is exercised by
    # `cof doctor`.
    if "$COF" check "$orch" --skip-preflight >/dev/null 2>&1; then
        printf '[ok]\n'
    else
        printf '[FAIL]\n'
        "$COF" check "$orch" --skip-preflight || true
        failures=$((failures + 1))
    fi
    checked=$((checked + 1))

    if [ "$LIVE" -eq 1 ]; then
        if needs_comfy "$orch" && [ "${CIRCUITRY_RUN_COMFY:-}" != "1" ]; then
            printf '  live %-40s [skip — needs ComfyUI; set CIRCUITRY_RUN_COMFY=1 to include]\n' "$name"
            continue
        fi
        printf '  live %-40s ' "$name"
        if "$COF" run "$orch" --dry-run --quiet >/dev/null 2>&1; then
            printf '[ok]\n'
        else
            printf '[FAIL]\n'
            "$COF" run "$orch" --dry-run --quiet || true
            failures=$((failures + 1))
        fi
    fi
done < <(find "$BUNDLED_DIR" -type f -name '*.yml' -print0 | sort -z)

echo
echo "Checked $checked orchestration(s); $failures failure(s)."

if [ "$failures" -gt 0 ]; then
    exit 1
fi
