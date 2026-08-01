#!/usr/bin/env bash
# Roost one-click installer. Run on macOS or Linux to go from nothing → running:
#   curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install.sh | bash
#
# What it does: gate on Tailscale (required) → install Bun if missing →
# clone/update the repo → `roost quickstart` (coord + local worker + opens
# the browser already-authorized). Other machines are NOT set up here.

set -euo pipefail

REPO_URL="https://github.com/cefege/roost.git"
ROOST_DIR="${ROOST_DIR:-$HOME/Roost}"

say() { printf '>> %s\n' "$1"; }
die() { printf 'ERROR: %s\n' "$1" >&2; shift; for h in "$@"; do printf '  %s\n' "$h" >&2; done; exit 1; }

# 0. macOS or Linux; everything downstream (bun install → roost quickstart →
#    apps/coord/scripts/install.sh) forks launchd vs systemd on its own.
case "$(uname -s)" in
  Darwin|Linux) ;;
  *) die "Roost installs on macOS or Linux only (found $(uname -s))." ;;
esac

# 1. Tailscale check (SOFT). quickstart runs an interactive guide-and-poll gate,
#    so we only WARN here and let the user reach it (cloning + bun install don't
#    need Tailscale). The open-source tailscaled (brew / distro package) needs NO
#    System Settings approval. Was a hard die() that stranded users before the
#    guided gate.
say "checking Tailscale"
if [ "$(uname -s)" = "Darwin" ]; then
  TS_INSTALL_HINT="brew install tailscale"
  TS_START_HINT="sudo tailscale up"
else
  TS_INSTALL_HINT="https://tailscale.com/download/linux"
  TS_START_HINT="sudo systemctl enable --now tailscaled && sudo tailscale up"
fi
if ! command -v tailscale >/dev/null 2>&1; then
  say "WARN: Tailscale not installed — quickstart will guide you ($TS_INSTALL_HINT)."
elif [ "$(tailscale status --json 2>/dev/null | grep -o '"BackendState":[[:space:]]*"[^"]*"' | head -1 | sed 's/.*"\([^"]*\)"$/\1/' || true)" != "Running" ]; then
  say "WARN: Tailscale not running — quickstart will guide you ($TS_START_HINT)."
fi

# 2. Bun.
if ! command -v bun >/dev/null 2>&1; then
  say "installing Bun"
  curl -fsSL https://bun.sh/install | bash
  export PATH="$HOME/.bun/bin:$PATH"
fi
command -v bun >/dev/null 2>&1 || die "Bun install did not land on PATH." "Open a new shell and re-run, or add ~/.bun/bin to PATH."

# 3. Source — clone or update.
if [ -d "$ROOST_DIR/.git" ]; then
  say "updating $ROOST_DIR"
  git -C "$ROOST_DIR" pull --ff-only
else
  say "cloning $REPO_URL → $ROOST_DIR"
  git clone "$REPO_URL" "$ROOST_DIR"
fi

# 4. Install deps + run quickstart (does the rest + opens the browser).
cd "$ROOST_DIR"
say "bun install"
bun install
say "roost quickstart"
exec bun apps/roost-cli/src/main.ts quickstart
