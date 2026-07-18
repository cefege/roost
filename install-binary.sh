#!/usr/bin/env bash
# Roost binary installer — download the single `roost` binary (no repo, no Bun)
# and put it on PATH, then point you at `roost quickstart`:
#   curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install-binary.sh | bash
#
# Dry-run against a local build (no download): ROOST_LOCAL_BIN=dist/roost bash install-binary.sh
set -euo pipefail

REPO="cefege/roost"
BIN_DIR="${ROOST_BIN_DIR:-$HOME/.local/bin}"

say() { printf '>> %s\n' "$1"; }
die() { printf 'ERROR: %s\n' "$1" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || die "Roost is macOS only (found $(uname -s))."

# Tailscale is walked through interactively by `roost quickstart`; just a nudge.
command -v tailscale >/dev/null 2>&1 || say "WARN: Tailscale not installed — quickstart will guide you (brew install tailscale)."

mkdir -p "$BIN_DIR"
DEST="$BIN_DIR/roost"
if [ -n "${ROOST_LOCAL_BIN:-}" ]; then
  say "installing local binary $ROOST_LOCAL_BIN → $DEST"
  cp "$ROOST_LOCAL_BIN" "$DEST"
else
  say "downloading roost → $DEST"
  curl -fsSL "https://github.com/$REPO/releases/latest/download/roost" -o "$DEST"
fi
chmod +x "$DEST"

say "installed roost $("$DEST" version)"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) say "NOTE: $BIN_DIR is not on your PATH — add it, or run $DEST directly." ;;
esac
say "next: roost quickstart"
