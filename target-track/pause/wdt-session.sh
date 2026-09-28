#!/bin/bash
# WDoorTerm: one build-lock hold for tests, mutations and clippy. Output under /tmp/wdt-s.*
cd /home/mike/repos/roost-v3-worker || exit 1
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=/home/mike/repos/roost-v3-worker/target-track
unset RUSTFLAGS
cargo test -p roost-worker --no-fail-fast \
  --test local_terminal_grants --test local_terminal_prehello --test local_terminal_socket \
  --test local_terminal_socket_input --test local_terminal_peer_socket --test local_terminal_scrollback \
  --test link_downstream_local_grant --test link_downstream_absent --test scrollback_read \
  --test local_terminal_pty > /tmp/wdt-s.tests 2>&1
echo "tests exit=$?" >> /tmp/wdt-s.tests
python3 /tmp/wdt-mut.py /tmp/wdt-muts.json /tmp/wdt-s.muts
cargo clippy -p roost-worker --all-targets --message-format short -- -D warnings > /tmp/wdt-s.clippy 2>&1
echo "clippy exit=$?" >> /tmp/wdt-s.clippy
