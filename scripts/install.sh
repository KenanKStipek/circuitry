#!/bin/sh
# Circuitry — Cybernetic orchestration framework (COF)
# Installer — installs via pipx for isolated CLI usage.
# Usage: curl -fsSL https://raw.githubusercontent.com/kenankstipek/circuitry/main/scripts/install.sh | sh
set -e

REPO_URL="git+https://github.com/kenankstipek/circuitry.git"
PIPX_PACKAGE="circuitry-cof"
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
# The PyPI distribution name is `circuitry-cof` (the unprefixed `circuitry`
# name on PyPI belongs to a different package). The Python import name remains
# `circuitry`, and the CLI is `cof`.
if pipx list 2>/dev/null | grep -qE "(^|[^-])$PIPX_PACKAGE\b"; then
    log "UPGRADE" "Upgrading $PIPX_PACKAGE..."
    pipx upgrade "$PIPX_PACKAGE" || pipx install --force "$REPO_URL"
else
    log "INSTALL" "Installing $PIPX_PACKAGE..."
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

# --- Create global config directory ---
CONFIG_DIR="$HOME/.config/circuitry"
if [ ! -d "$CONFIG_DIR" ]; then
    mkdir -p "$CONFIG_DIR"
    ok "Created $CONFIG_DIR"
fi

# --- Seed default config if none exists ---
CONFIG_FILE="$CONFIG_DIR/config.json"
if [ ! -f "$CONFIG_FILE" ]; then
    cat > "$CONFIG_FILE" << 'CONF'
{
  "default_model": "llama3:latest",
  "default_adapter": "ollama",
  "runtime": {
    "adapters": {
      "ollama": {
        "base_url": "http://localhost:11434",
        "timeout_seconds": 6000
      }
    }
  }
}
CONF
    ok "Created default config: $CONFIG_FILE"
else
    log "SKIP" "Config already exists: $CONFIG_FILE"
fi

# --- First-run guidance ---
echo ""
echo "  ┌─────────────────────────────────────────────┐"
echo "  │        Circuitry installed successfully!     │"
echo "  ├─────────────────────────────────────────────┤"
echo "  │                                             │"
echo "  │  Get started:                               │"
echo "  │    cof list          Browse orchestrations  │"
echo "  │    cof run hello -e name=World   Try it!    │"
echo "  │    cof init          Start a new project    │"
echo "  │    cof doctor        Check your setup       │"
echo "  │                                             │"
echo "  │  Config: ~/.config/circuitry/config.json    │"
echo "  │                                             │"
echo "  └─────────────────────────────────────────────┘"
