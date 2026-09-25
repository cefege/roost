<!-- Agent metadata contract: private durable OMP recovery references and volatile PID-free status observations. -->
<!-- Neither surface creates or controls a structured agent session; both describe the ordinary shell PTY. -->
<!-- Canonical limits live in @roost/protocol/agent-conversation-reference, wire, and terminal-input. -->

# Agent metadata

## Purpose

Roost carries two deliberately separate agent-adjacent contracts. `agent_reference` is private durable equality/continuation metadata used only for one fenced OMP recovery input. Agent status is volatile worker observation (`idle|working|blocked`) used for dashboard state, waits, notifications, and one identity-fenced PTY input. Neither is an agent execution API.

## Messages

| Message | File | Meaning |
| --- | --- | --- |
| `AgentConversationReferenceV1`, `AgentReferenceEvt` | `protocol/proto/roost/v1/wire.proto`; `protocol/proto/roost/v1/events.proto:107-112` | Private versioned opaque `omp` id/path and durable set/clear event. |
| `SessionRecoveryMetadata`, `SessionsListResponse.recovery_metadata` | `protocol/proto/roost/v1/coordinator.proto:125-137` | Worker-only private recovery row. Never returned to browsers. |
| `WAgentStatus` | `protocol/proto/roost/v1/worker_transport.proto:66-79` | Worker-originated volatile observation. |
| `AgentStatusFrame` | `protocol/proto/roost/v1/sync.proto:83-96` | Same observation fanned to browser Sync. |
| `AgentStatusView`, `AgentStatusGet/List/Wait` messages | `protocol/proto/roost/v1/coordinator.proto:412-440` | PID-free RPC projection plus coordinator-derived `promptable`. |
| `SessionsPromptRequest/Response`, `DAgentPrompt`, `WInputResult` | `protocol/proto/roost/v1/coordinator.proto`; `protocol/proto/roost/v1/worker_transport.proto` | Exact status-identity fence around one ordinary PTY input, with separate input/wait outcomes. |

## State machine

**Private conversation reference**

1. An official integration reports an opaque value through a separate acknowledged worker-local method. Worker revalidation covers session capability, kernel-attested peer PID, and fresh agent-process ancestry; integration data cannot select provider/executable/template.
2. An official absolute `session_file` is stored as `kind=path`; otherwise the official `session_id` from the same call is stored as `kind=id`. Accepted set/replacement/clear appends durable `SessionEvent(agent_reference)` ordered by worker `client_seq`.
3. Coordinator folds only strictly newer same-session sequences into private recovery metadata. The value is absent from public `Session`, Sync, browser/CLI lists, search, logs, audit, and diagnostics. Session close deletes it.
4. Worker-authored agent exit emits exactly one durable clear while the session remains live. On involuntary loss, keeper adoption runs first; successful adoption sends zero resume input. Only after adoption failure, replacement-shell creation, and admitted `respawned` may the fixed OMP descriptor issue one `omp --resume=<opaque>` input batch.
5. A claimed reference cannot resume twice in one reconciliation. Rejection proven before any keeper byte releases the claim. Accepted/rejected/ambiguous outcomes are terminal for that boot attempt; a nonaccepted result reporting written bytes is followed by one `0x03` discard byte, never a second resume.

**Volatile status and prompt**

1. Worker process scanning, integration reports, and screen/title observation produce one effective row per session. `status_epoch` identifies registry lifetime; `occupant_id` identifies a verified process incarnation; `source` is `integration|screen`; revisions are monotonic within that identity.
2. An integration observation beats screen. A silent integration lease expires and the worker falls back automatically. Only an identified integration row is `promptable`; screen and identityless legacy rows remain readable.
3. Authenticated worker frames enter the coordinator's in-memory hub. A new active identity replaces the old one; retired identities remain fenced. `active:false` removes retained status. Session close removes the row and resolves waiters `session_closed`. Sync seeds its current hub snapshot; nothing survives process restart.
4. `AgentStatusWait` registers before inspecting current state, pins exact epoch/occupant, and resolves only on real state progress, timeout, occupant replacement, or close. Revision/message republish at the same state does not satisfy a wait.
5. `SessionsPrompt` requires exact epoch, occupant, and revision plus a current idle/working integration state. Coordinator registers its optional waiter, sends `DAgentPrompt`, and consumes the waiter only after definite rejection or after accepted/ambiguous input. `WInputResult` remains write truth; ambiguous input is never retried.

## Limits

| Constant | Value | TypeScript source |
| --- | ---: | --- |
| `AGENT_CONVERSATION_SESSION_ID_MAX_UTF8_BYTES` | `512` | `packages/protocol/src/agent-conversation-reference.ts:11` |
| `AGENT_CONVERSATION_SESSION_PATH_MAX_UTF8_BYTES` | `4,096` | `packages/protocol/src/agent-conversation-reference.ts:12` |
| `AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES` | `8,192` | `packages/protocol/src/agent-conversation-reference.ts:16` |
| `AGENT_ID_MAX_LENGTH` | `32` | `packages/protocol/src/wire/agent-status.ts:8` |
| `AGENT_STATUS_MESSAGE_MAX_LENGTH` | `512` | `packages/protocol/src/wire/agent-status.ts:9` |
| `INTEGRATION_LEASE_MS` | `30,000 ms` | `apps/worker/src/agents/stable-detection.ts` |
| `AGENT_STATUS_WAIT_MAX_TIMEOUT_MS` | `300,000 ms` | `apps/coord/src/agents/agent-status-wait.ts:81` |
| `AGENT_STATUS_WAIT_MAX_PER_SESSION` / global | `32 / 2,048` | `apps/coord/src/agents/agent-status-wait.ts:82-83` |
| `AGENT_PROMPT_MAX_TEXT_BYTES` | `16,384` | `packages/protocol/src/terminal-input.ts:15` |
| `AGENT_PROMPT_MAX_REASON_LENGTH` | `200` | `packages/protocol/src/terminal-input.ts:21` |
| Prompt wait timeout | `1..300,000 ms` | `packages/protocol/src/terminal-input.ts:22-23` |

## Errors

- Status schemas require `completed_revision <= revision` and all-or-none identity fields. Invalid message/identity/ordering fails validation and does not mutate the hub.
- Missing/foreign/closed status RPC sessions normalize to `NotFound: agent status not found`. Wait invalid input is `InvalidArgument`, capacity is `ResourceExhausted`, and caller cancellation is `Canceled`.
- Wait outcomes are `matched`, `timed_out`, `occupant_changed`, `session_closed`; prompt wait additionally reports `prompt_stalled`.
- Prompt rejection reasons are `BLOCKED`, `NOT_PROMPTABLE`, `NOT_FOREGROUND`, `FENCE_CHANGED`, `PROCESS_CHANGED`, `SESSION_UNAVAILABLE`, `EXPIRED`, `KEEPER_REJECTED`. Input remains separately `accepted|rejected|ambiguous`.
- Private reference values must be nonempty, valid Unicode, control-free, and absolute for `path`; violations fail validation before persistence. A private event cannot enter a browser Sync frame.

## Reference implementation

- Private reference contract/fold: `packages/protocol/src/agent-conversation-reference.ts`, `packages/protocol/src/agent-conversation-reference-proto.ts`
- Worker reporting/restore: `apps/worker/src/agents/reference-admission.ts`, `apps/worker/src/agents/agent-conversation-restore.ts`
- Coordinator private projection: `apps/coord/src/sessions/handlers-sessions.ts`, `apps/coord/src/events/`
- Volatile status schema: `packages/protocol/src/wire/agent-status.ts`
- Worker detection: `apps/worker/src/agents/`
- Coordinator hub/waits: `apps/coord/src/agents/agent-status-hub.ts`, `apps/coord/src/agents/agent-status-wait.ts`
- Prompt boundary: `apps/coord/src/agents/handlers-agent-prompt.ts`, `apps/coord/src/agents/agent-prompt-control.ts`, `apps/worker/src/agents/agent-prompt-control.ts`
