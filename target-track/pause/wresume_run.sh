#!/bin/bash
# WResume P2: worker tests, keeper + worker mutants, clippy — one queued run.
set -u
R=/home/almalinux/repos/roost-v3-worker
M=/tmp/wresume_mut.sh
echo "## worker tests"
/tmp/wcargo.sh test -p roost-worker --test boot_adoption_gate --test keeper_survivor_adoption \
  --test keeper_pool_channels --test session_adoption --test open_session_set --test boot_keeper \
  --test worker_keeper_admission --test terminal_pipeline_bounds --test session_resume_teardown \
  --test query_reply_replay_align --test terminal_stream_state --test terminal_stream_core_trap \
  --test reconcile_gate --test session_spawn --test session_binding --test channel_creation_gate 2>&1 \
  | grep -E "^test .*(ok|FAILED|ignored)$|test result|panicked at|^error|^  --> |Running tests" | grep -v "\.\.\. ok$"
echo "## keeper mutants"
$M roost-keeper keeper_queries pre_boundary_output_is_dropped_exactly_once crates/roost-keeper/src/client_history.rs \
  "                    dropped += frame.payload.len();" "                    self.defer(frame);"
$M roost-keeper channel_history an_evicted_marker_becomes_the_base_geometry crates/roost-keeper/src/channel_history.rs \
  "            self.base_cols = evicted.cols;
            self.base_rows = evicted.rows;
" ""
$M roost-keeper keeper_queries the_ordered_history_is_the_resize_the_keeper_recorded crates/roost-keeper/src/keeper_ops.rs \
  "if request.seq > before && state.applied_seq == request.seq {" "if false {"
echo "## worker mutants (six independent mutants in one build; each named test must fail)"
W=$R/crates/roost-worker/src
declare -a F=(keeper_pool/pool_lifecycle.rs session/respawn_replace.rs boot_keeper.rs runtime/reconcile_gate.rs terminal_pipeline/mod.rs session/resume.rs)
for f in "${F[@]}"; do cp "$W/$f" "/tmp/wresume_$(echo $f | tr / _).bak"; done
python3 - "$W" <<'EOF'
import sys
w = sys.argv[1]
muts = [
 ("keeper_pool/pool_lifecycle.rs", "channel.output().on_exit(None);", "channel.output().on_error(reason.clone());"),
 ("session/respawn_replace.rs", "if let Err(fault) = self.keeper.kill_channel(raw) {", "if let Err(fault) = Ok::<(), super::keeper_channels::KeeperFault>(()) {"),
 ("boot_keeper.rs", "if probe.authenticated && probe.protocol_compatible {", "if probe.authenticated && probe.protocol_compatible && probe.exact_target {"),
 ("runtime/reconcile_gate.rs", "if state.keeper_restarts.len() >= KEEPER_RESTART_BUDGET {", "if state.keeper_restarts.len() > KEEPER_RESTART_BUDGET {"),
 ("terminal_pipeline/mod.rs", "        facts.resize_frames += 1;\n", ""),
 ("session/resume.rs", "if let Err(fault) = self.keeper.kill_channel(channel) {", "if let Err(fault) = Ok::<(), super::keeper_channels::KeeperFault>(()) {"),
]
for f, old, new in muts:
    p = w + "/" + f
    s = open(p).read()
    print(("APPLIED " if old in s else "ANCHOR MISSING ") + f)
    open(p, "w").write(s.replace(old, new, 1))
EOF
/tmp/wcargo.sh test -p roost-worker --test keeper_pool_channels --test boot_adoption_gate --test boot_keeper \
  --test reconcile_gate --test terminal_pipeline_bounds --test session_resume_teardown 2>&1 \
  | grep -E "^test .*(ok|FAILED)$|test result|^error|Running tests"
for f in "${F[@]}"; do cp "/tmp/wresume_$(echo $f | tr / _).bak" "$W/$f"; done
echo "restored: $(cd $R && git diff --stat -- crates/roost-worker/src/boot_keeper.rs | tail -1)"
echo "## clippy"
/tmp/wcargo.sh clippy -p roost-worker -p roost-keeper --all-targets -- -D warnings 2>&1 \
  | grep -E "^(error|warning)|^  --> " | paste - - | grep -E "roost-keeper/|keeper_pool/(pool|pool_history|pool_lifecycle|pending_resizes|session_seam|dispatch|error)|session/(resume|resume_core|respawn|respawn_replace|keeper_health|binding|binding_close|table|spawn|lifecycle|keeper_channels)\.rs|runtime/(keeper_boot|keeper_prepare|keeper_retire|reconcile|reconcile_gate|reconcile_claim|session_reconcile|stop)\.rs|boot_keeper|session_lifecycle|terminal_pipeline|tests/(boot_adoption_gate|keeper_survivor_adoption|keeper_pool_channels|session_adoption|open_session_set|boot_keeper|worker_keeper_admission|terminal_pipeline_bounds|session_resume_teardown|reconcile_gate)" | head -40
echo "## done"
