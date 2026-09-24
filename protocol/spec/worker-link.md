<!-- Worker-link contract: authenticated raw WebSocket, ordered durable replay, snapshot barrier, then live traffic. -->
<!-- WorkerService.Attach is retired; production uses /ws/coord-worker/<fingerprint> with binary CoordWorker frames. -->
<!-- Protocol meaning lives in protocol/proto/roost/v1/worker_transport.proto. -->

# Coordinator-worker link

## Purpose

Each worker dials one long-lived, full-duplex binary WebSocket to the coordinator. The worker is not a browser client: the URL fingerprint, JWT, persisted principal, key generation, and current connection generation must all agree. Application traffic cannot become live before one ordered durable replay and one authoritative worker snapshot commit.

## Messages

| Message | File | Meaning |
| --- | --- | --- |
| `WHello`, `DHelloAck` | `protocol/proto/roost/v1/worker_transport.proto` | Worker identity/version/capabilities/process epoch and coordinator capability acknowledgement. |
| `WSessionEvent`, `DEventAck` | `protocol/proto/roost/v1/worker_transport.proto` | At-least-once durable event plus exact `client_seq` ACK. |
| `CoordWorkerUp`, `CoordWorkerDown` | `protocol/proto/roost/v1/worker_transport.proto` | Full upstream/downstream binary oneofs. |
| `WRefreshJwt` | `protocol/proto/roost/v1/worker_transport.proto:54` | In-band replacement token, revalidated against the exact worker principal. |
| `WCellGrid`, `WCellGridChunk` | `protocol/proto/roost/v1/worker_transport.proto:58-59` | Terminal full/delta or bounded snapshot part. |
| `WAgentStatus` | `protocol/proto/roost/v1/worker_transport.proto:66-79` | Volatile PID-free status observation. |
| `WInputResult`, `WTerminalStreamResult` | `protocol/proto/roost/v1/worker_transport.proto:96-159` | Truthful write/stream outcome with write phase and failure classification. |
| `WTerminalViewState`, `WTerminalViewProjection` | `protocol/proto/roost/v1/worker_transport.proto:170-187` | Worker-owned view result and coordinator presence/diagnostic projection. |
| Direct grant/peer/attachment result frames | `protocol/proto/roost/v1/worker_transport.proto` | Coordinator-authorized direct terminal/attachment admission and liveness. |

## State machine

1. Worker dials `/ws/coord-worker/<64-hex fingerprint>` with exactly `[WORKER_AUTH_SUBPROTOCOL, jwt]`; credentials never enter the query. Coordinator rejects any query, malformed subprotocol envelope, invalid JWT, non-worker principal, path/JWT mismatch, or stale key generation before upgrade.
2. Socket lifecycle is `idle → connecting → open → reconnecting`; `open` is not application-ready. The only forced first write is `WHello`. A duplicate/mismatched hello closes the socket.
3. `DHelloAck` moves the application barrier from `hello` to `replay`. Exactly one durable `WSessionEvent` is in flight; each exact positive `DEventAck` acknowledges insert or unique-index deduplication. Stale/duplicate ACKs cannot release the barrier.
4. With no durable event or blocking reservation, the worker sends one authoritative sequenced `snapshot` SessionEvent. The coordinator commits it, ACKs its sequence, marks that exact connection routable, and triggers missing-session respawn repair.
5. Snapshot ACK moves the worker to `live` and drains volatile agent status, control, cell, and compatibility metadata lanes. A durable append or blocking reservation that appears during snapshot forces replay again before later traffic.
6. Disconnect resets application state to `hello` and clears volatile lanes; durable SQLite outbox rows remain. Reconnect repeats hello, one-at-a-time replay, snapshot, then live. A newer authenticated hello immediately supersedes the old connection generation; delayed callbacks are identity-fenced.
7. View ownership capability `terminal-view-owner-v1` delegates membership/geometry/stream generations to `TerminalViewOwner`; otherwise the coordinator `TerminalViewHub` owns them. Grants and direct peer signaling retain the exact worker epoch and connection fence.
8. A worker may refresh its JWT in band before expiry. Valid refresh updates the same stream; invalid, expired, revoked, fingerprint-changing, or generation-changing refresh closes it.

## Limits

| Constant | Value | TypeScript source |
| --- | ---: | --- |
| `WORKER_SNAPSHOT_MAX_SESSIONS` | `1,024` | `apps/worker/src/transport/coord-link-constants.ts:39` |
| `WORKER_SNAPSHOT_MAX_BYTES` | `4 MiB` | `apps/worker/src/transport/coord-link-constants.ts:40` |
| `UNACKED_CAP` | `8,192` | `apps/worker/src/transport/coord-link-constants.ts:52` |
| `PENDING_BYTES_CAP` | `8 MiB` | `apps/worker/src/transport/coord-link-constants.ts:31` |
| `WS_BUFFERED_HIGH_WATER_BYTES` | `4 MiB` | `apps/worker/src/transport/coord-link-constants.ts:35` |
| `STABLE_SESSION_MS` | `30,000 ms` | `apps/worker/src/transport/coord-link-constants.ts:48` |
| `STALE_LINK_TIMEOUT_MS` / check | `90,000 / 15,000 ms` | `apps/worker/src/transport/coord-link-constants.ts:61-62` |
| Reconnect initial / ordinary cap | `500 / 30,000 ms` | `apps/worker/src/transport/coord-link-constants.ts:4-5` |

## Errors

- Upgrade failures return HTTP `401 unauthorized`; failed Bun upgrade returns HTTP `400 upgrade failed` (`apps/coord/src/workers/worker-ws-upgrade.ts:53-117`).
- Pre-hello durable events diagnose `event_before_hello`; other pre-ready traffic diagnoses `before_snapshot_ready`. Duplicate/mismatched hello and invalid in-band refresh close the stream.
- Queue overflow closes `1009`; worker rate violation closes `1008`; credential revocation closes `4001`; authenticated reauth deadline closes `4003`.
- Coordinator routing is unavailable until exact snapshot ACK. RPC callers normally surface an absent/superseded worker as Connect `Unavailable: worker offline`.
- `WInputResult` separates `accepted|rejected|ambiguous` and `TerminalWritePhase pre_write|written|unknown`; only proven `PRE_WRITE` rejection is retry-safe. `WTerminalStreamResult` additionally carries `RETRYABLE_PRE_WRITE`, session-not-live, invalid-request, core-failed, or ambiguous-boundary failure kind.

## Reference implementation

- Proto: `protocol/proto/roost/v1/worker_transport.proto`
- Worker socket lifecycle: `apps/worker/src/transport/coord-link.ts`, `apps/worker/src/transport/coord-link-reconnect.ts`
- Ordered barrier/outbox: `apps/worker/src/transport/coord-link-unacked.ts`, `apps/worker/src/transport/coord-link-outbox.ts`
- Durable store: `apps/worker/src/transport/session-event-store.ts`
- Coordinator upgrade/live handler: `apps/coord/src/workers/worker-ws-upgrade.ts`, `apps/coord/src/workers/worker-ws-handler.ts`, `apps/coord/src/workers/worker-conn.ts`
