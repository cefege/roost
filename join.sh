#!/usr/bin/env bash
# Roost pull-based worker join. Run on a NEW machine (macOS or Linux) to go
# from nothing → registered worker, no SSH/push from the coordinator:
#   curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/join.sh | \
#     ROOST_COORDINATOR_URL="https://roost.example.com" \
#     ROOST_BOOTSTRAP_TOKEN="roost_bt_…" [ROOST_WORKER_LABEL="my-box"] bash
#
# Get that one-liner from `roost add-machine` on the coordinator (or the web
# Settings → Machines → Add machine dialog). What it does: check the platform,
# check the grant, find the `roost` binary, and hand the environment to
# `roost join`, which installs and registers the local worker.
#
# WHY THIS IS A LOCATE-AND-EXEC AND NOT AN INSTALLER. v2 fetched Bun, cloned
# the repository, pinned the checkout to the coordinator's live commit and ran
# `bun apps/roost-cli/src/main.ts join`. Every step of that spine is a v2
# artefact: there is no Bun in an all-v3 fleet and the TypeScript entrypoint is
# deleted at cutover, so a script that ended there dead-ended an operator in an
# invocation of a file that would not exist. v3 ships a binary, and the binary
# is the only thing this script needs to hand the grant to.
#
# WHAT IS NOT HERE YET, STATED RATHER THAN FAKED. A machine with nothing on it
# has no `roost` to find, and the answer to "where does the first binary come
# from" is the release pipeline, which lands with the v3 release. Inventing a
# download URL that does not resolve would be a script that fails later and
# further away, so this one fails here, by name, and says what is missing. The
# moment `roost` is on PATH this script is complete.

set -euo pipefail

say() { printf '>> %s\n' "$1"; }
die() { printf 'ERROR: %s\n' "$1" >&2; shift; for h in "$@"; do printf '  %s\n' "$h" >&2; done; exit 1; }

# 0. macOS (launchd) or Linux (systemd --user). Nothing else has a service
# installer. v2 also refused macOS below 13 here, and that check is GONE rather
# than relaxed: its stated reason was "Bun does [require macOS 13]", and there
# is no Bun. A Rust binary carries no such floor, so keeping the check would
# refuse a machine that can run the worker perfectly well.
case "$(uname -s)" in
  Darwin|Linux) ;;
  *) die "Roost joins on macOS or Linux only (found $(uname -s))." \
          "Nothing was installed." ;;
esac

# 1. Required env — the join target + credential come from `roost add-machine`.
if [ -z "${ROOST_COORDINATOR_URL:-}" ] || [ -z "${ROOST_BOOTSTRAP_TOKEN:-}" ]; then
  die "ROOST_COORDINATOR_URL and ROOST_BOOTSTRAP_TOKEN are required." \
      "Run \`roost add-machine --platform macos\` or \`roost add-machine --platform linux\` on your coordinator" \
      "to get the full one-liner, then paste it here. It looks like:" \
      "  curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/join.sh | \\" \
      "    ROOST_COORDINATOR_URL=\"https://roost.example.com\" \\" \
      "    ROOST_BOOTSTRAP_TOKEN=\"roost_bt_…\" bash"
fi

# 2. The binary. ROOST_BIN wins so an operator mid-install can point at a build
#    without touching PATH; then PATH; then the install location the CLI's own
#    `self-link` maintains. A relative ROOST_BIN is resolved against the CURRENT
#    directory, because that is the shell's rule and a different one here would
#    make the same string mean two things.
ROOST_BIN="${ROOST_BIN:-}"
if [ -n "$ROOST_BIN" ] && [ "${ROOST_BIN#/}" = "$ROOST_BIN" ]; then
  ROOST_BIN="$PWD/$ROOST_BIN"
fi
if [ -z "$ROOST_BIN" ] && command -v roost >/dev/null 2>&1; then
  ROOST_BIN="$(command -v roost)"
fi
if [ -z "$ROOST_BIN" ] && [ -x "$HOME/.local/bin/roost" ]; then
  ROOST_BIN="$HOME/.local/bin/roost"
fi
if [ -z "$ROOST_BIN" ]; then
  die "No \`roost\` binary on this machine, and nothing was installed." \
      "Install the v3 release first — it is one program, not a toolchain:" \
      "  see GETTING_STARTED.md, or the release assets for this platform" \
      "Then re-run this one-liner. To try a local build instead, set ROOST_BIN" \
      "to its path, e.g. ROOST_BIN=./target/release/roost" \
      "A machine that already has a coordinator on it does not need this at all:" \
      "  roost add-machine --platform <macos|linux>"
fi
[ -x "$ROOST_BIN" ] || die "ROOST_BIN=$ROOST_BIN is not executable." \
                            "Nothing was installed."

# 3. Hand the grant over. ROOST_COORDINATOR_URL / ROOST_BOOTSTRAP_TOKEN /
#    ROOST_WORKER_LABEL are already in the environment and are inherited.
#    `join` is the command that installs and registers the worker; every message
#    from it goes to this terminal, which is the operator watching.
say "roost join ($ROOST_BIN)"
exec "$ROOST_BIN" join
