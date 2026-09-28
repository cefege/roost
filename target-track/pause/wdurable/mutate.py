#!/usr/bin/env python3
"""WDurableP3 mutation rounds. Run under the build lock (one acquisition):
flock target-track/.roost-build.lock /home/mike/repos/roost-build-slot python3 target-track/wdurable/mutate.py [round...]
Each round applies mutations whose target tests live in disjoint binaries, runs
those binaries, restores every file (also on error), and records which tests failed."""
import os, re, subprocess, sys, json

ROOT = "/home/mike/repos/roost-v3-worker"
SRC = ROOT + "/crates/roost-worker/src/"
LOG = ROOT + "/target-track/wdurable/mutations.log"

M = {
 # id: (file, old, new, binary, expected failing test)
 "B1": ("runtime/link_drain.rs", "    loop_state.sync_durable_rows().await;\n    loop_state.authorise_snapshot().await;", "    loop_state.authorise_snapshot().await;", "durable_delivery", "sink_rows_reach_the_coordinator_before_the_snapshot_and_while_live"),
 "B2": ("runtime/link_loop/durable_sync.rs", "Ok(Some(seq)) => self.pump.reassign_snapshot_sequence(seq),", "Ok(Some(_)) => {}", "durable_delivery", "a_restarted_link_numbers_its_snapshot_above_the_previous_process"),
 "B3": ("runtime/link_loop/durable.rs", "        self.durable_resync = true;\n        tracing::info!(\"the coordinator link replays from a durable outbox\");", "        tracing::info!(\"the coordinator link replays from a durable outbox\");", "durable_delivery", "a_restart_replays_the_unacknowledged_rows_before_its_snapshot"),
 "B4": ("runtime/link_loop/durable_sync.rs", "            && !self.pump.snapshot_blocked()\n", "", "durable_delivery", "the_replay_barrier_waits_for_acked_rows_and_unblocked_claims"),
 "B5": ("session/journal_sink.rs", "            if written.is_ok() {\n                delivery.note_store_changed();", "            if written.is_ok() {\n                let _ = &delivery;", "durable_delivery", "sink_rows_reach_the_coordinator_before_the_snapshot_and_while_live"),
 "W1": ("runtime/reconcile_gate.rs", "if let Err(disposed) = replay.wait_for_replay().await {", "if let Err(disposed) = Ok::<(), crate::session::durable_delivery::LinkDisposed>(()) {", "boot_admission", "the_boot_pass_reads_only_after_the_replay_and_the_snapshot_only_follows_the_pass"),
 "W2": ("runtime/boot_admission.rs", ") -> anyhow::Result<(ReconcileSummary, Readiness)> {\n", ") -> anyhow::Result<(ReconcileSummary, Readiness)> {\n    snapshot.activate();\n", "boot_admission", "the_boot_pass_reads_only_after_the_replay_and_the_snapshot_only_follows_the_pass"),
 "W3": ("runtime/reconcile_gate.rs", "        let outcome = self\n            .inner\n            .admission\n            .reference_admission\n            .run_exclusive(", "        let outcome = crate::agents::reference_admission::AgentReferenceAdmissionGate::new()\n            .run_exclusive(", "boot_admission", "a_reference_reporter_queued_during_a_pass_enters_only_after_it"),
 "W4": ("session/durable_delivery.rs", ".wait_for(|drained| *drained || self.disposed.load(Ordering::Acquire))", ".wait_for(|drained| *drained)", "boot_admission", "a_disposed_link_refuses_the_boot_pass_without_a_read"),
 "W5": ("runtime/link_loop/durable_sync.rs", "        if self.snapshot_held() {", "        if false {", "durable_delivery", "a_held_snapshot_drains_the_replay_and_publishes_only_once_activated"),
 "W6": ("runtime/link_loop/durable_sync.rs", "        if self.snapshot_wanted && self.pump.barrier() == Barrier::Snapshot {", "        if false {", "durable_delivery", "a_held_snapshot_drains_the_replay_and_publishes_only_once_activated"),
 "W7": ("link_barrier.rs", "                self.next_seq = drawn;", "                let _ = drawn;", "durable_delivery", "a_held_snapshot_drains_the_replay_and_publishes_only_once_activated"),
 "R1": ("runtime/reconcile_restore.rs", "self.keys.insert(conversation_restore_dedupe_key(reference));", "let _ = conversation_restore_dedupe_key(reference);", "agent_conversation_restore_reconcile", "an_adopted_sessions_reference_is_claimed_before_any_respawn_restore"),
 "R2": ("runtime/reconcile_restore.rs", "if AssertUnwindSafe(attempt).catch_unwind().await.is_err() {", "if { let _ = AssertUnwindSafe(()); let _ = attempt.await; false } {", "agent_conversation_restore_reconcile", "an_unexpected_restore_failure_never_escapes_the_pass"),
 "A1": ("agents/prompt_control.rs", "() = &mut grant, if !granted => granted = true,", "() = &mut grant, if false => granted = true,", "agent_prompt_fences", "a_prompt_reserves_its_receive_order_before_the_initial_scan_settles"),
 "A2": ("agents/prompt_control.rs", "            abort.abort();\n", "            let _ = &abort;\n", "agent_prompt_fences", "a_stalled_final_scan_is_aborted_and_input_released_at_budget_expiry"),
 "A3": ("agents/prompt_control.rs", "if remaining <= PROMPT_SUBMIT_DELAY {", "if false {", "agent_prompt_control", "a_budget_that_cannot_cover_the_submit_delay_rejects_without_writing"),
 "A4": ("agents/prompt_submit.rs", "    tokio::time::sleep(PROMPT_SUBMIT_DELAY).await;", "    let _ = PROMPT_SUBMIT_DELAY;", "agent_prompt_control", "the_cr_goes_out_only_after_the_settle_delay_and_is_accepted_only_when_acknowledged"),
 "A5": ("agents/prompt_fence.rs", "AgentRuntimeState::Blocked => Err(\"agent is blocked\"),", "AgentRuntimeState::Blocked => Ok(proof),", "agent_prompt_fences", "blocked_and_screen_only_status_reject_without_a_process_refresh"),
 "A6": ("agents/conversation_restore.rs", "            keys.remove(&dedupe_key);", "            let _ = &dedupe_key;", "agent_conversation_restore", "a_proven_rejection_releases_the_reference_claim_and_an_ambiguous_one_keeps_it"),
 "A7": ("agents/conversation_restore.rs", "        discard_partial_restore_input(manager, session_id, &outcome).await;", "        let _ = (manager, &outcome);", "agent_conversation_restore", "a_partly_delivered_resume_command_is_discarded_from_the_prompt"),
 "A8": ("agents/reference_admission.rs", "        Err(error) => {\n            sink.release(reservation).await;\n", "        Err(error) => {\n", "agent_reference_admission", "a_failed_append_gives_its_claim_back"),
 "A9": ("agents/reference_admission.rs", "        let _turn = self.turn.lock().await;", "        let _turn = ();", "agent_reference_admission", "a_later_turn_waits_for_the_one_holding_the_gate_and_runs_in_arrival_order"),
 "A10": ("runtime/downstream/agent_prompt.rs", "                            replies::AGENT_PROMPT_HANDLER_FAILED,", "                            \"worker agent prompt handler is unavailable\",", "link_downstream_agent_prompt", "an_owner_failure_answers_a_static_ambiguous_result_and_logs_none_of_it"),
}

ROUNDS = {
 "1": ["B1", "W1", "R1", "A1", "A3", "A6", "A8", "A10"],
 "2": ["B2", "W2", "R2", "A2", "A4", "A7", "A9"],
 "3": ["B3", "W3", "A5"],
 "4": ["B4", "W4"],
 "5": ["B5"],
 "6": ["W5"],
 "7": ["W6"],
 "8": ["W7"],
 "9": ["B2"],
}

def log(line):
    with open(LOG, "a") as out:
        out.write(line + "\n")
    print(line, flush=True)

def run_round(name):
    ids = ROUNDS[name]
    originals = {}
    try:
        for mid in ids:
            path, old, new, _, _ = M[mid]
            full = SRC + path
            text = originals.get(full) or open(full).read()
            originals.setdefault(full, text)
            current = open(full).read()
            if current.count(old) != 1:
                log(f"{mid}: SKIPPED (pattern count {current.count(old)} in {path})")
                continue
            open(full, "w").write(current.replace(old, new, 1))
        binaries = sorted({M[mid][3] for mid in ids})
        cmd = ["cargo", "test", "-p", "roost-worker", "--no-fail-fast"]
        for binary in binaries:
            cmd += ["--test", binary]
        proc = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, timeout=3000)
        out = proc.stdout + proc.stderr
        open(f"{ROOT}/target-track/wdurable/round{name}.out", "w").write(out)
        failed = set(re.findall(r"^test (\S+) \.\.\. FAILED", out, re.M))
        compile_error = "error[E" in out or "could not compile" in out
        for mid in ids:
            path, _, _, binary, test = M[mid]
            verdict = "FAILED (guard holds)" if test in failed else ("COMPILE ERROR" if compile_error else "PASSED (guard missing)")
            log(f"{mid} {path} -> {binary}::{test}: {verdict}")
        log(f"round {name} failed tests: {sorted(failed)}")
    finally:
        for full, text in originals.items():
            open(full, "w").write(text)
        log(f"round {name} restored {len(originals)} files")

if __name__ == "__main__":
    os.environ["PATH"] = os.path.expanduser("~/.cargo/bin") + ":" + os.environ["PATH"]
    os.environ.update(CARGO_INCREMENTAL="0", CARGO_BUILD_JOBS="3", CARGO_TARGET_DIR=ROOT + "/target-track")
    os.environ.pop("RUSTFLAGS", None)
    for name in (sys.argv[1:] or sorted(ROUNDS)):
        run_round(name)
