#!/usr/bin/env bash
# Reproducibly rebuild apps/shared/wasm/wterm-roost.wasm from upstream wterm
# sources plus Roost's checked-in scrollback and resize changes.
#
# Roost runs a PATCHED @wterm/core WASM: upstream caps alt-screen scrollback at
# 1,000 lines while the SPA renders 10,000, and upstream resize updates only
# the active grid, leaving the saved primary grid at stale dimensions while
# alternate screen is active. The audited patch changes only src/scrollback.zig
# and src/terminal.zig. This script proves an unmodified build of the pinned
# upstream commit reproduces upstream's own committed wterm.wasm byte-for-byte,
# then applies the patch and rebuilds.
#
# TOOLCHAIN PREREQUISITES (all mandatory — the script fails loudly, it NEVER
# falls back to the prebuilt binary):
#   * zig 0.16.0 exactly, on PATH or via $ZIG.
#       curl -fsSLO https://ziglang.org/download/0.16.0/zig-x86_64-linux-0.16.0.tar.xz
#       # sha256 70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00
#       tar xf zig-x86_64-linux-0.16.0.tar.xz
#       export PATH="$PWD/zig-x86_64-linux-0.16.0:$PATH"
#   * git with network access to https://github.com/vercel-labs/wterm
#   * sha256sum, cmp (coreutils/diffutils)
#
# Usage:
#   scripts/rebuild-wterm-wasm.sh            # rebuild + install artifact and digest
#   scripts/rebuild-wterm-wasm.sh --verify   # CI gate: rebuild, compare, touch nothing
#
# $WTERM_SRC_CACHE (optional) reuses a checkout/build directory across runs.

set -euo pipefail

# ── pinned inputs ───────────────────────────────────────────────────────────
readonly UPSTREAM_REPO="https://github.com/vercel-labs/wterm.git"
readonly UPSTREAM_COMMIT="4a73024d9f9003972f9efa6fe1a9086d1c90417b" # tag v0.3.4
readonly ZIG_VERSION="0.16.0"
# Upstream's own committed ReleaseSmall artifact at $UPSTREAM_COMMIT. An
# unmodified build must reproduce this exactly, or the toolchain/source is not
# what we think it is and the patched output means nothing.
readonly UPSTREAM_WASM_PATH="packages/@wterm/core/wasm/wterm.wasm"
readonly UPSTREAM_WASM_SHA256="dab230ac368e4bdaa16fdbb2e3844bae530d98bc78a15ccdc2f86269dc1845f4"
readonly PATCH_REL="scripts/wterm-0.3.4-roost.patch"
readonly ARTIFACT_REL="apps/shared/wasm/wterm-roost.wasm"
readonly DIGEST_REL="apps/shared/wasm/wterm-roost.wasm.sha256"

readonly REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

verify_only=0
case "${1-}" in
  "") ;;
  --verify) verify_only=1 ;;
  *) echo "rebuild-wterm-wasm: unknown argument '$1' (expected nothing or --verify)" >&2; exit 2 ;;
esac

die() { echo "rebuild-wterm-wasm: $*" >&2; exit 1; }
step() { echo "==> $*"; }

# ── prerequisite gate: explicit, before any work ────────────────────────────
for tool in git sha256sum cmp; do
  command -v "$tool" >/dev/null 2>&1 || die "missing required tool '$tool'. See the TOOLCHAIN PREREQUISITES header."
done

zig_bin="${ZIG:-zig}"
command -v "$zig_bin" >/dev/null 2>&1 || die "zig $ZIG_VERSION is required and was not found (tried '$zig_bin'). Install it as shown in the TOOLCHAIN PREREQUISITES header, or set \$ZIG. This script never falls back to the prebuilt $ARTIFACT_REL."
zig_have="$("$zig_bin" version)"
[ "$zig_have" = "$ZIG_VERSION" ] || die "zig $ZIG_VERSION is required, found $zig_have at '$(command -v "$zig_bin")'. The artifact digest is toolchain-exact; a different zig produces a different binary."

[ -f "$REPO_ROOT/$PATCH_REL" ] || die "missing $PATCH_REL — the checked-in Roost changes are part of the build input."

# ── source checkout ─────────────────────────────────────────────────────────
if [ -n "${WTERM_SRC_CACHE-}" ]; then
  src="$WTERM_SRC_CACHE"
  mkdir -p "$src"
else
  src="$(mktemp -d "${TMPDIR:-/tmp}/wterm-src.XXXXXX")"
  trap 'rm -rf "$src"' EXIT
fi

if [ -d "$src/.git" ]; then
  step "reusing checkout $src"
else
  step "cloning $UPSTREAM_REPO"
  git clone --filter=blob:none --no-checkout "$UPSTREAM_REPO" "$src" >/dev/null
fi
step "fetching pinned commit $UPSTREAM_COMMIT"
git -C "$src" fetch --depth 1 origin "$UPSTREAM_COMMIT" >/dev/null 2>&1 \
  || die "could not fetch $UPSTREAM_COMMIT from $UPSTREAM_REPO"
git -C "$src" -c advice.detachedHead=false checkout --force "$UPSTREAM_COMMIT" >/dev/null
git -C "$src" clean -fdx -e zig-out -e .zig-cache >/dev/null
have_commit="$(git -C "$src" rev-parse HEAD)"
[ "$have_commit" = "$UPSTREAM_COMMIT" ] || die "checkout is at $have_commit, expected $UPSTREAM_COMMIT"

# Upstream's committed artifact must itself be the byte sequence we pinned;
# otherwise the "stock build reproduces upstream" proof below is circular.
committed_stock="$(sha256sum "$src/$UPSTREAM_WASM_PATH" | cut -d' ' -f1)"
[ "$committed_stock" = "$UPSTREAM_WASM_SHA256" ] \
  || die "upstream's committed $UPSTREAM_WASM_PATH is $committed_stock, pinned $UPSTREAM_WASM_SHA256"

build() { # $1 = label
  step "zig build -Doptimize=ReleaseSmall ($1)"
  rm -rf "$src/zig-out"
  ( cd "$src" && "$zig_bin" build -Doptimize=ReleaseSmall )
  [ -f "$src/zig-out/bin/wterm.wasm" ] || die "$1 build produced no zig-out/bin/wterm.wasm"
}

# ── proof 1: unmodified sources reproduce upstream's committed binary ───────
build "stock"
cmp -s "$src/zig-out/bin/wterm.wasm" "$src/$UPSTREAM_WASM_PATH" || die \
  "stock build does NOT reproduce upstream's committed $UPSTREAM_WASM_PATH
     built:    $(sha256sum "$src/zig-out/bin/wterm.wasm" | cut -d' ' -f1)
     upstream: $UPSTREAM_WASM_SHA256
   The toolchain or sources differ from the pinned pair; refusing to derive a patched artifact."
step "stock build reproduces upstream $UPSTREAM_WASM_PATH exactly ($UPSTREAM_WASM_SHA256)"

# ── proof 2: apply the audited changes, rebuild ──────────────────────────────
step "applying $PATCH_REL"
git -C "$src" apply --whitespace=nowarn "$REPO_ROOT/$PATCH_REL" \
  || die "$PATCH_REL does not apply to $UPSTREAM_COMMIT — rebase the patch before rebuilding."
changed="$(git -C "$src" diff --name-only)"
[ "$changed" = "$(printf '%s\n' src/scrollback.zig src/terminal.zig)" ] \
  || die "patch touches unexpected files (expected src/scrollback.zig and src/terminal.zig):
$changed"
build "patched"

built_sha="$(sha256sum "$src/zig-out/bin/wterm.wasm" | cut -d' ' -f1)"
step "patched artifact $built_sha ($(wc -c < "$src/zig-out/bin/wterm.wasm") bytes)"

# ── install or verify ───────────────────────────────────────────────────────
if [ "$verify_only" = 1 ]; then
  [ -f "$REPO_ROOT/$ARTIFACT_REL" ] || die "$ARTIFACT_REL is missing; nothing to verify against."
  cmp -s "$src/zig-out/bin/wterm.wasm" "$REPO_ROOT/$ARTIFACT_REL" || die \
    "committed $ARTIFACT_REL does not match a reproducible build
     committed: $(sha256sum "$REPO_ROOT/$ARTIFACT_REL" | cut -d' ' -f1)
     rebuilt:   $built_sha
   Re-run scripts/rebuild-wterm-wasm.sh and commit the result."
  ( cd "$REPO_ROOT/$(dirname "$DIGEST_REL")" && sha256sum -c "$(basename "$DIGEST_REL")" >/dev/null ) \
    || die "$DIGEST_REL does not match $ARTIFACT_REL"
  step "verified: $ARTIFACT_REL and $DIGEST_REL match a reproducible build of $UPSTREAM_COMMIT + $PATCH_REL"
  exit 0
fi

# Redirect rather than `cp`: the tracked artifact keeps its recorded file mode,
# so a rebuild is a pure content change in the diff.
cat "$src/zig-out/bin/wterm.wasm" > "$REPO_ROOT/$ARTIFACT_REL"
( cd "$REPO_ROOT/$(dirname "$ARTIFACT_REL")" && sha256sum "$(basename "$ARTIFACT_REL")" > "$(basename "$DIGEST_REL")" )
step "wrote $ARTIFACT_REL and $DIGEST_REL"
echo
echo "Next: bun scripts/gen-embed.ts   # re-bake the compiled-binary embed"
