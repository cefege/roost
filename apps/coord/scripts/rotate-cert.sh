#!/usr/bin/env bash
# Rotate the tailscale TLS cert for the coord service (launchd on macOS,
# systemd --user on Linux).
# Tailscale certs expire after 90 days; this regenerates + reloads.
# Schedule via launchd (per-user agent), cron:
#   0 4 1,15 * *  $REPO/apps/coord/scripts/rotate-cert.sh >> $LOG_DIR/cert-rotate.log 2>&1
# or a systemd --user timer (roost-cert-rotate.timer, OnCalendar=*-*-01,15 04:00).
# Twice-monthly is conservative; tailscale cert is idempotent so cheap to run.
#
# On Linux `tailscale cert` writes through tailscaled and needs root or an
# operator grant (`sudo tailscale set --operator=$USER`); the worker installer
# already grants this best-effort.

set -euo pipefail

OS="$(uname -s)"
IS_LINUX=false
if [[ "$OS" == "Linux" ]]; then
  IS_LINUX=true
  TLS_DIR="${ROOST_COORDINATOR_TLS_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/RoostCoordinatorV2/tls}"
  LABEL="${ROOST_COORD_LABEL:-roost-coord}"
else
  TLS_DIR="${ROOST_COORDINATOR_TLS_DIR:-$HOME/Library/Application Support/RoostCoordinatorV2/tls}"
  LABEL="${ROOST_COORD_LABEL:-com.roost.coordinator-v2}"
fi
# FQDN: env override, else this box's tailnet DNSName (no hardcoded tailnet).
FQDN="${ROOST_COORDINATOR_FQDN:-$(tailscale status --json 2>/dev/null | grep -o '"DNSName"[^,]*' | head -1 | sed 's/.*: *"//;s/\.\{0,1\}".*//')}"
if [[ -z "$FQDN" ]]; then
  echo "ERROR: cannot resolve coord FQDN — set ROOST_COORDINATOR_FQDN or start tailscale" >&2
  exit 1
fi

mkdir -p "$TLS_DIR"
cd "$TLS_DIR"

# Skip if cert still has > 14 days validity. BSD `date -j -f` and GNU `date -d`
# parse the openssl notAfter string differently; picking the wrong one silently
# yields epoch 0 via the `|| echo 0` fallback, which re-mints on EVERY run.
CERT="$TLS_DIR/${FQDN}.crt"
if [[ -f "$CERT" ]]; then
  not_after=$(openssl x509 -in "$CERT" -noout -enddate | sed 's/notAfter=//')
  if $IS_LINUX; then
    expiry_epoch=$(date -d "$not_after" "+%s" 2>/dev/null || echo 0)
  else
    expiry_epoch=$(date -j -f "%b %e %T %Y %Z" "$not_after" "+%s" 2>/dev/null || echo 0)
  fi
  now_epoch=$(date "+%s")
  days_left=$(( (expiry_epoch - now_epoch) / 86400 ))
  if [[ $days_left -gt 14 ]]; then
    echo "$(date -u +%FT%TZ) cert valid ${days_left}d; skip"
    exit 0
  fi
  echo "$(date -u +%FT%TZ) cert expires in ${days_left}d; rotating"
fi

# Mint fresh cert. No-sudo on macOS GUI Tailscale; needs the operator grant on Linux.
if $IS_LINUX; then
  tailscale cert "$FQDN" 2>&1 | tail -3
else
  PATH="/opt/homebrew/bin:/usr/local/bin:$PATH" tailscale cert "$FQDN" 2>&1 | tail -3
fi

# Kick the coord service so it re-reads cert/key.
if $IS_LINUX; then
  export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
  systemctl --user restart "${LABEL}.service" 2>&1 | head -3 || true
else
  launchctl kickstart -k "gui/$(id -u)/${LABEL}" 2>&1 | head -3 || true
fi

echo "$(date -u +%FT%TZ) rotation complete"
