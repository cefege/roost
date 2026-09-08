#!/usr/bin/env bash
# Roost pull-based worker join. Run on a NEW machine (macOS or Linux) to go
# from nothing → registered worker, no SSH/push from the coordinator:
#   curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/join.sh | \
#     ROOST_COORDINATOR_URL="https://roost.example.com" \
#     ROOST_BOOTSTRAP_TOKEN="roost_bt_…" [ROOST_WORKER_LABEL="my-box"] bash
#
# Get that one-liner from `roost add-machine` on the coordinator (or the web
# Settings → Machines → Add machine dialog). What it does: install Bun if
# missing → clone/update the repo → pin the checkout to the coordinator's live
# commit (so the drift badge stays quiet) → `bun install` → `roost join`
# (installs + registers the local worker).

set -euo pipefail

REPO_URL="https://github.com/cefege/roost.git"
ROOST_DIR="${ROOST_DIR:-$HOME/Roost}"

say() { printf '>> %s\n' "$1"; }
die() { printf 'ERROR: %s\n' "$1" >&2; shift; for h in "$@"; do printf '  %s\n' "$h" >&2; done; exit 1; }

# 0. macOS (launchd) or Linux (systemd --user). Nothing else has a service
# installer in apps/worker/scripts/install.sh.
case "$(uname -s)" in
  Darwin|Linux) ;;
  *) die "Roost joins on macOS or Linux only (found $(uname -s))." ;;
esac

# 0b. Bun — and therefore Roost — requires macOS 13.0+. An older Mac cannot run
# a worker at all; print the verdict before anything is installed rather than
# leaving behind a Bun that dies on launch and a LaunchAgent that never starts.
# POSIX-only constructs here: stock macOS ships bash 3.2.
if [ "$(uname -s)" = "Darwin" ]; then
  MACOS_VERSION="$(sw_vers -productVersion 2>/dev/null || true)"
  MACOS_MAJOR="$(printf '%s' "$MACOS_VERSION" | cut -d. -f1)"
  case "$MACOS_MAJOR" in
    ''|*[!0-9]*) MACOS_MAJOR=0 ;;
  esac
  if [ "$MACOS_MAJOR" -lt 13 ]; then
    die "This Mac runs macOS ${MACOS_VERSION:-unknown}; Roost needs macOS 13 (Ventura) or newer because Bun does." \
        "Options: upgrade this Mac to macOS 13+, or run the worker on a different machine." \
        "Nothing was installed."
  fi
fi

# 1. Required env — the join target + credential come from `roost add-machine`.
if [ -z "${ROOST_COORDINATOR_URL:-}" ] || [ -z "${ROOST_BOOTSTRAP_TOKEN:-}" ]; then
  die "ROOST_COORDINATOR_URL and ROOST_BOOTSTRAP_TOKEN are required." \
      "Run \`roost add-machine --platform macos\` or \`roost add-machine --platform linux\` on your coordinator" \
      "to get the full one-liner, then paste it here. It looks like:" \
      "  curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/join.sh | \\" \
      "    ROOST_COORDINATOR_URL=\"https://roost.example.com\" \\" \
      "    ROOST_BOOTSTRAP_TOKEN=\"roost_bt_…\" bash"
fi

# 2. Bun.
if ! command -v bun >/dev/null 2>&1; then
  say "installing Bun"
  curl -fsSL https://bun.sh/install | bash
  export PATH="$HOME/.bun/bin:$PATH"
fi
command -v bun >/dev/null 2>&1 || die "Bun install did not land on PATH." "Open a new shell and re-run, or add ~/.bun/bin to PATH."

# 3. Source — clone or fetch (do NOT pull yet; step 4 pins the checkout).
if [ -d "$ROOST_DIR/.git" ]; then
  say "fetching $ROOST_DIR"
  git -C "$ROOST_DIR" fetch --quiet origin
else
  say "cloning $REPO_URL → $ROOST_DIR"
  git clone "$REPO_URL" "$ROOST_DIR"
fi

# 4. Version pin (load-bearing) — stamp the SAME commit the coord runs, else
# the drift badge fires on this fresh Mac. Fetch the coord's live HEAD from
# the PUBLIC MiscHealth RPC (no auth) and detach-checkout it. Best-effort: on
# a 'dev'/unpushed/unreachable SHA, stay on main and warn (same semantics as
# today's deploy).
COORD_SHA="$(curl -fsS -m 8 -X POST -H 'Content-Type: application/json' -d '{}' \
  "${ROOST_COORDINATOR_URL%/}/roost.v1.CoordinatorService/MiscHealth" 2>/dev/null \
  | grep -o '"gitSha":[[:space:]]*"[^"]*"' | head -1 | sed 's/.*"\([^"]*\)"$/\1/' || true)"
if [ -n "$COORD_SHA" ] && [ "$COORD_SHA" != "dev" ] \
   && git -C "$ROOST_DIR" cat-file -e "${COORD_SHA}^{commit}" 2>/dev/null; then
  say "pinning to coord commit ${COORD_SHA}"
  git -C "$ROOST_DIR" checkout --quiet --detach "$COORD_SHA"
else
  say "WARN: could not pin coord commit (got '${COORD_SHA:-none}') — staying on main; drift badge may show until coord is on a pushed commit"
  git -C "$ROOST_DIR" checkout --quiet main && git -C "$ROOST_DIR" pull --ff-only --quiet || true
fi

# 5. Install deps (no native deps → fast, no codesign/quarantine repairs).
cd "$ROOST_DIR"
say "bun install"
bun install

# 6. Install + register the local worker. ROOST_COORDINATOR_URL /
# ROOST_BOOTSTRAP_TOKEN / ROOST_WORKER_LABEL are already in the env and inherited.
say "roost join"
exec bun apps/roost-cli/src/main.ts join
