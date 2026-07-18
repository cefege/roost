#!/usr/bin/env bash
# Rotate the tailscale TLS cert for the coord LaunchAgent.
# Tailscale certs expire after 90 days; this regenerates + reloads.
# Schedule via launchd (per-user agent) or cron:
#   0 4 1,15 * *  $REPO/apps/coord/scripts/rotate-cert.sh >> $LOG_DIR/cert-rotate.log 2>&1
# Twice-monthly is conservative; tailscale cert is idempotent so cheap to run.

set -euo pipefail

TLS_DIR="${ROOST_COORDINATOR_TLS_DIR:-$HOME/Library/Application Support/RoostCoordinatorV2/tls}"
# FQDN: env override, else this box's tailnet DNSName (no hardcoded tailnet).
FQDN="${ROOST_COORDINATOR_FQDN:-$(tailscale status --json 2>/dev/null | grep -o '"DNSName"[^,]*' | head -1 | sed 's/.*: *"//;s/\.\{0,1\}".*//')}"
if [[ -z "$FQDN" ]]; then
  echo "ERROR: cannot resolve coord FQDN — set ROOST_COORDINATOR_FQDN or start tailscale" >&2
  exit 1
fi
LABEL="${ROOST_COORDINATOR_LABEL:-com.roost.coordinator-v2}"

mkdir -p "$TLS_DIR"
cd "$TLS_DIR"

# Skip if cert still has > 14 days validity.
CERT="$TLS_DIR/${FQDN}.crt"
if [[ -f "$CERT" ]]; then
  expiry_epoch=$(openssl x509 -in "$CERT" -noout -enddate | sed 's/notAfter=//' | xargs -I{} date -j -f "%b %e %T %Y %Z" "{}" "+%s" 2>/dev/null || echo 0)
  now_epoch=$(date "+%s")
  days_left=$(( (expiry_epoch - now_epoch) / 86400 ))
  if [[ $days_left -gt 14 ]]; then
    echo "$(date -u +%FT%TZ) cert valid ${days_left}d; skip"
    exit 0
  fi
  echo "$(date -u +%FT%TZ) cert expires in ${days_left}d; rotating"
fi

# Mint fresh cert. tailscale cert is no-sudo on macOS GUI Tailscale.
PATH="/opt/homebrew/bin:/usr/local/bin:$PATH" tailscale cert "$FQDN" 2>&1 | tail -3

# Kick the coord LaunchAgent so it re-reads cert/key.
launchctl kickstart -k "gui/$(id -u)/${LABEL}" 2>&1 | head -3 || true

echo "$(date -u +%FT%TZ) rotation complete"
