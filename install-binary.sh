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
# Darwin arm64 `roost` asset stays unsuffixed for back-compat with existing
# release links. Change both together or the installer 404s.
OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS/$ARCH" in
  Darwin/arm64)  ASSET="roost" ;;
  Darwin/x86_64) ASSET="roost-darwin-x64" ;;
  Linux/x86_64)  ASSET="roost-linux-x64" ;;
  Linux/aarch64) ASSET="roost-linux-arm64" ;;
  *) die "no prebuilt roost binary for $OS/$ARCH — install from source: https://github.com/$REPO" ;;
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
TMP_DIR="$(mktemp -d "$BIN_DIR/.roost-install.XXXXXX")"
chmod 0700 "$TMP_DIR"
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT HUP INT TERM
CANDIDATE="$TMP_DIR/roost"

if [ -n "${ROOST_LOCAL_BIN:-}" ]; then
  say "installing local binary $ROOST_LOCAL_BIN → $DEST"
  cp "$ROOST_LOCAL_BIN" "$CANDIDATE"
else
  say "downloading $ASSET → $DEST"
  DIGEST_FILE="$TMP_DIR/$ASSET.sha256"
  curl -fsSL "https://github.com/$REPO/releases/latest/download/$ASSET" -o "$CANDIDATE"
  curl -fsSL "https://github.com/$REPO/releases/latest/download/$ASSET.sha256" -o "$DIGEST_FILE"

  DIGEST_CONTENT="$(< "$DIGEST_FILE")"
  if [[ ! "$DIGEST_CONTENT" =~ ^([0-9a-f]{64})[[:space:]]*$ ]]; then
    die "invalid checksum file for $ASSET"
  fi
  EXPECTED_SHA256="${BASH_REMATCH[1]}"
  if [ "$OS" = "Darwin" ]; then
    ACTUAL_OUTPUT="$(shasum -a 256 "$CANDIDATE")"
  else
    ACTUAL_OUTPUT="$(sha256sum "$CANDIDATE")"
  fi
  ACTUAL_SHA256="${ACTUAL_OUTPUT%%[[:space:]]*}"
  if [ "$ACTUAL_SHA256" != "$EXPECTED_SHA256" ]; then
    die "checksum mismatch for $ASSET"
  fi
fi

chmod 0755 "$CANDIDATE"
mv -f "$CANDIDATE" "$DEST"
trap - EXIT HUP INT TERM
cleanup

say "installed roost $("$DEST" version)"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) say "NOTE: $BIN_DIR is not on your PATH — add it, or run $DEST directly." ;;
esac
say "next: roost quickstart"
