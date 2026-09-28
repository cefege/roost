#!/bin/bash
# WPeer verification: baseline tests on the real tree, two mutation batches in
# a private copy (so no sibling build ever compiles a mutant), then clippy.
set -u
R=/home/almalinux/repos/roost-v3-worker
O=$R/target-track/tmp/wpeer
M=$O/mut
TESTS="--test terminal_peer_owner --test terminal_peer_packet_port --test terminal_peer_str0m --test link_downstream_direct --test direct_terminal"
rm -f "$O"/*.done "$O"/*.log
/tmp/wcargo.sh test -p roost-worker --no-fail-fast --lib $TESTS --test link_downstream_absent --test link_downstream_live --test link_downstream_local_grant > "$O/base-test.log" 2>&1
echo "exit=$?" >> "$O/base-test.log"
touch "$O/base-test.done"
for batch in A B; do
  rm -rf "$M"; mkdir -p "$M"
  (cd "$R" && cp -r Cargo.toml Cargo.lock rust-toolchain.toml clippy.toml .cargo crates xtask protocol third_party native "$M/")
  if ! python3 "$O/mutate.py" "$M" "$batch" > "$O/mutate-$batch.log" 2>&1; then
    echo "mutate failed" >> "$O/mutate-$batch.log"; continue
  fi
  /tmp/wcargo.sh test --manifest-path "$M/Cargo.toml" -p roost-worker --no-fail-fast --lib $TESTS > "$O/mut-$batch.log" 2>&1
  echo "exit=$?" >> "$O/mut-$batch.log"
done
rm -rf "$M"
touch "$O/mut.done"
/tmp/wcargo.sh clippy -p roost-worker --lib $TESTS --test link_downstream_live -- -D warnings > "$O/clippy.log" 2>&1
echo "exit=$?" >> "$O/clippy.log"
touch "$O/all.done"
