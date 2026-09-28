#!/bin/bash
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=/home/mike/repos/roost-v3-worker/target-track
unset RUSTFLAGS
cd /home/mike/repos/roost-v3-worker || exit 1
WAD_CARGO=cargo python3 /tmp/wad-mutate.py > /tmp/wad-mutate.log 2>&1
{ cargo test -p roost-worker --test attachment_direct_socket --test attachment_peer_owner --test attachment_peer_port --test attachment_loopback_upload --test attachment_peer_upload; cargo test -p roost-protocol --test attachment_transfer_packets; echo DONE; } > /tmp/wad-test2.log 2>&1
cargo clippy -p roost-worker -p roost-protocol --all-targets -- -D warnings > /tmp/wad-clippy.log 2>&1; echo DONE >> /tmp/wad-clippy.log
