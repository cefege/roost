#!/usr/bin/env bash
# Roost pull-based worker join. Run on a NEW machine (macOS or Linux) to go
# from nothing → registered worker, no SSH/push from the coordinator:
#   curl -fsSL https://raw.githubusercontent.com/cefege/roost/v3/join.sh | \
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
# THE FIRST BINARY IS FETCHED HERE, AND THAT IS THE POINT. An earlier version of
# this script located a `roost` and handed the grant to it, which made the chain
# circular on every machine that matters: a machine joining for the first time
# has no v3 install — that is why it is joining — so the lookup found whatever
# older `roost` was on PATH and exec'd THAT against a v3 coordinator. A v2
# binary enrolled as a half-working v2 worker and reported success. So a machine
# with nothing gets the release asset, checked against the digest published
# beside it, and the only thing this script will exec is a binary that reports
# itself as v3.
#
# THE BRANCH, NOT A TAG. The URL points at `v3`, so a fix to this chain reaches
# a machine that enrolled last month. A pinned tag would mean editing this URL on
# every release, and an operator who pasted the one-liner in month three would
# keep fetching month three's script forever — which is the same defect one
# layer up, in the enrolment command that prints this URL.

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
# joined by script and later runs `roost update` ends up on the same release.
RELEASE_REPO="cefege/roost"
RELEASE_API="${ROOST_RELEASE_API_URL:-https://api.github.com/repos/${RELEASE_REPO}/releases?per_page=100}"
RELEASE_DOWNLOADS="${ROOST_RELEASE_BASE_URL:-https://github.com/${RELEASE_REPO}/releases/download}"

# The asset name for this machine. These are the four names the release
# pipeline publishes, and they are the same strings `roost update` resolves, so
# a machine that joins by script and a machine that later runs `roost update`
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
# `v3.0.0-rc.N` until `v3.0.0` exists, and a join that cannot find an rc is a
# join that cannot find a release.
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

# Install the release's `roost` for this machine and print where it went.
fetch_roost() {
  local tag asset dest base dest_dir tmp got want
  tag="$(newest_v3_tag)"
  [ -n "$tag" ] || return 0
  asset="$(asset_name)"
  base="${RELEASE_DOWNLOADS}/${tag}"
  dest_dir="${HOME}/.local/bin"
  dest="${dest_dir}/roost"
  mkdir -p "$dest_dir"
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
        "Nothing was installed. Fetch it by hand and compare before trusting it:" \
        "  ${base}/${asset}"
  fi
  # Move into place only after the bytes matched, and through a name this
  # account owns: a system-wide roost belongs to the system, not to a join.
  mv "${tmp}/roost" "${dest}.incoming" && chmod 0755 "${dest}.incoming" && mv "${dest}.incoming" "$dest"
  rm -rf "$tmp"
  say "installed ${tag} (${asset}) to ${dest}" >&2
  say "it matches the digest published beside it" >&2
  printf '%s' "$dest"
}

# The keeper is a SEPARATE BINARY and a joined machine needs it, so this is not
# optional the way fetching `roost` is. `roost join` looks for `roost-keeper`
# BESIDE the running binary -- `LocalPrograms::of_this_process` reads
# `current_exe()` and joins `roost-keeper` onto its parent -- and it filters a
# missing file out rather than refusing. A machine that enrolled without a
# keeper would report success, appear in `roost api workers`, and serve no
# terminal: the "roster looks converged, terminal serves nothing" outcome the
# selection order above exists to prevent, arriving by a different door.
#
# So: SKIP if one is already beside the binary, FETCH if not, DIE if the fetch
# fails. An optional keeper is the defect.
ensure_keeper_beside() {
  local roost_path dir keeper tag base tmp got want
  roost_path="$1"
  dir="$(dirname "$roost_path")"
  if [ -x "${dir}/roost-keeper" ]; then
    return 0
  fi

  # The keeper's name is the roost name with its leading `roost` replaced, which
  # is `update/assets.rs::keeper_release_asset_name`. Written as a strip and a
  # prepend rather than a replacement: no subprocess, and no expansion form the
  # rest of this file does not already use. `${keeper#roost}` removes ONE
  # anchored occurrence, so a name that ever contained `roost` twice cannot have
  # its own suffix rewritten.
  keeper="$(asset_name)"
  keeper="roost-keeper${keeper#roost}"
  tag="$(newest_v3_tag)"
  [ -n "$tag" ] || die "The newest published v3 release could not be found, and this" \
                        "machine has no roost-keeper beside ${roost_path}." \
                        "A joined machine needs BOTH programs: the worker cannot" \
                        "run a session without its keeper." \
                        "Nothing was installed." \
                        "The release assets are at https://github.com/${RELEASE_REPO}/releases"
  base="${RELEASE_DOWNLOADS}/${tag}"
  tmp="$(mktemp -d)" || die "Could not create a temporary directory." "Nothing was installed."

  say "fetching ${keeper} from ${tag}" >&2
  if ! curl -fsSL -o "${tmp}/roost-keeper" "${base}/${keeper}"; then
    rm -rf "$tmp"
    die "The ${tag} release does not publish ${keeper}, and this machine has" \
        "no keeper beside ${roost_path}." \
        "A joined machine needs BOTH programs." \
        "Nothing was installed." \
        "  ${base}/${keeper}"
  fi
  if ! curl -fsSL -o "${tmp}/roost-keeper.sha256" "${base}/${keeper}.sha256"; then
    rm -rf "$tmp"
    die "No published digest beside ${keeper}." \
        "An unverified keeper is not installed: the digest is the only thing" \
        "that says these bytes are the ones the release published." \
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
        "Nothing was installed. Fetch it by hand and compare before trusting it:" \
        "  ${base}/${keeper}"
  fi
  mv "${tmp}/roost-keeper" "${dir}/.roost-keeper.incoming" \
    && chmod 0755 "${dir}/.roost-keeper.incoming" \
    && mv "${dir}/.roost-keeper.incoming" "${dir}/roost-keeper"
  rm -rf "$tmp"
  say "installed ${tag} (${keeper}) to ${dir}/roost-keeper" >&2
}

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
      "  curl -fsSL https://raw.githubusercontent.com/cefege/roost/v3/join.sh | \\" \
      "    ROOST_COORDINATOR_URL=\"https://roost.example.com\" \\" \
      "    ROOST_BOOTSTRAP_TOKEN=\"roost_bt_…\" bash"
fi

# 2. The binary, and refusing anything that is not a v3 one.
#
# ORDER MATTERS AND THE ORDER IS NOT "whatever PATH says". A machine that has
# never had v3 installed still has the PREVIOUS generation's `roost` somewhere on
# PATH, and `command -v` finds it first. Handing a v3 grant to a v2 binary
# half-enrolls a v2 worker and reports success, which is the worst outcome
# available: a fleet roster that looks converged and a terminal that serves
# nothing. So the self-link location is asked first, PATH second, and every
# candidate must then PROVE it is v3 before anything is exec'd.
ROOST_BIN="${ROOST_BIN:-}"
if [ -n "$ROOST_BIN" ] && [ "${ROOST_BIN#/}" = "$ROOST_BIN" ]; then
  ROOST_BIN="$PWD/$ROOST_BIN"
fi
if [ -z "$ROOST_BIN" ] && [ -x "$HOME/.local/bin/roost" ]; then
  ROOST_BIN="$HOME/.local/bin/roost"
fi
if [ -z "$ROOST_BIN" ] && command -v roost >/dev/null 2>&1; then
  ROOST_BIN="$(command -v roost)"
fi

# `import-v2` is a v3 subcommand. The previous generation has no such verb, so
# its absence from --help identifies a pre-v3 binary without parsing a version
# string whose format has already changed once.
is_v3() {
  [ -x "$1" ] && "$1" --help 2>&1 | grep -q 'import-v2'
}

if [ -n "$ROOST_BIN" ] && ! is_v3 "$ROOST_BIN"; then
  say "found $ROOST_BIN, which is not a v3 roost — fetching the release instead"
  ROOST_BIN=""
fi

if [ -z "$ROOST_BIN" ]; then
  ROOST_BIN="$(fetch_roost)"
fi
[ -n "$ROOST_BIN" ] || die "No v3 \`roost\` on this machine, and none could be fetched." \
                            "Nothing was installed." \
                            "The release assets are at https://github.com/cefege/roost/releases" \
                            "To try a local build instead, set ROOST_BIN to its path, e.g." \
                            "  ROOST_BIN=./target/release/roost"

[ -x "$ROOST_BIN" ] || die "ROOST_BIN=$ROOST_BIN is not executable." \
                            "Nothing was installed."

# The keeper is not optional the way the binary above is: `roost join` filters a
# missing one out rather than refusing, so without this the join succeeds and
# the machine serves nothing. Called after ROOST_BIN is settled, so a machine
# that already has both programs is left alone.
ensure_keeper_beside "$ROOST_BIN"

# 3. Hand the grant over. ROOST_COORDINATOR_URL / ROOST_BOOTSTRAP_TOKEN /
#    ROOST_WORKER_LABEL are already in the environment and are inherited.
#    `join` is the command that installs and registers the worker; every message
#    from it goes to this terminal, which is the operator watching.
say "roost join ($ROOST_BIN)"
exec "$ROOST_BIN" join
