#!/usr/bin/env bash
# Roost binary installer — download the single `roost` binary (no repo, no Bun)
# and put it on PATH, then point you at `roost quickstart`. macOS or Linux:
#   curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install-binary.sh | bash
#
# Dry-run against a local build (no download): ROOST_LOCAL_BIN=dist/roost bash install-binary.sh
set -euo pipefail

REPO="cefege/roost"
BIN_DIR="${ROOST_BIN_DIR:-$HOME/.local/bin}"

say() { printf '>> %s\n' "$1"; }
die() { printf 'ERROR: %s\n' "$1" >&2; exit 1; }

# Asset names mirror releaseAssetName() in apps/roost-cli/src/update.ts — the
# `roost` (darwin-arm64) arm stays unsuffixed for back-compat with existing
# release links. Change both together or the installer 404s.
case "$(uname -s)/$(uname -m)" in
  Darwin/*)      ASSET="roost" ;;
  Linux/x86_64)  ASSET="roost-linux-x64" ;;
  Linux/aarch64) ASSET="roost-linux-arm64" ;;
  *) die "no prebuilt roost binary for $(uname -s)/$(uname -m) — install from source: https://github.com/$REPO" ;;
esac

# Tailscale is walked through interactively by `roost quickstart`; just a nudge.
if ! command -v tailscale >/dev/null 2>&1; then
  if [ "$(uname -s)" = "Darwin" ]; then
    say "WARN: Tailscale not installed — quickstart will guide you (brew install tailscale)."
  else
    say "WARN: Tailscale not installed — quickstart will guide you (https://tailscale.com/download/linux)."
  fi
fi

mkdir -p "$BIN_DIR"
DEST="$BIN_DIR/roost"
if [ -n "${ROOST_LOCAL_BIN:-}" ]; then
  say "installing local binary $ROOST_LOCAL_BIN → $DEST"
  cp "$ROOST_LOCAL_BIN" "$DEST"
else
  say "downloading $ASSET → $DEST"
  curl -fsSL "https://github.com/$REPO/releases/latest/download/$ASSET" -o "$DEST"
fi
chmod +x "$DEST"

say "installed roost $("$DEST" version)"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) say "NOTE: $BIN_DIR is not on your PATH — add it, or run $DEST directly." ;;
esac
say "next: roost quickstart"
