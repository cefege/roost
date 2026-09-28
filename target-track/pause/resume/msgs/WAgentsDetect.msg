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

Mutations: the 12 planned mutants (target-track/pause/wad/run.py MUT table: M1-M12 over
registry_recompute, registry, stable_detection, detector/scan, process_scan, link_drain and
link_loop/agent_status) and the protocol P1 are NOT RUN yet — open item in the handoff.

Deviations/parity notes: every status queues and drains on the next wake (no
socket direct-write; single-item compaction equals v2's direct write); screen
stability is stamped from the injected clock's wall ms; v2's diag
transport.frame_dropped is a tracing warn; win32 branches not ported.
Cross-owner edits: none.

Verification at commit: every test in the series tip passes (track gate, two runs);
link_agent_status::successful_retirements_replay_in_order_without_an_active_snapshot was a
test race (published before the worker took the snapshot barrier live; v2
coord-link-agent-status.test.ts:203-217 acks synchronously) — go_live_at now waits for the
worker's on_snapshot_ready; no assert changed.

Series: one of the wave 2+3 per-slice commits (ProtoDoor..WAgentsPrompt). A file shared by
several slices lands whole in the FIRST slice commit that lists it, so intermediate commits
may not build; the track gate (tests x2, clippy, lint, fmt) ran on the series tip.
