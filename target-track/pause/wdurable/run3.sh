#!/bin/bash
# WDurableP3: durable_delivery baseline with the restart-sequence test, then mutation B2.
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=/home/almalinux/repos/roost-v3-worker/target-track
unset RUSTFLAGS
cd /home/almalinux/repos/roost-v3-worker || exit 1
cargo test -p roost-worker --test durable_delivery > target-track/wdurable/run3.log 2>&1
echo "baseline exit=$?" >> target-track/wdurable/run3.log
grep -q "error\[E\|could not compile" target-track/wdurable/run3.log && exit 3
python3 target-track/wdurable/mutate.py 9 >> target-track/wdurable/run3.log 2>&1
