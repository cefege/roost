#!/bin/bash
# Builds the three self-contained commits (bun_abi cherry-pick, agent-status retirement decode,
# terminal capture protocol types) in a scratch worktree on top of HEAD and verifies roost-protocol/keeper.
set -x
M=/home/almalinux/repos/roost-v3-worker; W=$M/target-track/protowt; L=$M/target-track/protocommits.log
cd $M && git worktree remove --force $W 2>/dev/null; git worktree add -q --detach $W HEAD || exit 1
cd $W && git cherry-pick c9772f99 || { git cherry-pick --abort; echo CHERRYFAIL; exit 2; }
for f in $(cat $M/target-track/commits2/ProtoAgentRetire.paths); do mkdir -p $(dirname $f); cp $M/$f $f; done
git add -- $(cat $M/target-track/commits2/ProtoAgentRetire.paths) && git commit -q -F $M/target-track/commits2/ProtoAgentRetire.msg || exit 3
CAP="crates/roost-protocol/src/terminal_capture.rs crates/roost-protocol/src/terminal_capture crates/roost-protocol/tests/terminal_capture_envelope.rs crates/roost-protocol/tests/terminal_capture_validate.rs crates/roost-protocol/tests/terminal_capture_view.rs"
for f in $CAP; do rm -rf $f; cp -r $M/$f $f; done
git add -- $CAP && git commit -q -F $M/target-track/protocap.msg || exit 4
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=$M/target-track/protowt-target
/home/almalinux/repos/roost-build-slot cargo test -p roost-protocol -p roost-keeper --no-fail-fast 2>&1 | grep -E '^test result|FAILED|panicked|error(\[|:)' ; echo "test-exit=${PIPESTATUS[0]}"
/home/almalinux/repos/roost-build-slot cargo clippy -p roost-protocol -p roost-keeper --all-targets -- -D warnings 2>&1 | tail -3; echo "clippy-exit=${PIPESTATUS[0]}"
git log --oneline -4
