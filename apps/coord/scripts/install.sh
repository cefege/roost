#!/usr/bin/env bash
# install / uninstall the v2 coord LaunchAgent. Binds :4102 (new port;
# legacy stays at :4101 until R4.5 cutover). Runs `bun apps/coord/src/main.ts`
# directly — no bundle step required (Bun runs .ts natively).
# REWRITE.md R0.13.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
# Load YOUR setup from repo-root .env.local (gitignored) — one place for ROOST_*.
set -a; [ -f "$REPO_ROOT/.env.local" ] && source "$REPO_ROOT/.env.local"; set +a
# Labels/paths overridable (binary-mode quickstart + isolated test installs);
# defaults are the daily-driver source install — unchanged when unset.
LABEL="${ROOST_COORD_LABEL:-com.roost.coordinator-v2}"
PLIST="${ROOST_COORD_PLIST:-$HOME/Library/LaunchAgents/${LABEL}.plist}"
DATA_DIR="${ROOST_COORD_DATA_DIR:-$HOME/Library/Application Support/RoostCoordinatorV2}"
DB_PATH="$DATA_DIR/coordinator_v2.db"
AUTH_KEYS="$DATA_DIR/authorized_keys.roost"
COORD_KEY="$DATA_DIR/ssh_ed25519.key"
LOG_DIR="${ROOST_COORD_LOG_DIR:-$HOME/Library/Logs/RoostCoord}"
BUN_BIN="${BUN_BIN:-$(command -v bun || echo /opt/homebrew/bin/bun)}"

# FRONTED mode (DEFAULT): coord serves PLAINTEXT on loopback behind
# `tailscale serve`, which terminates TLS with the tailnet cert. This dodges
# the Bun 1.3.14 segfault in us_internal_ssl_on_close / RequestContext.onAbort
# that fires when a browser aborts a long-lived streaming TLS response (the
# Sync firehose) — Bun never runs the TLS close path, so the coord stops
# crash-looping. ROOST_FRONTED=0 reverts to direct Bun TLS (the old path).
FRONTED="${ROOST_FRONTED:-1}"
COORD_LOOPBACK_PORT="${ROOST_COORD_LOOPBACK_PORT:-4103}"
TAILNET_HTTPS_PORT="${ROOST_TAILNET_HTTPS_PORT:-4102}"

# Optional TLS via `tailscale cert <fqdn>`. When both are set, coord serves
# HTTPS instead of HTTP — required for tailnet daily-driver (browsers need
# secure-context for WebCrypto on non-localhost origins).
TLS_CERT_PATH="${ROOST_TLS_CERT_PATH:-}"
TLS_KEY_PATH="${ROOST_TLS_KEY_PATH:-}"
# Auto-detect from $DATA_DIR/tls/ when env vars unset. Cert files persist
# across hostname renames (a `tailscale cert <new-fqdn>` adds a new pair
# without deleting the old one); pick the most recently modified .crt and
# its sibling .key. Eliminates the "reinstall silently dropped TLS" failure
# mode where coord came up plaintext on :4102 and rejected every https
# client until env vars were re-exported.
if [[ "$FRONTED" != "1" && ( -z "$TLS_CERT_PATH" || -z "$TLS_KEY_PATH" ) ]]; then
  TLS_DIR="$HOME/Library/Application Support/RoostCoordinatorV2/tls"
  if [[ -d "$TLS_DIR" ]]; then
    # ls -t newest first; null-safe via `|| true` (set -e otherwise aborts on empty dir)
    AUTO_CRT="$(ls -t "$TLS_DIR"/*.crt 2>/dev/null | head -1 || true)"
    if [[ -n "$AUTO_CRT" ]]; then
      AUTO_KEY="${AUTO_CRT%.crt}.key"
      if [[ -f "$AUTO_KEY" ]]; then
        TLS_CERT_PATH="$AUTO_CRT"
        TLS_KEY_PATH="$AUTO_KEY"
        echo "auto-detected TLS: $TLS_CERT_PATH"
      fi
    fi
  fi
fi
TLS_PLIST=""
if [[ "$FRONTED" != "1" && -n "$TLS_CERT_PATH" && -n "$TLS_KEY_PATH" ]]; then
  # Expand leading tilde — plist values aren't shell-expanded at read time.
  TLS_CERT_PATH="${TLS_CERT_PATH/#\~/$HOME}"
  TLS_KEY_PATH="${TLS_KEY_PATH/#\~/$HOME}"
  TLS_PLIST=$'\n    <key>ROOST_TLS_CERT_PATH</key>\n    <string>'"${TLS_CERT_PATH}"$'</string>\n    <key>ROOST_TLS_KEY_PATH</key>\n    <string>'"${TLS_KEY_PATH}"$'</string>'
fi

# Stamp the current repo HEAD into coord's env so misc.health.git_sha
# returns a real SHA instead of "dev". SPA's MachineSection compares
# each worker's last-reported git_sha to this value and flags stale
# workers.
GIT_SHA_PLIST=""
GIT_SHA_RESOLVED="${ROOST_GIT_SHA:-$(cd "$REPO_ROOT" && git rev-parse HEAD 2>/dev/null || true)}"
if [[ -n "$GIT_SHA_RESOLVED" ]]; then
  GIT_SHA_PLIST=$'\n    <key>ROOST_GIT_SHA</key>\n    <string>'"${GIT_SHA_RESOLVED}"$'</string>'
fi

# Bind + trust-proxy depend on FRONTED. Fronted: plaintext on loopback, trust
# X-Forwarded-For from `tailscale serve` (verified un-spoofable: tailscale
# OVERWRITES client XFF with the authenticated tailnet IP). Direct: 0.0.0.0 TLS.
if [[ "$FRONTED" == "1" ]]; then
  BIND_VALUE="127.0.0.1:${COORD_LOOPBACK_PORT}"
  TRUST_PROXY_PLIST=$'\n    <key>ROOST_TRUST_PROXY</key>\n    <string>1</string>'
else
  BIND_VALUE="0.0.0.0:${TAILNET_HTTPS_PORT}"
  TRUST_PROXY_PLIST=""
fi

cmd="${1:-status}"

write_plist() {
  mkdir -p "$DATA_DIR" "$LOG_DIR"
  # ProgramArguments/workdir/dist switch by mode: ROOST_EXEC_BIN set → compiled
  # binary (`roost coord`); unset → from-source (`bun …/main.ts`). Only these
  # differ between modes; the safety-critical env below (BIND, TRUST_PROXY,
  # TLS) is computed identically for both.
  local prog_bin prog_arg2 workdir web_dist
  if [[ -n "${ROOST_EXEC_BIN:-}" ]]; then
    prog_bin="${ROOST_EXEC_BIN}"; prog_arg2="coord"
  else
    prog_bin="${BUN_BIN}"; prog_arg2="${REPO_ROOT}/apps/coord/src/main.ts"
  fi
  workdir="${ROOST_WORKDIR:-$REPO_ROOT}"
  web_dist="${ROOST_WEB_DIST_PATH:-$REPO_ROOT/apps/web/dist}"
  cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>${LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>${prog_bin}</string>
    <string>${prog_arg2}</string>
  </array>
  <key>WorkingDirectory</key>
  <string>${workdir}</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HOME</key>
    <string>${HOME}</string>
    <key>PATH</key>
    <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
    <key>ROOST_COORDINATOR_BIND</key>
    <string>${BIND_VALUE}</string>
    <key>ROOST_COORDINATOR_DB</key>
    <string>${DB_PATH}</string>
    <key>ROOST_COORDINATOR_AUTHORIZED_KEYS</key>
    <string>${AUTH_KEYS}</string>
    <key>ROOST_COORDINATOR_KEY_PATH</key>
    <string>${COORD_KEY}</string>
    <key>ROOST_WEB_DIST_PATH</key>
    <string>${web_dist}</string>
    <key>ROOST_DIAG</key>
    <string>\${ROOST_DIAG:-0}</string>${TLS_PLIST}${GIT_SHA_PLIST}${TRUST_PROXY_PLIST}
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <!-- Segfault recovery: launchd default throttle is 10s, so a Bun crash
       freezes every browser's Sync stream for ~10s. 1s shrinks the outage
       to a reconnect blip the seq-resume path absorbs. -->
  <key>ThrottleInterval</key>
  <integer>1</integer>
  <key>ProcessType</key>
  <string>Interactive</string>
  <key>StandardOutPath</key>
  <string>${LOG_DIR}/main.out.log</string>
  <key>StandardErrorPath</key>
  <string>${LOG_DIR}/main.err.log</string>
  <!-- Log rotation: launchd does not support native log rotation with date
       placeholders. Rotate via newsyslog.d:
         sudo tee /etc/newsyslog.d/roost-coord.conf <<'CONF'
         ${LOG_DIR}/main.out.log  ${USER}:staff  640  5  100000  *  GZ
         ${LOG_DIR}/main.err.log  ${USER}:staff  640  5  100000  *  GZ
         CONF
       Then: sudo newsyslog -vf /etc/newsyslog.d/roost-coord.conf
       Fields: path  owner:group  mode  keep  size(kB)  when  flags
       size=100000 = 100 MB; keep=5 generations; GZ=compress.
       Alternatively truncate in place: > "${LOG_DIR}/main.err.log" -->
</dict>
</plist>
EOF
  chmod 0644 "$PLIST"
  echo "wrote $PLIST"
}

bootstrap() {
  launchctl bootout "gui/$UID/${LABEL}" 2>/dev/null || true
  # bootout is asynchronous; immediate bootstrap can race and fail with
  # "Input/output error (5)". Retry up to 3 times with a 1s pause.
  local i
  for i in 1 2 3; do
    sleep 1
    if launchctl bootstrap "gui/$UID" "$PLIST" 2>/dev/null; then
      break
    fi
    if [[ $i -eq 3 ]]; then
      echo "launchctl bootstrap failed after 3 retries" >&2
      exit 1
    fi
  done
  launchctl enable "gui/$UID/${LABEL}"
  launchctl kickstart -k "gui/$UID/${LABEL}" 2>/dev/null || true
  sleep 1
  echo "bootstrapped gui/$UID/${LABEL}"
}

case "$cmd" in
  install)
    write_plist
    bootstrap
    if [[ "$FRONTED" == "1" ]]; then
      echo ">> tailscale serve --https=${TAILNET_HTTPS_PORT} → http://127.0.0.1:${COORD_LOOPBACK_PORT} (TLS off Bun)"
      if tailscale serve --bg --https="${TAILNET_HTTPS_PORT}" "http://127.0.0.1:${COORD_LOOPBACK_PORT}"; then
        echo "   tailscale serve configured (persists in tailscaled state)"
      else
        echo "   WARN: tailscale serve failed — coord is reachable on loopback :${COORD_LOOPBACK_PORT} only" >&2
        echo "   run manually: tailscale serve --bg --https=${TAILNET_HTTPS_PORT} http://127.0.0.1:${COORD_LOOPBACK_PORT}" >&2
      fi
    fi
    echo
    echo "Coord v2 starting (FRONTED=${FRONTED}) — tailnet https://<host>:${TAILNET_HTTPS_PORT}. Logs:"
    echo "  bun apps/roost-cli/src/main.ts logs coord"
    ;;
  write-plist)
    write_plist
    ;;
  uninstall)
    launchctl bootout "gui/$UID/${LABEL}" 2>/dev/null || true
    rm -f "$PLIST"
    echo "removed ${PLIST}"
    echo "(DB + keys left at ${DATA_DIR})"
    ;;
  reinstall)
    "$0" uninstall || true
    "$0" install
    ;;
  status)
    launchctl print "gui/$UID/${LABEL}" 2>&1 | head -8 || echo "not loaded"
    ;;
  *)
    echo "usage: $0 {install|uninstall|reinstall|status}"
    exit 1
    ;;
esac
