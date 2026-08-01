#!/usr/bin/env bash
# install / uninstall the v2 coord service. macOS → launchd LaunchAgent;
# Linux → systemd --user unit. Binds :4102 (new port; legacy stays at :4101
# until R4.5 cutover). Runs `bun apps/coord/src/main.ts` directly — no bundle
# step required (Bun runs .ts natively).
# REWRITE.md R0.13.

set -euo pipefail

REPO_ROOT="${ROOST_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)}"
# Load YOUR setup from repo-root .env.local (gitignored) — one place for ROOST_*.
# CoordTarget sets ROOST_SKIP_ENV_LOCAL=1: on a relocation the deployed repo's
# .env.local carries the SOURCE coordinator's public URL, and sourcing it would
# make the new coordinator advertise the machine it just moved off.
if [[ -z "${ROOST_SKIP_ENV_LOCAL:-}" ]]; then
  set -a; [ -f "$REPO_ROOT/.env.local" ] && source "$REPO_ROOT/.env.local"; set +a
fi
# Labels/paths overridable (binary-mode quickstart + isolated test installs);
# defaults are the daily-driver source install — unchanged when unset.
OS="$(uname -s)"
IS_LINUX=false
if [[ "$OS" == "Linux" ]]; then
  IS_LINUX=true
  LABEL="${ROOST_COORD_LABEL:-roost-coord}"
  UNIT="${ROOST_COORD_UNIT:-$HOME/.config/systemd/user/${LABEL}.service}"
  DATA_DIR="${ROOST_COORD_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/RoostCoordinatorV2}"
  LOG_DIR="${ROOST_COORD_LOG_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/RoostCoord}"
  # Resource limits baked into the unit; dial down on a small box via env.
  COORD_MEM_HIGH="${ROOST_COORD_MEMORY_HIGH:-1G}"
  COORD_MEM_MAX="${ROOST_COORD_MEMORY_MAX:-2G}"
  COORD_TASKS_MAX="${ROOST_COORD_TASKS_MAX:-256}"
  LOGROTATE_CONF="${ROOST_COORD_LOGROTATE_CONF:-${XDG_CONFIG_HOME:-$HOME/.config}/logrotate.d/roost-coord.conf}"
else
  LABEL="${ROOST_COORD_LABEL:-com.roost.coordinator-v2}"
  PLIST="${ROOST_COORD_PLIST:-$HOME/Library/LaunchAgents/${LABEL}.plist}"
  DATA_DIR="${ROOST_COORD_DATA_DIR:-$HOME/Library/Application Support/RoostCoordinatorV2}"
  LOG_DIR="${ROOST_COORD_LOG_DIR:-$HOME/Library/Logs/RoostCoord}"
fi
DB_PATH="${ROOST_COORDINATOR_DB:-$DATA_DIR/coordinator_v2.db}"
AUTH_KEYS="${ROOST_COORDINATOR_AUTHORIZED_KEYS:-$DATA_DIR/authorized_keys.roost}"
COORD_KEY="${ROOST_COORDINATOR_KEY_PATH:-$DATA_DIR/ssh_ed25519.key}"
HANDOFF_PATH="${ROOST_COORDINATOR_HANDOFF_PATH:-$DATA_DIR/coord-handoff.json}"
PUBLIC_URL="${ROOST_COORDINATOR_PUBLIC_URL:-}"
# Resolve bun the same way the worker installer does: explicit override,
# `command -v`, then a fallback list including ~/.bun/bin so a tarball install
# on a box without Homebrew (every Linux box) also works.
_find_bin() {
  local name="$1"; shift
  local v
  v=$(command -v "$name" 2>/dev/null) || true
  if [ -n "$v" ] && [ -x "$v" ]; then echo "$v"; return 0; fi
  for p in "$@"; do
    if [ -x "$p" ]; then echo "$p"; return 0; fi
  done
  echo "/opt/homebrew/bin/$name"
}
BUN_BIN="${BUN_BIN:-$(_find_bin bun /usr/local/bin/bun "$HOME/.bun/bin/bun")}"

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
  TLS_DIR="$DATA_DIR/tls"
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
    <key>ROOST_COORDINATOR_HANDOFF_PATH</key>
    <string>${HANDOFF_PATH}</string>
    <key>ROOST_COORDINATOR_PUBLIC_URL</key>
    <string>${PUBLIC_URL}</string>
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

# systemd --user counterpart of write_plist: same env-key set, same computed
# BIND/TRUST_PROXY/TLS values. No KillMode=process — that exists in the worker
# unit for the detached keeper, and coord has no such child.
#
# The resource limits were hand-written drop-ins on the first Linux box until
# they landed here; a reinstall used to silently discard them. Coord owns no
# session subtree, so a hard MemoryMax has no blast radius (unlike the worker,
# where one fat PTY would take every live session down with the unit).
write_unit() {
  mkdir -p "$(dirname "$UNIT")" "$DATA_DIR" "$LOG_DIR"
  local prog_bin prog_arg2 workdir web_dist
  if [[ -n "${ROOST_EXEC_BIN:-}" ]]; then
    prog_bin="${ROOST_EXEC_BIN}"; prog_arg2="coord"
  else
    prog_bin="${BUN_BIN}"; prog_arg2="${REPO_ROOT}/apps/coord/src/main.ts"
  fi
  workdir="${ROOST_WORKDIR:-$REPO_ROOT}"
  web_dist="${ROOST_WEB_DIST_PATH:-$REPO_ROOT/apps/web/dist}"
  {
    cat <<EOF
[Unit]
Description=Roost coordinator
After=network-online.target

[Service]
Type=simple
WorkingDirectory=${workdir}
ExecStart=${prog_bin} ${prog_arg2}
Environment=HOME=${HOME}
Environment=PATH=${HOME}/.bun/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
Environment=ROOST_COORDINATOR_BIND=${BIND_VALUE}
Environment=ROOST_COORDINATOR_DB=${DB_PATH}
Environment=ROOST_COORDINATOR_AUTHORIZED_KEYS=${AUTH_KEYS}
Environment=ROOST_COORDINATOR_KEY_PATH=${COORD_KEY}
Environment=ROOST_COORDINATOR_HANDOFF_PATH=${HANDOFF_PATH}
Environment=ROOST_COORDINATOR_PUBLIC_URL=${PUBLIC_URL}
Environment=ROOST_COORD_DATA_DIR=${DATA_DIR}
Environment=ROOST_COORD_LOG_DIR=${LOG_DIR}
Environment=ROOST_WEB_DIST_PATH=${web_dist}
Environment=ROOST_DIAG=${ROOST_DIAG:-0}
EOF
    if [[ "$FRONTED" != "1" && -n "$TLS_CERT_PATH" && -n "$TLS_KEY_PATH" ]]; then
      echo "Environment=ROOST_TLS_CERT_PATH=${TLS_CERT_PATH}"
      echo "Environment=ROOST_TLS_KEY_PATH=${TLS_KEY_PATH}"
    fi
    [[ -n "$GIT_SHA_RESOLVED" ]]      && echo "Environment=ROOST_GIT_SHA=${GIT_SHA_RESOLVED}"
    [[ "$FRONTED" == "1" ]]           && echo "Environment=ROOST_TRUST_PROXY=1"
    [[ -n "${ROOST_EXEC_BIN:-}" ]]    && echo "Environment=ROOST_EXEC_BIN=${ROOST_EXEC_BIN}"
    # RestartSec=1 is the systemd analogue of the plist's ThrottleInterval 1:
    # a Bun crash must not freeze every browser's Sync stream for 10s.
    cat <<EOF
Restart=always
RestartSec=1
TimeoutStopSec=10
MemoryHigh=${COORD_MEM_HIGH}
MemoryMax=${COORD_MEM_MAX}
TasksMax=${COORD_TASKS_MAX}
StandardOutput=append:${LOG_DIR}/main.out.log
StandardError=append:${LOG_DIR}/main.err.log

[Install]
WantedBy=default.target
EOF
  } > "$UNIT"
  chmod 0600 "$UNIT"
  echo "wrote $UNIT"
}

# systemd's StandardOutput=append: holds the fd open, so rotation MUST be
# copytruncate — a rename-based rotate would leave the service writing to an
# unlinked inode. The .1.gz naming this produces is what doctor.ts already
# parses. A *user* logrotate config is not run by the system timer, so the
# matching user timer is installed beside it.
write_logrotate() {
  local rotate_bin conf_dir unit_dir
  rotate_bin="$(command -v logrotate || true)"
  [[ -z "$rotate_bin" && -x /usr/sbin/logrotate ]] && rotate_bin=/usr/sbin/logrotate
  if [[ -z "$rotate_bin" ]]; then
    echo "WARN: logrotate not found - coord logs in ${LOG_DIR} will grow unbounded" >&2
    return 0
  fi
  conf_dir="$(dirname "$LOGROTATE_CONF")"
  mkdir -p "$conf_dir"
  cat > "$LOGROTATE_CONF" <<EOF
${LOG_DIR}/main.out.log ${LOG_DIR}/main.err.log {
    size 100M
    rotate 5
    compress
    missingok
    notifempty
    copytruncate
}
EOF
  echo "wrote $LOGROTATE_CONF"

  # logrotate reads every file in a directory argument, so coord and worker
  # each own one conf and share one timer.
  unit_dir="$(dirname "$UNIT")"
  mkdir -p "$unit_dir" "${XDG_STATE_HOME:-$HOME/.local/state}/roost"
  cat > "$unit_dir/roost-logrotate.service" <<EOF
[Unit]
Description=Rotate Roost logs

[Service]
Type=oneshot
ExecStart=${rotate_bin} --state ${XDG_STATE_HOME:-$HOME/.local/state}/roost/logrotate.status ${conf_dir}
EOF
  cat > "$unit_dir/roost-logrotate.timer" <<EOF
[Unit]
Description=Rotate Roost logs daily

[Timer]
OnCalendar=daily
Persistent=true

[Install]
WantedBy=timers.target
EOF
  echo "wrote $unit_dir/roost-logrotate.timer"
}

bootstrap_systemd() {
  export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
  # Linger is mandatory: without it the user manager exits at logout and takes
  # the coordinator with it.
  loginctl enable-linger "$USER" 2>/dev/null || sudo -n loginctl enable-linger "$USER" 2>/dev/null || \
    echo "WARN: could not enable linger - the coordinator will stop when you log out" >&2
  systemctl --user daemon-reload
  systemctl --user enable --now "${LABEL}.service"
  systemctl --user restart "${LABEL}.service"
  echo "started ${LABEL}.service"
  if [[ -f "$(dirname "$UNIT")/roost-logrotate.timer" ]]; then
    systemctl --user enable --now roost-logrotate.timer 2>/dev/null || \
      echo "WARN: could not enable roost-logrotate.timer - logs will grow unbounded" >&2
  fi
}

# Unchanged across platforms: a bare-`tailscale` PATH lookup that works on Linux.
serve_front() {
  if [[ "$FRONTED" == "1" ]]; then
    echo ">> tailscale serve --https=${TAILNET_HTTPS_PORT} -> http://127.0.0.1:${COORD_LOOPBACK_PORT} (TLS off Bun)"
    if tailscale serve --bg --https="${TAILNET_HTTPS_PORT}" "http://127.0.0.1:${COORD_LOOPBACK_PORT}"; then
      echo "   tailscale serve configured (persists in tailscaled state)"
    else
      echo "   WARN: tailscale serve failed - coord is reachable on loopback :${COORD_LOOPBACK_PORT} only" >&2
      echo "   run manually: tailscale serve --bg --https=${TAILNET_HTTPS_PORT} http://127.0.0.1:${COORD_LOOPBACK_PORT}" >&2
    fi
  fi
}

case "$cmd" in
  install)
    if $IS_LINUX; then write_unit; write_logrotate; bootstrap_systemd; else write_plist; bootstrap; fi
    serve_front
    echo
    echo "Coord v2 starting (FRONTED=${FRONTED}) - tailnet https://<host>:${TAILNET_HTTPS_PORT}. Logs:"
    echo "  bun apps/roost-cli/src/main.ts logs coord"
    ;;
  # The verb name stays `write-plist` on both platforms: CoordTarget invokes it
  # by that literal, exactly as the worker installer already does.
  write-plist)
    if $IS_LINUX; then write_unit; else write_plist; fi
    ;;
  uninstall)
    if $IS_LINUX; then
      export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
      systemctl --user disable --now "${LABEL}.service" 2>/dev/null || true
      rm -f "$UNIT"
      systemctl --user daemon-reload 2>/dev/null || true
      echo "removed ${UNIT}"
    else
      launchctl bootout "gui/$UID/${LABEL}" 2>/dev/null || true
      rm -f "$PLIST"
      echo "removed ${PLIST}"
    fi
    echo "(DB + keys left at ${DATA_DIR})"
    ;;
  reinstall)
    "$0" uninstall || true
    "$0" install
    ;;
  status)
    if $IS_LINUX; then
      export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
      systemctl --user status "${LABEL}.service" --no-pager 2>&1 | head -8 || echo "not loaded"
    else
      launchctl print "gui/$UID/${LABEL}" 2>&1 | head -8 || echo "not loaded"
    fi
    ;;
  *)
    echo "usage: $0 {install|uninstall|reinstall|status|write-plist}"
    exit 1
    ;;
esac
