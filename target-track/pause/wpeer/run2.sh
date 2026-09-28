#!/bin/bash
# WPeer verification, second pass: the split test files on the real tree,
# mutation batch B in a private copy, then clippy on the real tree.
set -u
R=/home/mike/repos/roost-v3-worker
O=$R/target-track/tmp/wpeer
M=$O/mut
TESTS="--test terminal_peer_owner --test terminal_peer_offer_faults --test terminal_peer_packet_port --test terminal_peer_packet_ingress --test terminal_peer_str0m --test link_downstream_direct --test direct_terminal"
/home/mike/wl/wcargo.sh test -p roost-worker --no-fail-fast --lib $TESTS --test link_downstream_live > "$O/base2-test.log" 2>&1
echo "exit=$?" >> "$O/base2-test.log"
rm -rf "$M"; mkdir -p "$M"; touch "$O/copy.marker"
(cd "$R" && cp -rp Cargo.toml Cargo.lock rust-toolchain.toml clippy.toml .cargo crates xtask protocol third_party native "$M/")
# A distinct version gives the mutant roost-worker its own artifact hash, so it
# never overwrites the real tree's; -p keeps every other crate's mtime, so the
# shared artifacts of unchanged crates stay fresh instead of being rebuilt.
sed -i 's/^version.workspace = true$/version = "3.0.0-wpeer-mutant"/' "$M/crates/roost-worker/Cargo.toml"
grep -q '3.0.0-wpeer-mutant' "$M/crates/roost-worker/Cargo.toml" || { echo "no version bump" > "$O/mutate-B.log"; exit 1; }
if python3 "$O/mutate.py" "$M" B > "$O/mutate-B.log" 2>&1; then
  /home/mike/wl/wcargo.sh test --manifest-path "$M/Cargo.toml" -p roost-worker --no-fail-fast --lib $TESTS > "$O/mut-B.log" 2>&1
  echo "exit=$?" >> "$O/mut-B.log"
fi
rm -rf "$M"
# A crate edited while the mutant built may now look older than its artifact.
(cd "$R" && find crates -name "*.rs" -newer "$O/copy.marker" -exec touch {} +)
/home/mike/wl/wcargo.sh clippy -p roost-worker --lib $TESTS --test link_downstream_live -- -D warnings > "$O/clippy.log" 2>&1
echo "exit=$?" >> "$O/clippy.log"
touch "$O/all2.done"
