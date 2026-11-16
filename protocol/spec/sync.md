<!-- Sync WebSocket contract: authenticated negotiation, recovery ordering, domains, and cumulative flow control. -->
<!-- Clients consume protocol/spec/sync.md; generated frame shapes come from protocol/proto/roost/v1/sync.proto. -->
<!-- roost_protocol::wire::sync_ws owns the exact endpoint and negotiation literals. -->

# Sync WebSocket

## Purpose

Sync is the authenticated browser metadata, control, terminal-fallback, and authoritative terminal-cell plane. It is a WebSocket transport at `/ws/coord-sync`, not a Connect streaming method: `CoordinatorService` declares no `Sync` method, and `POST /roost.v1.CoordinatorService/Sync` answers HTTP 410 `sync moved to /ws/coord-sync`.

## Messages

| Message | File | Meaning |
| --- | --- | --- |
| `FirehoseFrame` | `protocol/proto/roost/v1/sync.proto` | Server application frame. `delivery_seq` is zero for unsequenced/control frames. |
| `SyncClientFrame` | `protocol/proto/roost/v1/sync.proto` | Cumulative `ack_delivery_seq`, v2 socket identity, domain subscription, terminal view/input, and probe commands. |
| `SyncSubscribedFrame`, `SyncDomainGeneration` | `protocol/proto/roost/v1/sync.proto` | v2 socket/process/domain generation announcement. |
| `SyncDomainReadyCommand`, `SyncDomainSubscriptionCommand` | `protocol/proto/roost/v1/sync.proto` | v2 domain hydration and subscription transitions. |
| `InputAccepted`, `InputRejected`, `InputAmbiguous` | `protocol/proto/roost/v1/sync.proto` | Truthful terminal-write result correlated by session, `input_seq`, and domain generation. |
| `KeepaliveFrame` | `protocol/proto/roost/v1/sync.proto` | Timestamp-only liveness frame. |
| `ClientStateChangedFrame` | `protocol/proto/roost/v1/sync.proto` | Empty account-scoped invalidation (field 61), sent after a client-state commit. Unsequenced control, independent of domain readiness; the browser re-reads `ClientStateList`. See [`client-state.md`](client-state.md). |
| `TerminalClipboardFrame` | `protocol/proto/roost/v1/sync.proto` | One-shot OSC 52 clipboard write for an observed session. Volatile event: not retained or seeded on reconnect; text is at most 256 KiB. |
| `ClipboardHistoryFrame` | `protocol/proto/roost/v1/sync.proto` | Persisted clipboard history added/removed/cleared event, sent to every install-wide Sync link (an added entry carries its text). Not seeded: the history sheet loads `ClipboardList` when it opens and folds these while open. |
| `TerminalCommandFinishedFrame` | `protocol/proto/roost/v1/sync.proto` | One-shot long-command completion for an observed session; not retained or seeded, and contains no terminal text. |
| `TerminalBellFrame` | `protocol/proto/roost/v1/sync.proto` | One-shot terminal BEL for an observed session (worker rate limit: one per 500 ms per channel); not retained or seeded. |
| `TerminalSignalsFrame` | `protocol/proto/roost/v1/sync.proto` | Per observed session: OSC 9;4 progress, OSC 1337 user variables and OSC 7501 program status are retained and seeded on connect (program status carries the whole record set, and an empty set clears it); the OSC 9/777 notification is one-shot. Each part is present only when it changed. |
| `WorkerLiveSubscription`, `WorkerLiveMetricsFrame` | `protocol/proto/roost/v1/sync.proto` | Browser asks for live metrics of a set of workers by fingerprint (`SyncClientFrame` field 13). Each send replaces the socket's whole set; an empty set clears it, and a repeat command is idempotent. A list that names one worker twice is refused. Samples arrive as `WorkerLiveMetricsFrame` (`FirehoseFrame` field 58). Socket-bound and latest-only: no ACK or replay, never persisted, dropped under backpressure like other live frames. |

## State machine

1. Client opens `/ws/coord-sync` with subprotocol `roost-auth`, bearer JWT, optional `tab`, optional `since`, and exact `flow=1&sync_v=2` for v2. A v2 socket without `tab` is read-only. Account browsers add `client_state=1` to subscribe to live account preference invalidations.
2. Coordinator validates query values and account-device authority, assigns socket scope, and creates the feed. Retained snapshots seed before the pre-ready live segment.
3. If `since > 0`, the coordinator fixes a recovery cutoff after subscribing, then replays ordered public session events above `since` through that cutoff. v2 recovery waits for terminal `domain_ready` and available bounded queue capacity before admitting each frame; socket drains and ACKs resume it without polling or advancing past an unadmitted event. The held live tail uses the same pacing, and newer live traffic stays held until that tail drains. A cursor ahead of the log requests a terminal reset with reason `cursor_ahead_of_log`, on which the client drops its cursor to zero; failed v2 recovery requests reset and the client keeps its cursor. A replay frame larger than the queue/window byte bound resets recovery rather than waiting for capacity it can never obtain.
4. A v2 socket receives `SyncSubscribedFrame`. Client subscribes/unsubscribes exact domains. A domain's retained snapshot must precede live frames; `domain_ready` closes the snapshot/live gap and then admits live application traffic. Terminal hydration requires the one-time `SessionsList` snapshot token.
5. Every application frame on a negotiated flow-control socket receives a positive monotonic `delivery_seq`. After synchronous client dispatch, the client sends cumulative `ack_delivery_seq` plus the current `socket_id`; frames held until terminal hydration are acknowledged when that hold drains, not later. Controls use `delivery_seq=0` and never consume the application window.
6. Coordinator releases every queued record through the ACK, including exact v2 pending terminal announcements. Stale or repeated ACKs are harmless; an ACK above the last sent sequence closes the socket. A full v2 application window pauses delivery until an ACK; its age deadline, failed writes, and socket-buffer limits still fail closed.
7. Disconnect/reconnect repeats negotiation, retained seeding, durable backfill, domain hydration, and live admission. The browser never treats component detach or terminal carrier change as Sync recovery.

## Limits

| Constant | Value | Rust source |
| --- | ---: | --- |
| `SYNC_WS_PATH` | `/ws/coord-sync` | `crates/roost-protocol/src/wire/sync_ws.rs` |
| `SYNC_AUTH_SUBPROTOCOL` | `roost-auth` | `crates/roost-protocol/src/wire/sync_ws.rs` |
| `SYNC_QUERY_FLOW_V1` | `1` | `crates/roost-protocol/src/wire/sync_ws.rs` |
| `SYNC_QUERY_V2` | `2` | `crates/roost-protocol/src/versioning.rs` |
| `MAX_UNACKED_FRAMES` | `512` | `crates/roost-coord/src/sync_ws/ack_window.rs` |
| `MAX_UNACKED_BYTES` | `4 MiB` | `crates/roost-coord/src/sync_ws/ack_window.rs` |
| `ACK_TIMEOUT_MS` | `10,000 ms` | `crates/roost-coord/src/sync_ws/ack_window.rs` |

## Errors

- Connection rejection uses close code `1013` (`CONNECTION_REJECTION_CLOSE_CODE` in `crates/roost-coord/src/sync_ws/upgrade_admission.rs`).
- Backpressure closes with `1013`; reason is one of `high_water`, `timeout`, `frame_limit`, `byte_limit`, `age_limit` (`crates/roost-coord/src/sync_ws/ack_window.rs`).
- An ACK above the last sent sequence closes with `1008` invalid-ACK policy (`INVALID_ACK_CLOSE_CODE` in `crates/roost-coord/src/sync_ws/ack_window.rs`).
- v2 `domain_ready` without a current terminal snapshot token resets that domain with `snapshot_token_invalid` (`crates/roost-coord/src/sync_ws/commands.rs`).
- Recovery reset reasons emitted by current code include `cursor_ahead_of_log`, `recovery_failed`, and backfill truncation diagnostics. `POST /roost.v1.CoordinatorService/Sync` answers HTTP 410 `sync moved to /ws/coord-sync`.

## Reference implementation

Endpoint literals `crates/roost-protocol/src/wire/sync_ws.rs`; proto `protocol/proto/roost/v1/sync.proto`; coordinator `crates/roost-coord/src/sync_ws/` (upgrade in `upgrade_admission.rs`, feed and backfill in `feed/` and `backfill.rs`, domains and egress in `commands.rs` and `egress.rs`, ACK and backpressure in `ack_window.rs` and `driver.rs`); browser dispatch and ACK `crates/roost-client-core/src/sync/` and `crates/roost-client-core/src/handle_sync/`.
