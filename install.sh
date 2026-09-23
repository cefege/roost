#!/usr/bin/env bash
# Roost one-click installer. Run on macOS or Linux to go from nothing → running:
#   curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install.sh | bash
#
# What it does: install Bun if missing → clone/update the repo → `roost quickstart`
# (coord + local worker + opens the browser). Set ROOST_WEB_PUBLIC_URL only when
# an operator-managed front door should be selected; Roost never provisions TLS,
# DNS, or a tunnel for that loopback coordinator.

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

# 1. Bun.
if ! command -v bun >/dev/null 2>&1; then
  say "installing Bun"
  curl -fsSL https://bun.sh/install | bash
  export PATH="$HOME/.bun/bin:$PATH"
fi
command -v bun >/dev/null 2>&1 || die "Bun install did not land on PATH." "Open a new shell and re-run, or add ~/.bun/bin to PATH."

# 2. Source — clone or update.
if [ -d "$ROOST_DIR/.git" ]; then
  say "updating $ROOST_DIR"
  git -C "$ROOST_DIR" pull --ff-only
else
  say "cloning $REPO_URL → $ROOST_DIR"
  git clone "$REPO_URL" "$ROOST_DIR"
fi

# 3. Install deps + run quickstart (does the rest + opens the browser).
cd "$ROOST_DIR"
say "bun install"
bun install
quickstart_args=()
if [[ -n "${ROOST_WEB_PUBLIC_URL:-}" ]]; then
  quickstart_args=(--coordinator-url "$ROOST_WEB_PUBLIC_URL")
fi
say "roost quickstart${ROOST_WEB_PUBLIC_URL:+ --coordinator-url $ROOST_WEB_PUBLIC_URL}"
exec bun apps/roost-cli/src/main.ts quickstart "${quickstart_args[@]}"
