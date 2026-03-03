#!/bin/sh
# Circuitry — Cybernetic orchestration framework (COF)
# Installer — installs via pipx for isolated CLI usage.
# Usage: curl -fsSL https://raw.githubusercontent.com/kenankstipek/circuitry/main/scripts/install.sh | sh
set -e

REPO_URL="git+https://github.com/kenankstipek/circuitry.git"
MIN_PYTHON="3.9"

log()  { printf '  \033[1;36m%s\033[0m %s\n' "$1" "$2"; }
err()  { printf '  \033[1;31m%s\033[0m %s\n' "ERROR" "$1" >&2; exit 1; }
ok()   { printf '  \033[1;32m%s\033[0m %s\n' "OK" "$1"; }

# --- Check Python ---
PYTHON=""
for cmd in python3 python; do
    if command -v "$cmd" >/dev/null 2>&1; then
        ver=$("$cmd" -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')" 2>/dev/null || true)
        if [ -n "$ver" ]; then
            major=$(echo "$ver" | cut -d. -f1)
            minor=$(echo "$ver" | cut -d. -f2)
            if [ "$major" -gt 3 ] || { [ "$major" -eq 3 ] && [ "$minor" -ge 9 ]; }; then
                PYTHON="$cmd"
                break
            fi
        fi
    fi
done

[ -z "$PYTHON" ] && err "Python >= $MIN_PYTHON is required but not found."
ok "Python $ver ($PYTHON)"

# --- Check / install pipx ---
if ! command -v pipx >/dev/null 2>&1; then
    log "INSTALL" "pipx not found, installing..."
    "$PYTHON" -m pip install --user pipx 2>/dev/null || "$PYTHON" -m pip install pipx
    "$PYTHON" -m pipx ensurepath
    # Reload PATH for this session
    export PATH="$HOME/.local/bin:$PATH"
fi
ok "pipx available"

# --- Install or upgrade circuitry ---
if pipx list 2>/dev/null | grep -q "circuitry"; then
    log "UPGRADE" "Upgrading circuitry..."
    pipx upgrade circuitry || pipx install --force "$REPO_URL"
else
    log "INSTALL" "Installing circuitry..."
    pipx install "$REPO_URL"
fi

# --- Verify ---
if command -v cof >/dev/null 2>&1; then
    ok "circuitry installed: $(cof version 2>&1 || echo 'unknown version')"
else
    # pipx may need PATH refresh
    export PATH="$HOME/.local/bin:$PATH"
    if command -v cof >/dev/null 2>&1; then
        ok "circuitry installed: $(cof version 2>&1 || echo 'unknown version')"
    else
        err "Installation succeeded but 'cof' not found in PATH. Run: pipx ensurepath"
    fi
fi

echo ""
log "NEXT" "Run 'cof init' to get started or 'cof doctor' to check your setup."
