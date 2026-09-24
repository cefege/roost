<!-- Session-event contract: durable ordering, public projection, and private recovery variants. -->
<!-- Clients consume protocol/spec/session-events.md; @roost/protocol is the TypeScript oracle. -->
<!-- Conformance vectors live in protocol/conformance/session-fold/. -->

# Session events

## Purpose

`SessionEvent` is the append-only contract for public session state. The worker reserves capacity before durable lifecycle mutation, the coordinator validates and commits each event before publication, and the browser folds public events in stable order. The same `foldEvent` implementation defines the public projection on both sides.

## Messages

| Message | File | Meaning |
| --- | --- | --- |
| `SessionEvent` variants: `opened`, `closed`, `attached`, `detached`, `cwd`, `workspace_assigned`, `snapshot`, `respawned`, `renamed`, `git`, `pr`, `ports`, `agent_reference` | `packages/protocol/src/wire/event.ts:20-123` | Validated portable event union. |
| `SessionEventProto` and `OpenedEvt` … `AgentReferenceEvt` | `protocol/proto/roost/v1/events.proto:9-132` | Protobuf event envelope and oneof variants. `event_id` is coordinator storage identity. |
| `WSessionEvent` | `protocol/proto/roost/v1/worker_transport.proto:33-36` | Worker event plus monotonic `client_seq`; at-least-once transport, deduplicated by coordinator. |
| `AgentConversationReferenceV1` | `protocol/proto/roost/v1/wire.proto`; `packages/protocol/src/agent-conversation-reference.ts:36-71` | Private opaque recovery reference; absent reference clears it. |

## State machine

1. A durable worker event is reserved in the bounded SQLite `SessionEventStore` before the corresponding keeper mutation. Its positive `client_seq` is stable across retries.
2. The worker link sends `WSessionEvent` and waits for `DEventAck { client_seq }`; coordinator insert or unique-index deduplication produces the ACK.
3. The coordinator commits the event and public `sessions` projection in one transaction, then publishes the committed public event. `agent_reference` updates only the sequence-aware private recovery projection.
4. A fresh browser receives current retained state. On reconnect, `since_event_id` replays ordered public rows above its last folded ID, then the socket changes to live delivery without crossing the snapshot/live gap.
5. `foldEvent` is pure and order-dependent: `opened` inserts; `closed` deletes; `respawned` rebinds channel and forces open; `renamed` sets or clears sticky custom title; `cwd`, `workspace_assigned`, `git`, `pr`, and `ports` patch an existing row; `snapshot` upserts announced sessions while retaining immutable creation fields and does not prune absent sessions.
6. `attached`, `detached`, and `agent_reference` are explicit public-fold no-ops. Private recovery applies only strictly newer `client_seq` values through `foldAgentConversationRecoveryMetadata`.

## Limits

| Constant | Value | TypeScript source |
| --- | ---: | --- |
| `AGENT_CONVERSATION_SESSION_ID_MAX_UTF8_BYTES` | `512` | `packages/protocol/src/agent-conversation-reference.ts:11` |
| `AGENT_CONVERSATION_SESSION_PATH_MAX_UTF8_BYTES` | `4,096` | `packages/protocol/src/agent-conversation-reference.ts:12` |
| `AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES` | `8,192` | `packages/protocol/src/agent-conversation-reference.ts:16` |

`timestamps` must be positive integers. `AgentConversationReferenceV1.schema_version` is `1`, `agent_id` is `omp`, and `kind` is `id|path`; a path must be absolute in POSIX or Windows shape and every value must be nonempty, well-formed Unicode without Unicode `Cc` controls.

## Errors

- `SessionEvent.parse` reports Zod validation failures; oversized private envelopes fail the `agent conversation reference event must not exceed 8192 UTF-8 bytes` refinement.
- Non-positive/unsafe worker `client_seq`, duplicate in-flight sequence, or an unencodable durable row raises `SessionEventStoreFatalError`; these are local fatal transport/store conditions, not retryable client errors.
- A private update for the wrong session throws `agent conversation recovery session mismatch`; a non-positive sequence throws `RangeError`. A sequence less than or equal to the retained sequence is an idempotent no-op.
- Missing sessions make `closed` and ordinary metadata patches no-ops. `closed` is the only public-fold deletion trigger.

## Reference implementation

- Portable schema and oracle: `packages/protocol/src/wire/event.ts`
- Protobuf adapters: `packages/protocol/src/wire/event-proto.ts`
- Durable worker store and replay: `apps/worker/src/transport/session-event-store.ts`, `apps/worker/src/transport/coord-link-unacked.ts`
- Coordinator transaction/publication: `apps/coord/src/events/event-log.ts`, `apps/coord/src/events/event-projection.ts`
- Browser fold: `apps/web/src/store/projector.ts`
- Private reference validation/fold: `packages/protocol/src/agent-conversation-reference.ts`
- Conformance: `protocol/conformance/session-fold/`
