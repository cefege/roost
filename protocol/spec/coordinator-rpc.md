<!-- Coordinator RPC contract: one Connect service implementation forwarding to domain handlers. -->
<!-- The method list is the service CoordinatorService block in protocol/proto/roost/v1/coordinator.proto. -->
<!-- crates/roost-coord/src/rpc/auth_gate.rs owns principal and request-context policy. -->

# Coordinator RPC

## Purpose

`CoordinatorService` is the browser-to-coordinator unary/control surface at `POST /roost.v1.CoordinatorService/<Method>`. Binary Connect is authoritative. Sync, direct terminal, attachment direct carriers, and the coordinator-worker link are separate transports specified elsewhere; `POST /roost.v1.CoordinatorService/Sync` answers HTTP 410 `sync moved to /ws/coord-sync`, and the service declares no `Sync` method.

## Messages

| Domain | Proto messages / method block | Current transport note |
| --- | --- | --- |
| Workers | `Workers*`, `WorkersDeploy*`, `WorkersPrepareKeeperUpdate*` | Browser and worker methods share one service; principal is method-specific. |
| Sessions | `Sessions*` | Includes lifecycle, input, scrollback/search, direct terminal grant/signaling, attachment-peer signaling. |
| Organization | `Workspaces*` | Durable dashboard-local coordination state. |
| Auth/devices/pair | `Auth*`, `Devices*`, `Pair*` | Public identity/bootstrap redemption alternate with device-gated and split pairing authority. |
| System/audit | `Misc*`, `Audit*` | Health is public; DB export URL additionally requires on-host; metrics/diagnostics/audit require device authority. |
| Files/attachments | `Files*`, `Attachments*`, `AttachFileChunk`, `AttachmentProbe`, `ListAttachments`, `DeleteAttachment` | AttachFileChunk is coordinator relay fallback, not the direct-carrier ACK contract. |
| Settings/UI/push | `Transcription*`, `Ui*`, `Push*` | Device-authorized dashboard state; UI apply targets one current Sync-v2 socket generation. |
| Diagnostics | `DiagDebugLogBatch`, `DiagSnapshot` | Device-authorized bounded diagnostics. |
| Sync compatibility | `FirehoseFrame` (`sync.proto`) | Current path is `/ws/coord-sync`; the retired Connect path answers HTTP 410. |


## State machine

1. The auth gate (`rpc/auth_gate.rs`) reads `Authorization: Bearer ...`, verifies and resolves a persisted principal, installs caller/trace/remote/on-host/listener-trust/tab context, then calls the single service implementation.
2. Each handler applies its method-specific principal and resource authorization before mutation or read. The service has exactly one `impl CoordinatorService` (`rpc/service_impl.rs`); a second service implementation would shadow the methods it omits with unimplemented answers, and `rpc/method_route.rs` holds the route table to the proto's method list.
3. Durable coordinator mutations selected by `write_gate::method_holds_lease` hold the exclusive write gate. Terminal input deliberately acquires its narrower per-session FIFO later so queued input cannot hold the global drain. Read methods do not hold it.
4. Handler mutation commits first, then the domain bus/publication updates Sync projections. Optimistic concurrency, resource scope, tab identity, and worker connection readiness are enforced at the owning domain boundary.
5. Connect error codes carry the failure. The auth gate maps them to HTTP-equivalent audit status and releases its write lease when the call ends; response-aware audit policy is independent of handler business logic.
6. `PairPoll` is token-bound anonymous high-volume polling and never persists audit rows. Successful `PairConfirm` writes an explicit success audit row. Other listed low-signal successes may be skipped, while failures remain eligible under listener policy.

## Limits

| Constant / policy | Value | Rust source |
| --- | --- | --- |
| RPC path | `/roost.v1.CoordinatorService/<Method>` | `protocol/proto/roost/v1/coordinator.proto` (`service CoordinatorService`) |
| `CoordinatorService` implementations | exactly `1` | `crates/roost-coord/src/rpc/service_impl.rs` |
| Route table rows | `87`, one per proto method | `crates/roost-coord/src/rpc/method_route_rows.rs` |
| Protected principal marker | `x-roost-auth-layer: device` | `crates/roost-coord/src/auth/principal.rs` (`AUTH_LAYER_HEADER`, `AUTH_LAYER_DEVICE`) |
| Terminal-search owner | device fingerprint + required `x-roost-tab-id` | `crates/roost-coord/src/search/rpc_search.rs` |
| Audit page maximum | `500` | `crates/roost-coord/src/diagnostics/rpc_audit.rs` (`MAX_LIMIT`) |
| File read chunk maximum | `4 MiB` | `crates/roost-coord/src/attachments/files.rs` (`MAX_CHUNK_LEN`) |

## Errors

Standard Connect `Code` values are used:

| Code | HTTP audit status | Typical meaning |
| --- | ---: | --- |
| `InvalidArgument`, `OutOfRange` | `400` | Malformed request or failed field validation. |
| `Unauthenticated` | `401` | Missing/invalid authority where the method requires it. |
| `PermissionDenied` | `403` | Wrong principal kind, resource scope, tab, or on-host requirement. |
| `NotFound` | `404` | Missing/foreign resource, normalized where required. |
| `AlreadyExists`, `Aborted` | `409` | Identity collision or concurrent transition. |
| `FailedPrecondition` | `412` | Invalid lifecycle/version/ordering state. |
| `ResourceExhausted` | `429` | Bounded owner/capacity rejection. |
| `Unimplemented` | `501` | Method not declared by this build. |
| `Unavailable` | `503` | Required worker/socket is offline or not routable. |
| `DeadlineExceeded` | `504` | Bounded downstream wait expired. |

Unhandled errors map to `500`. The mapping is `crates/roost-coord/src/middleware/audit_policy.rs`; the auth gate itself does not reject public methods, and handler helpers produce the actual Connect error.

## Reference implementation

The method list is the `service CoordinatorService` block in `protocol/proto/roost/v1/coordinator.proto`; the one implementation is `crates/roost-coord/src/rpc/service_impl.rs`, which forwards each method to its domain handler:

| Domain | Handler module | Auth requirement |
| --- | --- | --- |
| Workers | `crates/roost-coord/src/workers/` | `WorkersRegister/Heartbeat`: worker; list/rename/delete/deploy: device. |
| Keeper update | `crates/roost-coord/src/deploy/keeper_update.rs` | Device plus update/on-host policy. |
| Sessions | `crates/roost-coord/src/sessions/rpc_sessions.rs` | `SessionsList`: device, or worker restricted to its own open recovery; all other methods: device. |
| Workspaces | `crates/roost-coord/src/sessions/rpc_workspaces.rs` | Device. |
| Auth/pair/devices | `crates/roost-coord/src/auth/` (`rpc_bootstrap.rs`, `rpc_devices.rs`, `rpc_identity.rs`, `rpc_pairing.rs`) | Mixed: public identity/bootstrap redemption; device mint/logout/devices; pairing public-token/on-host/device rules per [`auth-and-pairing.md`](auth-and-pairing.md). |
| System/diagnostics/audit | `crates/roost-coord/src/diagnostics/` and `crates/roost-coord/src/rpc/service.rs` | Health public; DB export URL device + on-host; remaining methods device. |
| Transcription | `crates/roost-coord/src/diagnostics/rpc_transcription.rs` | Device. |
| Attachments/files | `crates/roost-coord/src/attachments/rpc_files.rs` | Device. |
| Attachment direct | `crates/roost-coord/src/attachments/rpc_direct.rs` | Device plus exact tab/grant descriptor. |
| Attachment peer | `crates/roost-coord/src/attachments/peer.rs` | Device plus exact tab/grant/worker fence. |
| UI | `crates/roost-coord/src/ui_state/rpc.rs` | Device plus persisted session/current socket generation. |
| Push | `crates/roost-coord/src/push/rpc.rs` | Device. |

### Assembly and policy

- Single service assembly and route table: `crates/roost-coord/src/rpc/service_impl.rs`, `crates/roost-coord/src/rpc/method_route.rs`
- JWT/principal/context/write-gate/audit policy: `crates/roost-coord/src/rpc/auth_gate.rs`, `crates/roost-coord/src/write_gate.rs`, `crates/roost-coord/src/middleware/audit_layer.rs`
- Persisted principal resolution: `crates/roost-coord/src/auth/principal.rs`
- Current Sync WebSocket: [`sync.md`](sync.md)
