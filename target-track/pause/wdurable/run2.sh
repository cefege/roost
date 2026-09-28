#!/bin/bash
# WDurableP3: baseline of the touched suites, then the mutation rounds, in one build-lock hold.
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=/home/almalinux/repos/roost-v3-worker/target-track
unset RUSTFLAGS
cd /home/almalinux/repos/roost-v3-worker || exit 1
avail=$(df --output=avail -B1G /home | tail -1 | tr -d ' ')
if [ "$avail" -lt 10 ]; then echo "DISK LOW" >&2; exit 97; fi
cargo test -p roost-worker --no-fail-fast \
  --test durable_delivery --test boot_admission --test agent_conversation_restore_reconcile \
  --test reconcile_gate --test worker_retire_authorization --test worker_boot_order \
  --test link_agent_status --test link_downstream_absent --test link_downstream_live \
  --test link_downstream_agent_prompt --test worker_reconnect_ladder --test worker_shutdown_boundary \
  --test link_barrier --test agent_prompt_fences --test agent_prompt_control \
  --test agent_conversation_restore --test agent_reference_admission --test worker_boot_config \
  > target-track/wdurable/run2.log 2>&1
echo "baseline exit=$?" >> target-track/wdurable/run2.log
if grep -q "error\[E\|could not compile" target-track/wdurable/run2.log; then exit 3; fi
python3 target-track/wdurable/mutate.py > target-track/wdurable/mutate.out 2>&1
echo "mutations exit=$?" >> target-track/wdurable/mutate.out
