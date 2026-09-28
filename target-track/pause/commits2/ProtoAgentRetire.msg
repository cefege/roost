protocol: the worker link admits an inactive agent status as its occupant's retirement

`coord_worker_proto::records::agent_status_from_proto` ran every decoded
agent status through `AgentStatus::parse`, which refuses `active: false`, so
`decode_upstream` failed on every retirement the worker publishes (the
registry's occupant_inactive/exit frames and the outbox's replayed retirement
history). A Rust coordinator would drop them and strand the row.

v2 parity: the coordinator folds every update and parses the full status only
when it is active (apps/coord/src/agents/agent-status-hub.ts:136-140 — an
inactive update deletes the session entry; ordering in
agent-status-order.ts:45-55). Now: an active status is still parsed as the
retained row (`AgentStatus::parse`); an inactive one is bounded by the common
checks (`AgentStatusUpdate::parse`) and carried as `AgentStatusFrame` with
`status.active == false` (doc on the frame says so). roost-coord's
worker_link/live_frames.rs already turns the frame into an AgentStatusUpdate,
so no consumer changes.

Test: crates/roost-protocol/tests/agent_status_retirement_proto.rs — an
inactive status round-trips as a retirement; a retirement past its bounds
(completed_revision > revision) is still refused; an active status is
unchanged.
Mutation (NOT YET OBSERVED — run killed by the stop order): records.rs — restore `AgentStatus::parse(value)` for every status →
`an_inactive_status_decodes_as_its_occupants_retirement` fails ("must be true;
an inactive status is an AgentStatusUpdate"); reverted.
