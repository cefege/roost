<!-- Sync WebSocket contract: authenticated negotiation, recovery ordering, domains, and cumulative flow control. -->
<!-- Clients consume protocol/spec/sync.md; generated frame shapes come from protocol/proto/roost/v1/sync.proto. -->
<!-- @roost/protocol/wire/sync-ws owns the exact endpoint and negotiation literals. -->

# Sync WebSocket

## Purpose

Sync is the authenticated browser metadata, control, terminal-fallback, and authoritative terminal-cell plane. It is a WebSocket transport at `/ws/coord-sync`, not the retired Connect streaming method; `CoordinatorService.Sync` remains in the proto only to return `Unimplemented` and direct callers to this endpoint.

## Messages

| Message | File | Meaning |
| --- | --- | --- |
| `SyncRequest` | `protocol/proto/roost/v1/sync.proto:15-21` | Legacy Connect request; WebSocket recovery uses query `since`. |
| `FirehoseFrame` | `protocol/proto/roost/v1/sync.proto:252-299` | Server application frame. `delivery_seq` is zero for unsequenced/control frames. |
| `SyncClientFrame` | `protocol/proto/roost/v1/sync.proto:304-322` | Cumulative `ack_delivery_seq`, v2 socket identity, domain subscription, terminal view/input, and probe commands. |
| `SyncSubscribedFrame`, `SyncDomainGeneration` | `protocol/proto/roost/v1/sync.proto:115-125` | v2 socket/process/domain generation announcement. |
| `SyncDomainReadyCommand`, `SyncDomainSubscriptionCommand` | `protocol/proto/roost/v1/sync.proto:134-143` | v2 domain hydration and subscription transitions. |
| `InputAccepted`, `InputRejected`, `InputAmbiguous` | `protocol/proto/roost/v1/sync.proto:229-249` | Truthful terminal-write result correlated by session, `input_seq`, and domain generation. |
| `KeepaliveFrame` | `protocol/proto/roost/v1/sync.proto:324-326` | Timestamp-only liveness frame. |

## State machine

1. Client opens `/ws/coord-sync` with subprotocol `roost-auth`, bearer JWT, optional `tab`, optional `since`, and exact `flow=1&sync_v=2` for v2. A v2 socket without `tab` is read-only.
2. Coordinator validates query values and account-device authority, assigns socket scope, and creates the feed. Retained snapshots seed before the pre-ready live segment.
3. If `since > 0`, the coordinator fixes a recovery cutoff, replays ordered public session events above `since` through that cutoff, and subscribes/advances live traffic without exposing a gap. A cursor ahead of the log requests reset; failed v2 recovery requests reset.
4. A v2 socket receives `SyncSubscribedFrame`. Client subscribes/unsubscribes exact domains. A domain's retained snapshot must precede live frames; `domain_ready` closes the snapshot/live gap and then admits live application traffic. Terminal hydration requires the one-time `SessionsList` snapshot token.
5. Every application frame on a negotiated flow-control socket receives a positive monotonic `delivery_seq`. After synchronous client dispatch, the client sends cumulative `ack_delivery_seq` plus the current `socket_id`. Controls use `delivery_seq=0` and never consume the application window.
6. Coordinator releases every queued record through the ACK, including exact v2 pending terminal announcements. Stale or repeated ACKs are harmless; an ACK above the last sent sequence closes the socket. Native backpressure and the application window both fail closed.
7. Disconnect/reconnect repeats negotiation, retained seeding, durable backfill, domain hydration, and live admission. The browser never treats component detach or terminal carrier change as Sync recovery.

## Limits

| Constant | Value | TypeScript source |
| --- | ---: | --- |
| `SYNC_WS_PATH` | `/ws/coord-sync` | `packages/protocol/src/wire/sync-ws.ts:6` |
| `SYNC_AUTH_SUBPROTOCOL` | `roost-auth` | `packages/protocol/src/wire/sync-ws.ts:7` |
| `SYNC_QUERY_FLOW_V1` | `1` | `packages/protocol/src/wire/sync-ws.ts:10` |
| `SYNC_QUERY_V2` | `2` | `packages/protocol/src/wire/sync-ws.ts:11` |
| `APPLICATION_MAX_UNACKED_FRAMES` | `512` | `apps/coord/src/sync/sync-ws-v1-delivery.ts:23` |
| `APPLICATION_MAX_UNACKED_BYTES` | `4 MiB` | `apps/coord/src/sync/sync-ws-v1-delivery.ts:24` |
| `APPLICATION_ACK_TIMEOUT_MS` | `3,000 ms` | `apps/coord/src/sync/sync-ws-v1-delivery.ts:25` |

## Errors

- Connection rejection uses close code `1013` (`SYNC_CONNECTION_REJECTION_CLOSE_CODE` in `apps/coord/src/sync/sync-ws-upgrade.ts:32`).
- Backpressure closes with `1013`; reason is one of `high_water`, `timeout`, `frame_limit`, `byte_limit`, `age_limit` (`apps/coord/src/sync/sync-ws-v1-delivery.ts:27-32`).
- An ACK above the last sent sequence closes with `1008` invalid-ACK policy (`apps/coord/src/sync/sync-ws-v1-delivery.ts:95-115`).
- v2 `domain_ready` without a current terminal snapshot token resets that domain with `snapshot_token_invalid` (`apps/coord/src/sync/sync-ws-v2-commands.ts:115-147`).
- Recovery reset reasons emitted by current code include `cursor_ahead_of_log`, `recovery_failed`, and backfill truncation diagnostics. `CoordinatorService.Sync` returns Connect `Unimplemented` with `sync moved to /ws/coord-sync`.

## Reference implementation

- Shared endpoint literals: `packages/protocol/src/wire/sync-ws.ts`
- Proto and generated messages: `protocol/proto/roost/v1/sync.proto`, `packages/protocol/src/gen/roost/v1/sync_pb.ts`
- Upgrade/auth/scope: `apps/coord/src/sync/sync-ws-upgrade.ts`
- Feed and backfill: `apps/coord/src/sync/sync-feed.ts`, `apps/coord/src/sync/sync-feed-seed.ts`
- v2 domains/egress: `apps/coord/src/sync/sync-ws-v2-commands.ts`, `apps/coord/src/sync/sync-ws-v2-egress.ts`
- ACK/backpressure: `apps/coord/src/sync/sync-ws-v1-delivery.ts`
- Browser dispatch/ACK: `apps/web/src/client/sync/sync-flow.ts`
