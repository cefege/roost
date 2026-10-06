#!/usr/bin/env bash
# The enrollment URL that coordinators up to v3.0.0-rc.10 print in their
# Add machine command. It runs install.sh, which joins when the pasted
# ROOST_COORDINATOR_URL and ROOST_BOOTSTRAP_TOKEN are in the environment;
# install.sh is the one implementation, so this file holds no logic of its own.
# It stays while a coordinator that prints this URL can still be running.

set -euo pipefail

INSTALL_SCRIPT_URL="https://raw.githubusercontent.com/cefege/roost/v3/install.sh"
installer="$(curl -fsSL "$INSTALL_SCRIPT_URL")" || {
  printf 'ERROR: could not fetch %s\n  Nothing was installed.\n' "$INSTALL_SCRIPT_URL" >&2
  exit 1
}
exec bash -c "$installer" install.sh "$@"
