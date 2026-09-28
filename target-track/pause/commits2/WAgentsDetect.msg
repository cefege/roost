worker: agent-status detection — registry, process scan, screen detector and the link's retirement-history outbox

Ports v2 apps/worker/src:
- agents/occupancy.ts -> agent_occupancy.rs (replaced wholesale: the old file
  modelled a multi-occupant observe/lose/acknowledge API v2 does not have;
  tests/agent_occupancy.rs and tests/agent_screen_signal.rs pinned that non-v2
  API and are deleted — pid recycling, exit vs withdrawal and the visible
  blocker are covered by the ported registry tests)
- agents/registry.ts -> agents/registry.rs + registry_recompute.rs
- agents/stable-detection.ts -> agents/stable_detection.rs
- agents/process-tree.ts -> agents/process_tree.rs
- agents/process-scan.ts (POSIX) -> agents/process_identity.rs,
  process_snapshot.rs, process_scan.rs
- agents/peer-process-id.ts (POSIX) -> agents/peer_process_id.rs
  (tokio peer_cred; unsafe_code is forbidden)
- agents/detector.ts -> agents/detector.rs + detector/{scan,reference,sessions}.rs
- transport/coord-link-agent-status.ts -> runtime/link_loop/agent_status.rs
  (retirement history, compaction, repair replay; drained after the durable
  rows and the writable notification, ahead of the control lane — v2
  drainQueues order). v2 Map.set keeps a re-queued session's drain position;
  so does SessionQueues::put.
- session-manager-state.ts setAgentStatusHooks.terminalChanged (called at
  session-emit.ts:115 for every chunk on every lane) -> session/terminal_changed.rs,
  notified by runtime/channel_delivery.rs
- composition main.ts:220-237 + coord-link-deps.ts onSnapshotReady ->
  agentRegistry.resend() -> agents/status_stack.rs + runtime/agent_owners.rs;
  owners.rs starts it before the link's session-closed hook (v2 order:
  detector.closeSession first) and fans the lifecycle through the new
  link_ports::LinkLifecycles (existing direct/cadence lifecycle, then the
  agent stack). WorkerOwners::build now returns anyhow::Result (manifest
  compile / epoch mint); boot_sequence propagates it.

Cutover: LinkLoop::send_agent_status moved from volatile.rs (control-lane
coalescing under AGENT_STATUS_LABEL) to the agent-status outbox; detach now
clears the whole control lane (v2 detachSocket) and calls
agent_statuses.disconnect(); Outbox::retain lost its only caller and is removed.

Consumers: AgentStatusOutbox <- LinkLoop/link_drain; TerminalChangedHooks <-
channel_delivery (notify) / status_stack (install); LinkLifecycles <- owners.rs
(DownstreamOwners.lifecycle); AgentStatusStack {registry, detector, manifests,
reference_admission} <- owners.rs, report server, prompt owner; ScanAbort <-
prompt control; LocalPeerProcessIdReader <- report server.

Mutations (each seen red, then reverted): see the slice report.

Deviations/parity notes: every status queues and drains on the next wake (no
socket direct-write; single-item compaction equals v2's direct write); screen
stability is stamped from the injected clock's wall ms; v2's diag
transport.frame_dropped is a tracing warn; win32 branches not ported.
Cross-owner edits: none.

PAUSED STATE (lead stop order): verified — lib + all 11 test targets compile;
passing: agent_process_identity 9/9, agent_status_arbitration 6/6,
exit_completion 5/5, registry_identity 8/8, stable_transitions 6/6,
screen_gate 6/6, osc_transition 5/5, reference_clear 4/4, peer_process_id 3/3,
link_agent_status 5/6 (incl. new a_session_requeued_while_pending_keeps_its_drain_position),
agent_status_link_e2e 1/2. UNVERIFIED: the v2 direct-write emulation in
runtime/link_loop/agent_status.rs (AgentStatusOutbox::send / direct FIFO /
LinkLoop::agent_status_can_write_direct) added to fix
successful_retirements_replay_in_order_without_an_active_snapshot and the e2e
frame test — not yet compiled or run; all 12 planned mutations NOT yet run
(anchors in target-track/wad/run.py MUT table); clippy not run.
