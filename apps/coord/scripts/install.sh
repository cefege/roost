#!/usr/bin/env bash
# install / uninstall the v2 coord service. macOS → launchd LaunchAgent;
# Linux → systemd --user unit. One shape: the coordinator serves PLAINTEXT on
# loopback and trusts X-Forwarded-For from the operator's own front door, which
# is told to it as ROOST_WEB_PUBLIC_URL. Runs `bun apps/coord/src/main.ts`.

set -euo pipefail

REPO_ROOT="${ROOST_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)}"
# Load repo-root defaults unless the caller supplies an authoritative endpoint.
# Quickstart and CoordTarget set ROOST_SKIP_ENV_LOCAL=1 so stale checkout-local
# values cannot replace the selected public URL or bind.
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
  [[ "$COORD_MEM_HIGH" =~ ^[0-9]+([.][0-9]+)?[KMGTP]?$ ]] || { echo "invalid ROOST_COORD_MEMORY_HIGH" >&2; exit 1; }
  [[ "$COORD_MEM_MAX" =~ ^[0-9]+([.][0-9]+)?[KMGTP]?$ ]] || { echo "invalid ROOST_COORD_MEMORY_MAX" >&2; exit 1; }
  [[ "$COORD_TASKS_MAX" =~ ^[0-9]+$ ]] || { echo "invalid ROOST_COORD_TASKS_MAX" >&2; exit 1; }
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
WEB_PUBLIC_URL="${ROOST_WEB_PUBLIC_URL:-}"
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

# Service definitions are data formats. Escape all dynamic values before
# interpolation so operator-controlled URLs and paths cannot inject plist keys
# or systemd directives.
xml_escape() {
  local value="$1"
  printf '%s' "$value" | sed \
    -e 's/&/\&amp;/g' \
    -e 's/</\&lt;/g' \
    -e 's/>/\&gt;/g' \
    -e 's/"/\&quot;/g' \
    -e "s/'/\&apos;/g"
}

# launchd will not load a file that is not a complete plist, and it reports no
# error for one it never loaded: the service simply stays absent from
# `launchctl list` until an operator notices. A redirect straight onto $PLIST
# truncates the installed job before the first byte of the replacement lands,
# so any interruption leaves an unloadable stub and no way back. Stage beside
# the target, prove it parses, then rename over it.
plist_is_complete() {
  grep -q '^</plist>$' "$1" || return 1
  command -v plutil >/dev/null 2>&1 || return 0
  plutil -lint "$1" >/dev/null 2>&1
}

install_plist() {
  local target="$1" staged
  staged="$(mktemp "${target}.new.XXXXXX")" || { echo "cannot stage $target" >&2; return 1; }
  if ! cat > "$staged" || ! plist_is_complete "$staged"; then
    rm -f "$staged"
    echo "refusing to install a malformed $target" >&2
    return 1
  fi
  chmod 0600 "$staged"
  mv -f "$staged" "$target" || { rm -f "$staged"; return 1; }
}

systemd_escape() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//$'\n'/\\n}"
  value="${value//$'\r'/\\r}"
  value="${value//$'\t'/\\t}"
  printf '%s' "$value"
}

systemd_quote() {
  printf '"%s"' "$(systemd_escape "$1")"
}

# systemd quoting is NOT universal: it applies to command lines (ExecStart=) and
# Environment=, but path/specifier settings — WorkingDirectory=, StandardOutput=,
# StandardError= — take the raw value. Quoting those makes systemd reject
# WorkingDirectory as "path is not absolute" (fatal: the unit refuses to start)
# and silently IGNORE the output specifiers, so logs vanish. Emit them raw, with
# `%` doubled so it is never read as a specifier, and refuse a value carrying the
# control characters that would let it forge a directive.
systemd_path() {
  local value="$1"
  if [[ "$value" == *$'\n'* || "$value" == *$'\r'* || "$value" == *'"'* ]]; then
    echo "refusing unit path with control characters: $value" >&2
    exit 1
  fi
  printf '%s' "${value//%/%%}"
}

systemd_env() {
  local value="$2"
  value="${value//%/%%}"
  printf 'Environment="%s=%s"\n' "$1" "$(systemd_escape "$value")"
}

# The coordinator owns no TLS, DNS, or tunnel: it binds loopback in plaintext
# and the operator's front door terminates TLS and sets X-Forwarded-For. That
# also dodges the Bun 1.3.14 segfault in us_internal_ssl_on_close /
# RequestContext.onAbort, which fired when a browser aborted a long-lived
# streaming TLS response (the Sync firehose) and left the coord crash-looping.
COORD_LOOPBACK_PORT="${ROOST_COORD_LOOPBACK_PORT:-4103}"
BIND_VALUE="${ROOST_COORDINATOR_BIND:-127.0.0.1:${COORD_LOOPBACK_PORT}}"
if [[ ! "$BIND_VALUE" =~ ^127\.0\.0\.1:[0-9]+$ ]]; then
  echo "ROOST_COORDINATOR_BIND must be 127.0.0.1:<port>; put your own front door in front of it" >&2
  exit 1
fi

# Stamp the current repo HEAD into coord's env so misc.health.git_sha
# returns a real SHA instead of "dev". SPA's MachineSection compares
# each worker's last-reported git_sha to this value and flags stale
# workers.
GIT_SHA_PLIST=""
GIT_SHA_RESOLVED="${ROOST_GIT_SHA:-$(cd "$REPO_ROOT" && git rev-parse HEAD 2>/dev/null || true)}"
if [[ -n "$GIT_SHA_RESOLVED" ]]; then
  GIT_SHA_PLIST=$'\n    <key>ROOST_GIT_SHA</key>\n    <string>'"$(xml_escape "${GIT_SHA_RESOLVED}")"$'</string>'
fi

ENDPOINT_PLIST=$'\n    <key>ROOST_TRUST_PROXY</key>\n    <string>1</string>'
[[ -n "$WEB_PUBLIC_URL" ]] && ENDPOINT_PLIST+=$'\n    <key>ROOST_WEB_PUBLIC_URL</key>\n    <string>'"$(xml_escape "${WEB_PUBLIC_URL}")"$'</string>'

cmd="${1:-status}"

write_plist() {
  mkdir -p "$DATA_DIR" "$LOG_DIR"
  # ProgramArguments/workdir/dist switch by execution form: ROOST_EXEC_BIN set
  # means compiled binary (`roost coord`); unset means from-source
  # (`bun …/main.ts`). The selected network mode's endpoint environment is
  # otherwise identical for both forms.
  local prog_bin prog_arg2 workdir web_dist label_xml prog_bin_xml prog_arg2_xml workdir_xml home_xml bind_xml db_xml auth_xml key_xml handoff_xml public_url_xml web_dist_xml diag_xml log_dir_xml
  if [[ -n "${ROOST_EXEC_BIN:-}" ]]; then
    prog_bin="${ROOST_EXEC_BIN}"; prog_arg2="coord"
  else
    prog_bin="${BUN_BIN}"; prog_arg2="${REPO_ROOT}/apps/coord/src/main.ts"
  fi
  workdir="${ROOST_WORKDIR:-$REPO_ROOT}"
  web_dist="${ROOST_WEB_DIST_PATH:-$REPO_ROOT/apps/web/dist}"
  label_xml="$(xml_escape "$LABEL")"
  prog_bin_xml="$(xml_escape "$prog_bin")"
  prog_arg2_xml="$(xml_escape "$prog_arg2")"
  workdir_xml="$(xml_escape "$workdir")"
  home_xml="$(xml_escape "$HOME")"
  bind_xml="$(xml_escape "$BIND_VALUE")"
  db_xml="$(xml_escape "$DB_PATH")"
  auth_xml="$(xml_escape "$AUTH_KEYS")"
  key_xml="$(xml_escape "$COORD_KEY")"
  handoff_xml="$(xml_escape "$HANDOFF_PATH")"
  public_url_xml="$(xml_escape "$PUBLIC_URL")"
  web_dist_xml="$(xml_escape "$web_dist")"
  diag_xml="$(xml_escape "${ROOST_DIAG:-0}")"
  log_dir_xml="$(xml_escape "$LOG_DIR")"
  install_plist "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>${label_xml}</string>
  <key>ProgramArguments</key>
  <array>
    <string>${prog_bin_xml}</string>
    <string>${prog_arg2_xml}</string>
  </array>
  <key>WorkingDirectory</key>
  <string>${workdir_xml}</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HOME</key>
    <string>${home_xml}</string>
    <key>PATH</key>
    <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
    <key>ROOST_COORDINATOR_BIND</key>
    <string>${bind_xml}</string>
    <key>ROOST_COORDINATOR_DB</key>
    <string>${db_xml}</string>
    <key>ROOST_COORDINATOR_AUTHORIZED_KEYS</key>
    <string>${auth_xml}</string>
    <key>ROOST_COORDINATOR_KEY_PATH</key>
    <string>${key_xml}</string>
    <key>ROOST_COORDINATOR_HANDOFF_PATH</key>
    <string>${handoff_xml}</string>
    <key>ROOST_COORDINATOR_PUBLIC_URL</key>
    <string>${public_url_xml}</string>
    <key>ROOST_WEB_DIST_PATH</key>
    <string>${web_dist_xml}</string>
    <key>ROOST_DIAG</key>
    <string>${diag_xml}</string>${GIT_SHA_PLIST}${ENDPOINT_PLIST}
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
  <string>${log_dir_xml}/main.out.log</string>
  <key>StandardErrorPath</key>
  <string>${log_dir_xml}/main.err.log</string>
</dict>
</plist>
EOF
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
  local prog_bin prog_args_unit workdir web_dist prog_bin_unit workdir_unit stdout_unit stderr_unit
  if [[ -n "${ROOST_EXEC_BIN:-}" ]]; then
    prog_bin="${ROOST_EXEC_BIN}"
    prog_args_unit="$(systemd_quote "coord")"
  else
    prog_bin="${BUN_BIN}"
    prog_args_unit="$(systemd_quote "--env-file=/dev/null") $(systemd_quote "${REPO_ROOT}/apps/coord/src/main.ts")"
  fi
  workdir="${ROOST_WORKDIR:-$REPO_ROOT}"
  web_dist="${ROOST_WEB_DIST_PATH:-$REPO_ROOT/apps/web/dist}"
  prog_bin_unit="$(systemd_quote "$prog_bin")"
  workdir_unit="$(systemd_path "$workdir")"
  stdout_unit="$(systemd_path "append:${LOG_DIR}/main.out.log")"
  stderr_unit="$(systemd_path "append:${LOG_DIR}/main.err.log")"
  {
    cat <<EOF
[Unit]
Description=Roost coordinator
After=network-online.target

[Service]
Type=simple
WorkingDirectory=${workdir_unit}
ExecStart=${prog_bin_unit} ${prog_args_unit}
EOF
    systemd_env "HOME" "$HOME"
    systemd_env "PATH" "$HOME/.bun/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
    systemd_env "ROOST_COORDINATOR_BIND" "$BIND_VALUE"
    systemd_env "ROOST_COORDINATOR_DB" "$DB_PATH"
    systemd_env "ROOST_COORDINATOR_AUTHORIZED_KEYS" "$AUTH_KEYS"
    systemd_env "ROOST_COORDINATOR_KEY_PATH" "$COORD_KEY"
    systemd_env "ROOST_COORDINATOR_HANDOFF_PATH" "$HANDOFF_PATH"
    systemd_env "ROOST_COORDINATOR_PUBLIC_URL" "$PUBLIC_URL"
    systemd_env "ROOST_COORD_DATA_DIR" "$DATA_DIR"
    systemd_env "ROOST_COORD_LOG_DIR" "$LOG_DIR"
    systemd_env "ROOST_WEB_DIST_PATH" "$web_dist"
    systemd_env "ROOST_DIAG" "${ROOST_DIAG:-0}"
    systemd_env "ROOST_COORD_MEMORY_HIGH" "$COORD_MEM_HIGH"
    systemd_env "ROOST_COORD_MEMORY_MAX" "$COORD_MEM_MAX"
    systemd_env "ROOST_COORD_TASKS_MAX" "$COORD_TASKS_MAX"
    systemd_env "ROOST_COORD_LOGROTATE_CONF" "$LOGROTATE_CONF"
    systemd_env "ROOST_TRUST_PROXY" "1"
    [[ -n "$GIT_SHA_RESOLVED" ]] && systemd_env "ROOST_GIT_SHA" "$GIT_SHA_RESOLVED"
    [[ -n "${ROOST_EXEC_BIN:-}" ]] && systemd_env "ROOST_EXEC_BIN" "$ROOST_EXEC_BIN"
    [[ -n "${ROOST_WEB_PUBLIC_URL:-}" ]] && systemd_env "ROOST_WEB_PUBLIC_URL" "$ROOST_WEB_PUBLIC_URL"
    # RestartSec=1 is the systemd analogue of the plist's ThrottleInterval 1:
    # a Bun crash must not freeze every browser's Sync stream for 10s.
    cat <<EOF
Restart=always
RestartSec=1
TimeoutStopSec=10
MemoryHigh=${COORD_MEM_HIGH}
MemoryMax=${COORD_MEM_MAX}
TasksMax=${COORD_TASKS_MAX}
StandardOutput=${stdout_unit}
StandardError=${stderr_unit}

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

service_alive() {
  if $IS_LINUX; then
    systemctl --user is-active --quiet "${LABEL}.service"
  else
    launchctl print "gui/$UID/${LABEL}" >/dev/null 2>&1
  fi
}

wait_until_ready() {
  local attempts="${ROOST_INSTALL_READY_ATTEMPTS:-30}"
  local interval="${ROOST_INSTALL_READY_INTERVAL_SECS:-1}"
  local health_url body code
  health_url="http://${BIND_VALUE}/roost.v1.CoordinatorService/AuthCoordIdentity"

  for ((i = 0; i < attempts; i++)); do
    if ! service_alive; then
      echo "coordinator exited before readiness" >&2
      return 1
    fi

    body="$(mktemp)"
    code="$(curl -sS -o "$body" -w '%{http_code}' \
      -X POST -H 'content-type: application/json' --data '{}' "$health_url" 2>/dev/null || true)"
    if [[ "$code" == "200" && "$(< "$body")" =~ \"gitSha\"[[:space:]]*:[[:space:]]*\"[^\"]+\" ]]; then
      rm -f "$body"
      return 0
    fi
    rm -f "$body"
    sleep "$interval"
  done
  echo "coordinator identity probe did not succeed on ${BIND_VALUE} (last=${code:-unset})" >&2
  return 1
}

rollback_service_definition() {
  local definition="$1" snapshot="$2" had_prior="$3" prior_mode="$4"
  if [[ "$had_prior" == "1" ]]; then
    cp "$snapshot" "$definition"
    chmod "$prior_mode" "$definition"
  else
    rm -f "$definition"
  fi
  if $IS_LINUX; then
    systemctl --user daemon-reload 2>/dev/null || true
    if [[ "$had_prior" == "1" ]]; then
      systemctl --user enable --now "${LABEL}.service" 2>/dev/null || true
      systemctl --user restart "${LABEL}.service" 2>/dev/null || true
    else
      systemctl --user disable --now "${LABEL}.service" 2>/dev/null || true
    fi
  else
    launchctl bootout "gui/$UID/${LABEL}" 2>/dev/null || true
    if [[ "$had_prior" == "1" ]]; then
      launchctl bootstrap "gui/$UID" "$PLIST" 2>/dev/null || true
      launchctl enable "gui/$UID/${LABEL}" 2>/dev/null || true
      launchctl kickstart -k "gui/$UID/${LABEL}" 2>/dev/null || true
    fi
  fi
}

case "$cmd" in
  install)
    definition="$($IS_LINUX && echo "$UNIT" || echo "$PLIST")"
    snapshot="$(mktemp)"
    chmod 0600 "$snapshot"
    had_prior=0
    prior_mode=0600
    if [[ -f "$definition" ]]; then
      had_prior=1
      if $IS_LINUX; then prior_mode="$(stat -c '%a' "$definition")"; else prior_mode="$(stat -f '%Lp' "$definition")"; fi
      cp "$definition" "$snapshot"
      chmod 0600 "$snapshot"
    fi
    if ! (
      if $IS_LINUX; then
        write_unit && write_logrotate && bootstrap_systemd || exit 1
      else
        write_plist && bootstrap || exit 1
      fi
      wait_until_ready || exit 1
    ); then
      rollback_service_definition "$definition" "$snapshot" "$had_prior" "$prior_mode"
      rm -f "$snapshot"
      exit 1
    fi
    rm -f "$snapshot"
    echo
    echo "Coord v2 ready - bind ${BIND_VALUE}${WEB_PUBLIC_URL:+, front door ${WEB_PUBLIC_URL}}. Logs:"
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
