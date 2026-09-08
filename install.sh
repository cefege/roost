#!/usr/bin/env bash
# Roost one-click installer. Run on macOS or Linux to go from nothing → running:
#   curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install.sh | \
#     ROOST_WEB_PUBLIC_URL="https://roost.example.com" bash
#
# What it does: install Bun if missing → clone/update the repo →
# `roost quickstart --coordinator-url "$ROOST_WEB_PUBLIC_URL"` (coord + local
# worker + opens the browser already-authorized). Other machines are NOT set
# up here, and no front door (TLS, DNS, tunnel) is installed: you put Caddy,
# nginx, a Cloudflare tunnel or `tailscale serve` in front of the coordinator's
# loopback listener yourself, then name it in ROOST_WEB_PUBLIC_URL.

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

# 1. Required env — the public URL your front door serves. The coordinator
#    binds loopback plaintext and never provisions TLS, DNS, or a tunnel.
if [ -z "${ROOST_WEB_PUBLIC_URL:-}" ]; then
  die "ROOST_WEB_PUBLIC_URL is required." \
      "It is the HTTPS origin your own front door serves, e.g.:" \
      "  curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install.sh | \\" \
      "    ROOST_WEB_PUBLIC_URL=\"https://roost.example.com\" bash" \
      "Set up that front door first (Caddy, nginx, a Cloudflare tunnel, or" \
      "\`tailscale serve\`) — see GETTING_STARTED.md for copy-paste recipes."
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
say "roost quickstart --coordinator-url $ROOST_WEB_PUBLIC_URL"
exec bun apps/roost-cli/src/main.ts quickstart --coordinator-url "$ROOST_WEB_PUBLIC_URL"
