<!-- Session-event contract: durable ordering and public projection. -->
<!-- Clients consume protocol/spec/session-events.md; roost_protocol::wire::event is the Rust oracle. -->
<!-- Conformance vectors live in protocol/conformance/session-fold/. -->

# Session events

## Purpose

`SessionEvent` is the append-only contract for public session state. The worker reserves capacity before durable lifecycle mutation, the coordinator validates and commits each event before publication, and the browser folds public events in stable order. The same `fold_event` implementation defines the public projection on both sides.

## Messages

| Message | File | Meaning |
| --- | --- | --- |
| `SessionEvent` variants: `opened`, `closed`, `attached`, `detached`, `cwd`, `workspace_assigned`, `snapshot`, `respawned`, `renamed`, `git`, `pr`, `ports` | `crates/roost-protocol/src/wire/event/variants.rs` | Validated portable event union. |
| `SessionEventProto` and `OpenedEvt` … `PortsEvt` | `protocol/proto/roost/v1/events.proto` | Protobuf event envelope and oneof variants (oneof tag 23 is reserved). `event_id` is coordinator storage identity. |
| `WSessionEvent` | `protocol/proto/roost/v1/worker_transport.proto` | Worker event plus monotonic `client_seq`; at-least-once transport, deduplicated by coordinator. |

## State machine

1. A durable worker event is reserved in the bounded SQLite event store (`event_store::Store`) before the corresponding keeper mutation. Its positive `client_seq` is stable across retries.
2. The worker link sends `WSessionEvent` and waits for `DEventAck { client_seq }`; coordinator insert or unique-index deduplication produces the ACK.
3. The coordinator commits the event and public `sessions` projection in one transaction, then publishes the committed event.
4. A fresh browser receives current retained state. On reconnect, `since_event_id` replays ordered public rows above its last folded ID, then the socket changes to live delivery without crossing the snapshot/live gap.
5. `fold_event` is pure and order-dependent: `opened` inserts; `closed` deletes; `respawned` rebinds channel and forces open; `renamed` sets or clears sticky custom title; `cwd`, `workspace_assigned`, `git`, `pr`, and `ports` patch an existing row; `snapshot` upserts announced sessions while retaining immutable creation fields and does not prune absent sessions.
6. `attached` and `detached` are explicit public-fold no-ops.

## Limits

`timestamps` must be positive integers.

## Errors

- `SessionEvent::parse` returns a `ProtocolError` for a malformed event (`crates/roost-protocol/src/wire/event/variants.rs`).
- A store admission failure (`ReserveError`, `AppendError` in `crates/roost-worker/src/event_store.rs`) or a journal failure (`JournalError` in `crates/roost-worker/src/event_store/database.rs`) is a local transport/store condition, not a retryable client error.
- Missing sessions make `closed` and ordinary metadata patches no-ops. `closed` is the only public-fold deletion trigger.

## Reference implementation

Schema, fold and proto adapters `crates/roost-protocol/src/wire/event.rs` and `crates/roost-protocol/src/wire/event_proto.rs`; durable worker store and replay `crates/roost-worker/src/event_store.rs` and `crates/roost-worker/src/outbox.rs`; coordinator transaction and publication `crates/roost-coord/src/events/`; browser fold `crates/roost-client-core/src/sessions.rs`; conformance `protocol/conformance/session-fold/`.
