#!/usr/bin/env bash
# install / uninstall the v2 worker service. macOS → launchd LaunchAgent;
# Linux → systemd --user unit. Runs `bun apps/worker/src/main.ts`.
# REWRITE.md R0.13.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
# Load host-local defaults from repo-root .env.local (gitignored), then restore
# every variable explicitly exported by the caller. Deployment passes the
# enrolled worker identity this way; a checkout-local coordinator URL must
# never overwrite it. Variables absent from the caller still come from the
# host-local file.
_ROOST_CALLER_ENV="$(export -p)"
set -a; [ -f "$REPO_ROOT/.env.local" ] && source "$REPO_ROOT/.env.local"; set +a
eval "$_ROOST_CALLER_ENV"
unset _ROOST_CALLER_ENV
# MemoryHigh must scale with the host: every PTY session lives in the worker's
# cgroup, so a flat cap sized for a laptop throttles the worker's own event loop
# into D-state on a big box (2026-08-01: ovh1, 1.4M memory.high events, worker
# invisible to coord for minutes at a time). 60% of RAM, floor 3G. Absolute
# value, not a percentage: systemd only accepts % for MemoryHigh from v240.
default_worker_mem_high() {
  local total_kb mb
  # MEMINFO is an override seam so the floor branch is testable without a 2GB box.
  total_kb=$(awk '/^MemTotal:/{print $2}' "${MEMINFO:-/proc/meminfo}" 2>/dev/null || echo 0)
  mb=$(( total_kb * 60 / 100 / 1024 ))
  if [ "$mb" -lt 3072 ]; then echo 3G; else echo "${mb}M"; fi
}
# Labels/paths overridable (binary-mode quickstart + isolated test installs);
# defaults are the daily-driver source install — unchanged when unset. NOTE:
# ROOST_WORKER_AGENT_LABEL is the launchd label / systemd unit name, distinct
# from ROOST_WORKER_LABEL (the worker's display name).
OS="$(uname -s)"
if [[ "$OS" == "Linux" ]]; then
  LABEL="${ROOST_WORKER_AGENT_LABEL:-roost-worker}"
  UNIT="${ROOST_WORKER_UNIT:-$HOME/.config/systemd/user/${LABEL}.service}"
  DATA_DIR="${ROOST_WORKER_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/RoostWorkerV2}"
  LOG_DIR="${ROOST_WORKER_LOG_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/RoostWorker}"
  # Resource limits baked into the unit; MemoryHigh defaults to 60% of host RAM
  # (floor 3G). Dial either down on a small box via env.
  WORKER_MEM_HIGH="${ROOST_WORKER_MEMORY_HIGH:-$(default_worker_mem_high)}"
  WORKER_TASKS_MAX="${ROOST_WORKER_TASKS_MAX:-4096}"
  LOGROTATE_CONF="${ROOST_WORKER_LOGROTATE_CONF:-${XDG_CONFIG_HOME:-$HOME/.config}/logrotate.d/roost-worker.conf}"
else
  LABEL="${ROOST_WORKER_AGENT_LABEL:-com.roost.worker-v2}"
  PLIST="${ROOST_WORKER_PLIST:-$HOME/Library/LaunchAgents/${LABEL}.plist}"
  DATA_DIR="${ROOST_WORKER_DATA_DIR:-$HOME/Library/Application Support/RoostWorkerV2}"
  LOG_DIR="${ROOST_WORKER_LOG_DIR:-$HOME/Library/Logs/RoostWorker}"
fi
# Resolve a runtime binary by searching in order: explicit env override,
# `command -v`, then a fallback list including ~/.bun/bin and ~/.node/bin
# so a tarball install on a fresh Mac without Homebrew also works. If
# nothing resolves, returns the canonical Homebrew path as a placeholder
# so install.sh can still write the plist (the worker will then warn).
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

# Required env: ROOST_COORDINATOR_URL — http(s)://coord-host:4102
# Optional env: ROOST_BOOTSTRAP_TOKEN (one-shot, cleared after redeem),
#               ROOST_WORKER_LABEL, ROOST_REACHABLE_ADDR
ROOST_COORDINATOR_URL="${ROOST_COORDINATOR_URL:-}"
if [[ -z "$ROOST_COORDINATOR_URL" && "${1:-status}" == "install" ]]; then
  echo "ERROR: ROOST_COORDINATOR_URL env var is required for install" >&2
  echo "  ROOST_COORDINATOR_URL=https://<your-coord-host>:4102 $0 install  (or set it in .env.local)" >&2
  exit 1
fi

# Optional plist entries — only emit the key/value block if env is set.
BOOTSTRAP_TOKEN_PLIST=""
LABEL_PLIST=""
REACHABLE_ADDR_PLIST=""
TLS_PLIST=""
if [[ -n "${ROOST_BOOTSTRAP_TOKEN:-}" ]]; then
  BOOTSTRAP_TOKEN_PLIST=$'\n    <key>ROOST_BOOTSTRAP_TOKEN</key>\n    <string>'"${ROOST_BOOTSTRAP_TOKEN}"$'</string>'
fi
if [[ -n "${ROOST_WORKER_LABEL:-}" ]]; then
  LABEL_PLIST=$'\n    <key>ROOST_WORKER_LABEL</key>\n    <string>'"${ROOST_WORKER_LABEL}"$'</string>'
fi
if [[ -n "${ROOST_REACHABLE_ADDR:-}" ]]; then
  REACHABLE_ADDR_PLIST=$'\n    <key>ROOST_REACHABLE_ADDR</key>\n    <string>'"${ROOST_REACHABLE_ADDR}"$'</string>'
fi
# Tailnet TLS via `tailscale cert <fqdn>`. When both are set, worker
# serves WSS instead of WS — required for browsers connecting over
# https:// (mixed-content rule blocks ws:// from https origin).
# Expand a leading tilde here — the plist value goes straight to
# readFileSync which doesn't shell-expand.
if [[ -n "${ROOST_TLS_CERT_PATH:-}" && -n "${ROOST_TLS_KEY_PATH:-}" ]]; then
  ROOST_TLS_CERT_PATH="${ROOST_TLS_CERT_PATH/#\~/$HOME}"
  ROOST_TLS_KEY_PATH="${ROOST_TLS_KEY_PATH/#\~/$HOME}"
  TLS_PLIST=$'\n    <key>ROOST_TLS_CERT_PATH</key>\n    <string>'"${ROOST_TLS_CERT_PATH}"$'</string>\n    <key>ROOST_TLS_KEY_PATH</key>\n    <string>'"${ROOST_TLS_KEY_PATH}"$'</string>'
fi

# Stamp the current repo HEAD into the LaunchAgent so the running worker
# reports it via heartbeat. SPA compares to coord's GIT_SHA and flags
# the worker as stale when they diverge — drives the "Deploy" button in
# MachineSection.
GIT_SHA_PLIST=""
GIT_SHA_RESOLVED="${GIT_SHA:-$(cd "$REPO_ROOT" && git rev-parse HEAD 2>/dev/null || true)}"
if [[ -n "$GIT_SHA_RESOLVED" ]]; then
  GIT_SHA_PLIST=$'\n    <key>GIT_SHA</key>\n    <string>'"${GIT_SHA_RESOLVED}"$'</string>'
fi
# ROOST_EXEC_BIN / ROOST_WORKDIR must reach the RUNNING worker, not just the
# installer: CoordTarget branches on ROOST_EXEC_BIN to decide whether it must
# `bun run build` the SPA, and forwards it to the coordinator installer so the
# bootstrapped coord uses the compiled binary rather than a from-source
# ExecStart that crash-loops.
EXEC_BIN_PLIST=""
WORKDIR_PLIST=""
if [[ -n "${ROOST_EXEC_BIN:-}" ]]; then
  EXEC_BIN_PLIST=$'\n    <key>ROOST_EXEC_BIN</key>\n    <string>'"${ROOST_EXEC_BIN}"$'</string>'
fi
if [[ -n "${ROOST_WORKDIR:-}" ]]; then
  WORKDIR_PLIST=$'\n    <key>ROOST_WORKDIR</key>\n    <string>'"${ROOST_WORKDIR}"$'</string>'
fi

cmd="${1:-status}"

write_plist() {
  mkdir -p "$DATA_DIR" "$LOG_DIR"
  # ProgramArguments/workdir switch by mode: ROOST_EXEC_BIN set → compiled
  # binary (`roost worker`); unset → from-source (`bun …/main.ts`).
  local prog_bin prog_arg2 workdir
  if [[ -n "${ROOST_EXEC_BIN:-}" ]]; then
    prog_bin="${ROOST_EXEC_BIN}"; prog_arg2="worker"
  else
    prog_bin="${BUN_BIN}"; prog_arg2="${REPO_ROOT}/apps/worker/src/main.ts"
  fi
  workdir="${ROOST_WORKDIR:-$REPO_ROOT}"
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
    <string>${HOME}/.bun/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
    <key>ROOST_WORKER_DATA_DIR</key>
    <string>${DATA_DIR}</string>
    <key>ROOST_COORDINATOR_URL</key>
    <string>${ROOST_COORDINATOR_URL}</string>
    <key>ROOST_DIAG</key>
    <string>\${ROOST_DIAG:-0}</string>${BOOTSTRAP_TOKEN_PLIST}${LABEL_PLIST}${REACHABLE_ADDR_PLIST}${TLS_PLIST}${GIT_SHA_PLIST}${EXEC_BIN_PLIST}${WORKDIR_PLIST}
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ProcessType</key>
  <string>Interactive</string>
  <key>StandardOutPath</key>
  <string>${LOG_DIR}/main.out.log</string>
  <key>StandardErrorPath</key>
  <string>${LOG_DIR}/main.err.log</string>
</dict>
</plist>
EOF
  chmod 0644 "$PLIST"
  echo "wrote $PLIST"
}

bootstrap() {
  launchctl bootout "gui/$UID/${LABEL}" 2>/dev/null || true
  # bootout is asynchronous — on loaded daily-driver agents launchd can keep
  # returning "Input/output error (5)" for several seconds after the service
  # disappears. Retry for up to ten seconds before failing the deployment.
  local i
  for i in 1 2 3 4 5 6 7 8 9 10; do
    sleep 1
    if launchctl bootstrap "gui/$UID" "$PLIST" 2>/dev/null; then
      break
    fi
    if [[ $i -eq 10 ]]; then
      echo "launchctl bootstrap failed after 10 retries" >&2
      exit 1
    fi
  done
  launchctl enable "gui/$UID/${LABEL}"
  launchctl kickstart -k "gui/$UID/${LABEL}" 2>/dev/null || true
  sleep 1
  echo "bootstrapped gui/$UID/${LABEL}"
}

# systemd --user counterpart of write_plist. Same env-key set minus the TLS
# pair (dropped in phase-25e; see apps/worker/src/config.ts).
write_unit() {
  mkdir -p "$(dirname "$UNIT")" "$DATA_DIR" "$LOG_DIR"
  local prog_bin prog_arg2 workdir
  if [[ -n "${ROOST_EXEC_BIN:-}" ]]; then
    prog_bin="${ROOST_EXEC_BIN}"; prog_arg2="worker"
  else
    prog_bin="${BUN_BIN}"; prog_arg2="${REPO_ROOT}/apps/worker/src/main.ts"
  fi
  workdir="${ROOST_WORKDIR:-$REPO_ROOT}"
  {
    cat <<EOF
[Unit]
Description=Roost worker
After=network-online.target

[Service]
Type=simple
WorkingDirectory=${workdir}
ExecStart=${prog_bin} ${prog_arg2}
Environment=HOME=${HOME}
Environment=PATH=${HOME}/.bun/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
Environment=ROOST_WORKER_DATA_DIR=${DATA_DIR}
Environment=ROOST_WORKER_LOG_DIR=${LOG_DIR}
Environment=ROOST_COORDINATOR_URL=${ROOST_COORDINATOR_URL}
Environment=ROOST_DIAG=${ROOST_DIAG:-0}
EOF
    [[ -n "${ROOST_BOOTSTRAP_TOKEN:-}" ]] && echo "Environment=ROOST_BOOTSTRAP_TOKEN=${ROOST_BOOTSTRAP_TOKEN}"
    [[ -n "${ROOST_WORKER_LABEL:-}" ]]    && echo "Environment=ROOST_WORKER_LABEL=${ROOST_WORKER_LABEL}"
    [[ -n "${ROOST_REACHABLE_ADDR:-}" ]]  && echo "Environment=ROOST_REACHABLE_ADDR=${ROOST_REACHABLE_ADDR}"
    [[ -n "$GIT_SHA_RESOLVED" ]]          && echo "Environment=GIT_SHA=${GIT_SHA_RESOLVED}"
    [[ -n "${ROOST_EXEC_BIN:-}" ]]        && echo "Environment=ROOST_EXEC_BIN=${ROOST_EXEC_BIN}"
    [[ -n "${ROOST_WORKDIR:-}" ]]         && echo "Environment=ROOST_WORKDIR=${ROOST_WORKDIR}"
    # KillMode=process is load-bearing: the keeper is spawned detached but
    # lands in this unit's cgroup, and the default control-group kill would
    # take every PTY down on each worker restart — destroying the "keeper
    # outlives worker restarts" invariant the reattach path depends on.
    #
    # MemoryMax is deliberately ABSENT and must stay absent: every PTY session
    # lives in this cgroup, so a hard cap plus Restart=always would let one fat
    # session kill the unit and take every other live session with it.
    # MemoryHigh only throttles and reclaims; OOMPolicy=continue closes the
    # same hole against a host-level OOM kill of a single child.
    cat <<EOF
Restart=always
RestartSec=1
KillMode=process
TimeoutStopSec=10
MemoryHigh=${WORKER_MEM_HIGH}
OOMPolicy=continue
TasksMax=${WORKER_TASKS_MAX}
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
# matching user timer is installed beside it; logrotate reads every file in a
# directory argument, so coord and worker each own one conf and share one timer.
write_logrotate() {
  local rotate_bin conf_dir unit_dir
  rotate_bin="$(command -v logrotate || true)"
  [[ -z "$rotate_bin" && -x /usr/sbin/logrotate ]] && rotate_bin=/usr/sbin/logrotate
  if [[ -z "$rotate_bin" ]]; then
    echo "WARN: logrotate not found - worker logs in ${LOG_DIR} will grow unbounded" >&2
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
  # Linger is mandatory: without it the user manager exits at logout and
  # takes the worker AND the detached keeper with it.
  loginctl enable-linger "$USER" 2>/dev/null || sudo -n loginctl enable-linger "$USER" 2>/dev/null || \
    echo "WARN: could not enable linger — the worker will stop when you log out" >&2
  # `tailscale serve` is a write to tailscaled and needs root or an operator
  # grant. Only matters if this box is ever made a coordinator: the coord
  # installer's serve_front runs as this user and merely WARNS on failure, so
  # without the grant a relocated coordinator binds loopback and is unreachable
  # at the https URL it advertises. Best-effort like linger above — CoordTarget
  # probes the capability at move preflight and blocks there, so a failure here
  # is a warning, not a broken install. Adds no privilege: anyone who can
  # `sudo -n` here can already run `sudo tailscale serve` directly.
  if command -v tailscale >/dev/null 2>&1; then
    sudo -n tailscale set --operator="$USER" 2>/dev/null || \
      echo "WARN: could not grant tailscale serve to $USER — run 'sudo tailscale set --operator=$USER' if this box should be able to host the coordinator" >&2
  fi
  systemctl --user daemon-reload
  systemctl --user enable --now "${LABEL}.service"
  systemctl --user restart "${LABEL}.service"
  echo "started ${LABEL}.service"
  if [[ -f "$(dirname "$UNIT")/roost-logrotate.timer" ]]; then
    systemctl --user enable --now roost-logrotate.timer 2>/dev/null || \
      echo "WARN: could not enable roost-logrotate.timer - logs will grow unbounded" >&2
  fi
}

IS_LINUX=false
[[ "$OS" == "Linux" ]] && IS_LINUX=true

case "$cmd" in
  install)
    if $IS_LINUX; then write_unit; write_logrotate; bootstrap_systemd; else write_plist; bootstrap; fi
    echo
    echo "Worker v2 starting on :2224. Logs:"
    echo "  bun apps/roost-cli/src/main.ts logs worker"
    ;;
  write-plist)
    if $IS_LINUX; then write_unit; else write_plist; fi
    ;;
  migrate-env)
    # Regenerate the service definition from the script's CURRENT canonical
    # form, then restart. Use this on already-installed hosts when the
    # canonical shape has changed (e.g. an env key was retired upstream and
    # lingers on disk).
    #
    # Difference from `reinstall`: migrate-env preserves data dirs +
    # keys (no `rm` of $DATA_DIR) and never asks the user to re-redeem
    # a bootstrap token. It's the in-place equivalent of "run install
    # again without uninstalling first" — every dead key written by an
    # older install.sh disappears because the file is replaced wholesale.
    #
    # If you also need to wipe the worker's data dir (keys, db,
    # sockets), use `reinstall` instead.
    if [[ -z "$ROOST_COORDINATOR_URL" ]]; then
      # write_plist/write_unit hard-fail on a missing ROOST_COORDINATOR_URL
      # upstream. Try to recover it from the installed service definition so
      # the user doesn't have to re-supply it just to scrub a dead env key.
      if $IS_LINUX; then
        [[ -f "$UNIT" ]] && ROOST_COORDINATOR_URL=$(sed -n 's/^Environment=ROOST_COORDINATOR_URL=//p' "$UNIT" | tail -1)
      elif [[ -f "$PLIST" ]]; then
        ROOST_COORDINATOR_URL=$(/usr/libexec/PlistBuddy -c "Print :EnvironmentVariables:ROOST_COORDINATOR_URL" "$PLIST" 2>/dev/null || true)
      fi
      export ROOST_COORDINATOR_URL
      if [[ -z "$ROOST_COORDINATOR_URL" ]]; then
        echo "ERROR: ROOST_COORDINATOR_URL not in env and not in the installed service" >&2
        exit 1
      fi
    fi
    if $IS_LINUX; then
      write_unit; bootstrap_systemd
      echo "regenerated $UNIT with canonical env + restarted ${LABEL}.service"
    else
      write_plist; bootstrap
      echo "regenerated $PLIST with canonical env + restarted gui/$UID/${LABEL}"
    fi
    ;;
  uninstall)
    if $IS_LINUX; then
      export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
      systemctl --user disable --now "${LABEL}.service" 2>/dev/null || true
      rm -f "$UNIT"
      systemctl --user daemon-reload
      echo "removed ${UNIT}"
    else
      launchctl bootout "gui/$UID/${LABEL}" 2>/dev/null || true
      rm -f "$PLIST"
      echo "removed ${PLIST}"
    fi
    echo "(data + keys left at ${DATA_DIR})"
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
    echo "usage: $0 {install|uninstall|reinstall|status|migrate-env}"
    exit 1
    ;;
esac
