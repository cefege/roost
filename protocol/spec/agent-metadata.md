<!-- Agent metadata contract: volatile PID-free status observations and the identity-fenced prompt. -->
<!-- Neither surface creates or controls a structured agent session; both describe the ordinary shell PTY. -->
<!-- Canonical limits live in @roost/protocol wire and terminal-input. -->

# Agent metadata

## Purpose

Agent status is volatile worker observation (`idle|working|blocked`) used for dashboard state, waits, notifications, and one identity-fenced PTY input. It is not an agent execution API.

## Messages

| Message | File | Meaning |
| --- | --- | --- |
| `WAgentStatus` | `protocol/proto/roost/v1/worker_transport.proto:66-79` | Worker-originated volatile observation. |
| `AgentStatusFrame` | `protocol/proto/roost/v1/sync.proto:83-96` | Same observation fanned to browser Sync. |
| `AgentStatusView`, `AgentStatusGet/List/Wait` messages | `protocol/proto/roost/v1/coordinator.proto:412-440` | PID-free RPC projection plus coordinator-derived `promptable`. |
| `SessionsPromptRequest/Response`, `DAgentPrompt`, `WInputResult` | `protocol/proto/roost/v1/coordinator.proto`; `protocol/proto/roost/v1/worker_transport.proto` | Exact status-identity fence around one ordinary PTY input, with separate input/wait outcomes. |

## State machine


1. Worker process scanning, integration reports, and screen/title observation produce one effective row per session. `status_epoch` identifies registry lifetime; `occupant_id` identifies a verified process incarnation; `source` is `integration|screen`; revisions are monotonic within that identity.
2. An integration observation beats screen. A silent integration lease expires and the worker falls back automatically. Only an identified integration row is `promptable`; screen and identityless legacy rows remain readable.
3. Authenticated worker frames enter the coordinator's in-memory hub. A new active identity replaces the old one; retired identities remain fenced. `active:false` removes retained status. Session close removes the row and resolves waiters `session_closed`. Sync seeds its current hub snapshot; nothing survives process restart.
4. `AgentStatusWait` registers before inspecting current state, pins exact epoch/occupant, and resolves only on real state progress, timeout, occupant replacement, or close. Revision/message republish at the same state does not satisfy a wait.
5. `SessionsPrompt` requires exact epoch, occupant, and revision plus a current idle/working integration state. Coordinator registers its optional waiter, sends `DAgentPrompt`, and consumes the waiter only after definite rejection or after accepted/ambiguous input. `WInputResult` remains write truth; ambiguous input is never retried.

## Limits

| Constant | Value | TypeScript source |
| --- | ---: | --- |
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
