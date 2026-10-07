<!-- Session-event contract: durable ordering, public projection, and private recovery variants. -->
<!-- Clients consume protocol/spec/session-events.md; @roost/protocol is the TypeScript oracle. -->
<!-- Conformance vectors live in protocol/conformance/session-fold/. -->

# Session events

## Purpose

`SessionEvent` is the append-only contract for public session state. The worker reserves capacity before durable lifecycle mutation, the coordinator validates and commits each event before publication, and the browser folds public events in stable order. The same `foldEvent` implementation defines the public projection on both sides.

## Messages

| Message | File | Meaning |
| --- | --- | --- |
| `SessionEvent` variants: `opened`, `closed`, `attached`, `detached`, `cwd`, `workspace_assigned`, `snapshot`, `respawned`, `renamed`, `git`, `pr`, `ports` | `packages/protocol/src/wire/event.ts:20-123` | Validated portable event union. |
| `SessionEventProto` and `OpenedEvt` … `PortsEvt` | `protocol/proto/roost/v1/events.proto` | Protobuf event envelope and oneof variants (oneof tag 23 is reserved). `event_id` is coordinator storage identity. |
| `WSessionEvent` | `protocol/proto/roost/v1/worker_transport.proto:33-36` | Worker event plus monotonic `client_seq`; at-least-once transport, deduplicated by coordinator. |

## State machine

1. A durable worker event is reserved in the bounded SQLite `SessionEventStore` before the corresponding keeper mutation. Its positive `client_seq` is stable across retries.
2. The worker link sends `WSessionEvent` and waits for `DEventAck { client_seq }`; coordinator insert or unique-index deduplication produces the ACK.
3. The coordinator commits the event and public `sessions` projection in one transaction, then publishes the committed event.
4. A fresh browser receives current retained state. On reconnect, `since_event_id` replays ordered public rows above its last folded ID, then the socket changes to live delivery without crossing the snapshot/live gap.
5. `foldEvent` is pure and order-dependent: `opened` inserts; `closed` deletes; `respawned` rebinds channel and forces open; `renamed` sets or clears sticky custom title; `cwd`, `workspace_assigned`, `git`, `pr`, and `ports` patch an existing row; `snapshot` upserts announced sessions while retaining immutable creation fields and does not prune absent sessions.
6. `attached` and `detached` are explicit public-fold no-ops.

## Limits

`timestamps` must be positive integers.

## Errors

- `SessionEvent.parse` reports Zod validation failures.
- Non-positive/unsafe worker `client_seq`, duplicate in-flight sequence, or an unencodable durable row raises `SessionEventStoreFatalError`; these are local fatal transport/store conditions, not retryable client errors.
- Missing sessions make `closed` and ordinary metadata patches no-ops. `closed` is the only public-fold deletion trigger.

## Reference implementation

- Portable schema and oracle: `packages/protocol/src/wire/event.ts`
- Protobuf adapters: `packages/protocol/src/wire/event-proto.ts`
- Durable worker store and replay: `apps/worker/src/transport/session-event-store.ts`, `apps/worker/src/transport/coord-link-unacked.ts`
- Coordinator transaction/publication: `apps/coord/src/events/event-log.ts`, `apps/coord/src/events/event-projection.ts`
- Browser fold: `apps/web/src/store/projector.ts`
- Private reference validation/fold: `packages/protocol/src/agent-conversation-reference.ts`
- Conformance: `protocol/conformance/session-fold/`
