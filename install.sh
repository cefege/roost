#!/usr/bin/env bash
# Roost installer for macOS and Linux. One command, two jobs:
#
#   First machine: install a coordinator and this machine's worker, then open a
#   browser already paired with it (`roost quickstart`):
#     curl -fsSL https://raw.githubusercontent.com/cefege/roost/v3/install.sh | bash
#   Arguments after `bash -s --` go to quickstart, e.g. `--dry-run` to see the
#   whole plan with nothing written, or `--coordinator-url https://…` for a
#   front door.
#
#   Another machine: join an existing coordinator (`roost join`). Copy the line
#   from Settings → Machines → Add machine, or `roost add-machine`:
#     curl -fsSL https://raw.githubusercontent.com/cefege/roost/v3/install.sh | \
#       ROOST_COORDINATOR_URL="https://roost.example.com" \
#       ROOST_BOOTSTRAP_TOKEN="roost_bt_…" [ROOST_WORKER_LABEL="my-box"] bash
#
# What it does: check the platform, find a v3 `roost` on this machine or fetch
# the newest v3 release's `roost` and `roost-keeper` and check both against the
# digests published beside them, then hand over to `roost quickstart` or
# `roost join`. Everything that installs a service is in the binary; this script
# only decides which binary runs.
#
# THE BRANCH, NOT A TAG. The URL points at `v3`, so a fix to this chain reaches
# a one-liner an operator pasted months ago. A pinned tag would mean editing
# this URL on every release, and every printed enrollment command with it.
#
# THE BODY IS ONE FUNCTION, CALLED ON THE LAST LINE. `curl | bash` executes as
# it downloads, so a connection cut mid-script would otherwise run half of it;
# and the program this hands over to inherits the pipe as stdin. Bash parses
# the whole function before the call runs, so neither can happen.

set -euo pipefail

say() { printf '>> %s\n' "$1"; }
die() { printf 'ERROR: %s\n' "$1" >&2; shift; for h in "$@"; do printf '  %s\n' "$h" >&2; done; exit 1; }

# --- fetching the first binary -------------------------------------------------
#
# WHAT THE DIGEST PROVES, STATED WHERE IT IS CHECKED. The sidecar and the asset
# come from the same origin over the same connection, so a successful check says
# the downloaded program matches the digest published BESIDE IT — and not that
# either is the one the maintainer built. That is the ordinary property of every
# published-checksum workflow, and the success line below says exactly this and
# nothing more. An origin that rewrites both files passes this check; that is
# what a mirror is.

# The origin, overridable by the SAME variable `roost update` reads, so an
# operator behind a mirror pins one thing rather than two, and a machine that
# installed by script and later runs `roost update` ends up on the same release.
RELEASE_REPO="cefege/roost"
RELEASE_API="${ROOST_RELEASE_API_URL:-https://api.github.com/repos/${RELEASE_REPO}/releases?per_page=100}"
RELEASE_DOWNLOADS="${ROOST_RELEASE_BASE_URL:-https://github.com/${RELEASE_REPO}/releases/download}"

# The asset name for this machine. These are the four names the release
# pipeline publishes, and they are the same strings `roost update` resolves, so
# a machine that installs by script and a machine that later runs `roost update`
# end up on the same release.
asset_name() {
  local os arch
  case "$(uname -s)" in
    Darwin) os="darwin" ;;
    *)      os="linux" ;;
  esac
  case "$(uname -m)" in
    arm64|aarch64) arch="arm64" ;;
    *)             arch="x64" ;;
  esac
  if [ "$os" = "darwin" ]; then
    [ "$arch" = "arm64" ] && printf 'roost' || printf 'roost-darwin-x64'
  else
    [ "$arch" = "arm64" ] && printf 'roost-linux-arm64' || printf 'roost-linux-x64'
  fi
}

# The newest published v3 tag, or nothing. Pre-releases count: the fleet runs
# `v3.0.0-rc.N` until `v3.0.0` exists, and an install that cannot find an rc is
# an install that cannot find a release.
newest_v3_tag() {
  curl -fsSL -H 'accept: application/vnd.github+json' "$RELEASE_API" 2>/dev/null \
    | grep -o '"tag_name": *"v3\.[^"]*"' \
    | head -1 \
    | sed 's/.*"tag_name": *"\(v3\.[^"]*\)".*/\1/'
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    printf ''
  fi
}

# Fetch the release's `roost` and `roost-keeper` for this machine into a
# directory this run creates, and print the staged `roost`. NEVER into
# ~/.local/bin: an install that wrote there replaced whatever `roost` the
# machine already had, and on a v2 machine that is v2's own CLI. Both
# `roost quickstart` and `roost join` copy the pair into the v3 release
# directory, so the staged pair is removed once they return.
fetch_roost() {
  local tag asset keeper base tmp got want
  tag="$(newest_v3_tag)"
  [ -n "$tag" ] || return 0
  asset="$(asset_name)"
  base="${RELEASE_DOWNLOADS}/${tag}"
  tmp="$(mktemp -d)" || return 0

  say "fetching ${asset} from ${tag}" >&2
  if ! curl -fsSL -o "${tmp}/roost" "${base}/${asset}"; then
    rm -rf "$tmp"
    return 0
  fi
  if ! curl -fsSL -o "${tmp}/roost.sha256" "${base}/${asset}.sha256"; then
    rm -rf "$tmp"
    return 0
  fi
  got="$(sha256_of "${tmp}/roost")"
  want="$(head -1 "${tmp}/roost.sha256" | cut -d' ' -f1 | tr -d '[:space:]')"
  if [ -z "$want" ]; then
    rm -rf "$tmp"
    die "The digest published beside ${asset} is not a digest." \
        "Nothing was installed."
  fi
  if [ "$got" != "$want" ]; then
    rm -rf "$tmp"
    die "The ${tag} asset does not match the digest published beside it." \
        "  expected ${want}" \
        "  actual   ${got}" \
        "Nothing was installed. Fetch it by hand and compare before relying on it:" \
        "  ${base}/${asset}"
  fi

  # The keeper is a SEPARATE PROGRAM and it must come from the SAME TAG as the
  # roost beside it. A keeper from a different release is a keeper contract the
  # worker was never built against, which is the mismatch
  # `ops/keeper_contract.rs` and keeper-update admission exist to refuse -- so
  # this runs inside fetch_roost, from `$tag`, into the same staging directory,
  # and never for a binary this function did not fetch.
  #
  # It is FATAL where the roost fetch is optional: a release `roost` with no
  # keeper beside it refuses to install, and a worker without one serves no
  # terminal. The staged path is printed only after BOTH assets have been
  # fetched and both digests have matched.
  keeper="$(asset_name)"
  keeper="roost-keeper${keeper#roost}"
  say "fetching ${keeper} from ${tag}" >&2
  if ! curl -fsSL -o "${tmp}/roost-keeper" "${base}/${keeper}"; then
    rm -rf "$tmp"
    die "The ${tag} release does not publish ${keeper}, so this machine would" \
        "get a worker whose keeper was never installed." \
        "Nothing was installed. Both programs are needed:" \
        "  ${base}/${asset}" \
        "  ${base}/${keeper}"
  fi
  if ! curl -fsSL -o "${tmp}/roost-keeper.sha256" "${base}/${keeper}.sha256"; then
    rm -rf "$tmp"
    die "No published digest beside ${keeper}." \
        "A keeper with no digest to match is not installed: the sidecar is the only thing" \
        "that says these bytes are the ones this release published." \
        "Nothing was installed."
  fi
  got="$(sha256_of "${tmp}/roost-keeper")"
  want="$(head -1 "${tmp}/roost-keeper.sha256" | cut -d' ' -f1 | tr -d '[:space:]')"
  if [ -z "$want" ]; then
    rm -rf "$tmp"
    die "The digest published beside ${keeper} is not a digest." "Nothing was installed."
  fi
  if [ "$got" != "$want" ]; then
    rm -rf "$tmp"
    die "The ${tag} ${keeper} does not match the digest published beside it." \
        "  expected ${want}" \
        "  actual   ${got}" \
        "Nothing was installed. Fetch it by hand and compare before relying on it:" \
        "  ${base}/${keeper}"
  fi
  chmod 0755 "${tmp}/roost" "${tmp}/roost-keeper"
  say "fetched ${tag} (${asset} and ${keeper}) into ${tmp}" >&2
  say "both match the digests published beside them" >&2
  printf '%s' "${tmp}/roost"
}

# `import-v2` is a v3 subcommand. The previous generation has no such verb, so
# its absence from --help identifies a pre-v3 binary without parsing a version
# string whose format has already changed once.
is_v3() {
  [ -x "$1" ] && "$1" --help 2>&1 | grep -q 'import-v2'
}

install_roost() {
  # 0. macOS (launchd) or Linux (systemd --user). Nothing else has a service
  # installer. A Rust binary carries no macOS version floor, so none is checked.
  case "$(uname -s)" in
    Darwin|Linux) ;;
    *) die "Roost installs on macOS or Linux only (found $(uname -s))." \
            "Any other device reaches Roost from a browser and needs no install." \
            "Nothing was installed." ;;
  esac

  # 1. Which job. The one-shot grant is what makes this a join; without it this
  # machine becomes a coordinator. A door with no grant is a pasted join line
  # that lost half of itself, and installing a coordinator in its place would
  # be the wrong machine set up silently, so that is refused.
  local command
  if [ -n "${ROOST_BOOTSTRAP_TOKEN:-}" ] || [ -n "${ROOST_COORDINATOR_URL:-}" ]; then
    if [ -z "${ROOST_COORDINATOR_URL:-}" ] || [ -z "${ROOST_BOOTSTRAP_TOKEN:-}" ]; then
      die "Joining needs both ROOST_COORDINATOR_URL and ROOST_BOOTSTRAP_TOKEN." \
          "Copy the whole line from Settings → Machines → Add machine, or from" \
          "\`roost add-machine\` on your coordinator. It looks like:" \
          "  curl -fsSL https://raw.githubusercontent.com/cefege/roost/v3/install.sh | \\" \
          "    ROOST_COORDINATOR_URL=\"https://roost.example.com\" \\" \
          "    ROOST_BOOTSTRAP_TOKEN=\"roost_bt_…\" bash" \
          "To set up a new coordinator on this machine instead, unset both and rerun."
    fi
    [ "$#" -eq 0 ] || die "Arguments are for a first install; a join takes none (got: $*)." \
                          "Nothing was installed."
    command="join"
  else
    command="quickstart"
  fi

  # 2. The binary, and refusing anything that is not a v3 one.
  #
  # ORDER MATTERS AND THE ORDER IS NOT "whatever PATH says". A machine that has
  # never had v3 installed may still have the PREVIOUS generation's `roost` on
  # PATH, and `command -v` finds it first. Handing a v3 grant to a v2 binary
  # half-enrolls a v2 worker and reports success: a fleet roster that looks
  # converged and a terminal that serves nothing. So the self-link location is
  # asked first, PATH second, and every candidate must then PROVE it is v3
  # before anything is exec'd.
  local roost_bin="${ROOST_BIN:-}"
  if [ -n "$roost_bin" ] && [ "${roost_bin#/}" = "$roost_bin" ]; then
    roost_bin="$PWD/$roost_bin"
  fi
  if [ -z "$roost_bin" ] && [ -x "$HOME/.local/bin/roost" ]; then
    roost_bin="$HOME/.local/bin/roost"
  fi
  if [ -z "$roost_bin" ] && command -v roost >/dev/null 2>&1; then
    roost_bin="$(command -v roost)"
  fi
  if [ -n "$roost_bin" ] && ! is_v3 "$roost_bin"; then
    say "found $roost_bin, which is not a v3 roost — fetching the release instead"
    roost_bin=""
  fi

  local staged=""
  if [ -z "$roost_bin" ]; then
    roost_bin="$(fetch_roost)"
    if [ -n "$roost_bin" ]; then staged="$(dirname "$roost_bin")"; fi
  fi
  [ -n "$roost_bin" ] || die "No v3 \`roost\` is available on this machine." \
                              "If a message above named a specific asset, that is why:" \
                              "\`fetch_roost\` runs inside a command substitution, so a refusal" \
                              "there exits only the substitution and not this script." \
                              "The release assets are at https://github.com/cefege/roost/releases" \
                              "To try a local build instead, set ROOST_BIN to its path, e.g." \
                              "  ROOST_BIN=./target/release/roost"
  [ -x "$roost_bin" ] || die "ROOST_BIN=$roost_bin is not executable." \
                              "Nothing was installed."

  # A binary this script did not fetch gets no keeper fetched beside it: the
  # newest release's keeper may not match that binary's version, and a
  # mismatched pair is the contract `ops/keeper_contract.rs` refuses. An
  # installed release already has its own keeper beside it.

  # 3. Hand over. ROOST_COORDINATOR_URL / ROOST_BOOTSTRAP_TOKEN /
  # ROOST_WORKER_LABEL are already in the environment and are inherited. Every
  # message from the binary goes to this terminal, which is the operator
  # watching.
  say "roost ${command} ($roost_bin)"
  if [ -z "$staged" ]; then
    exec "$roost_bin" "$command" "$@"
  fi
  # Not exec'd: the staged pair is this run's to remove once the binary has
  # copied it into the v3 release directory.
  local status=0
  "$roost_bin" "$command" "$@" || status=$?
  rm -rf "$staged"
  return "$status"
}

install_roost "$@"
