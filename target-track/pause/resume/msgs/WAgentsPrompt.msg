worker: agent prompts, conversation references and OMP resume restore

Ports v2 apps/worker/src/agents/agent-prompt-control.ts (validateRequest,
checkBudget, statusFenceFailure, processProofMatches, refreshProcessProof,
waitForAdmissionGrant, writeAgentPrompt), agent-prompt-submit.ts,
reference-admission.ts, agent-conversation-restore.ts,
boot/boot-session-reconcile.ts _assertExactRecoveryMetadata,
transport/coord-link-deps.ts onAgentPrompt, the agentPrompt case of
transport/coord-link-downstream.ts, and host/config.ts
ROOST_AGENT_CONVERSATION_RESTORE.

A prompt passes the epoch/occupant/revision fence, a process proof refreshed
twice (aborted at budget expiry), the pane-foreground check and the budget
gates, then writes its text and, 300 ms later, the CR on ONE held keeper-input
lane (session::input_write::HeldInputLane) taken before the first scan, so
receive order is kept and there is no second writer. agent_reference events
enter the durable outbox as DurableEventKind::AgentReference through
emit_durable_agent_reference, under the one AgentReferenceAdmissionGate
(built by runtime::agent_owners, shared via AgentStatusStack).

Replaced downstream arm: agentPrompt. Old reply: input-result REJECTED /
PRE_WRITE "worker agent prompt handler is unavailable" for every prompt. New
owner: agents::prompt_port::AgentPromptOwner (DownstreamOwners.agent_prompt,
built in runtime::owners over the agent-status stack and the shared input work
budget). The old reply stays only when the dispatcher has no owners, as v2. An
owner failure answers AMBIGUOUS "worker agent prompt handler failed" and logs
agent_prompt_failed without the failure text.

Consumers: AgentPromptPort -> runtime/downstream/agent_prompt.rs;
HeldInputLane -> prompt_control/prompt_submit; worker_input_result ->
terminal_input/port.rs + prompt_port.rs; SessionManager::durable_event_sink ->
runtime/agent_owners.rs (report server, detector clear);
DurableEventKind::AgentReference -> emit_durable_agent_reference;
WorkerBoot.agent_conversation_restore / conversation_restore /
conversation_recovery -> the reconcile pass, wired in the WBootOrder commit
(runtime::reconcile_restore, runtime::reconcile::read_open_sessions,
ReconcileGate::run under the reference admission gate).

Mutations A1-A10 (slice run2): each named product line mutated, the named test failed,
reverted (per-mutant list lost with the old host's report; verdict recorded in
target-track/pause/wdurable/MUTATIONS.md: "A1-A10 -> every named test FAILED").

Series: one of the wave 2+3 per-slice commits (ProtoDoor..WAgentsPrompt). A file shared by
several slices lands whole in the FIRST slice commit that lists it, so intermediate commits
may not build; the track gate (tests x2, clippy, lint, fmt) ran on the series tip.
