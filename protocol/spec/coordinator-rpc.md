<!-- Coordinator RPC contract: one Connect service assembled from domain handler factories. -->
<!-- The method list is the service CoordinatorService block in protocol/proto/roost/v1/coordinator.proto. -->
<!-- apps/coord/src/auth/auth-interceptor.ts owns principal and request-context policy. -->

# Coordinator RPC

## Purpose

`CoordinatorService` is the browser-to-coordinator unary/control surface at `POST /roost.v1.CoordinatorService/<Method>`. Binary Connect is authoritative. Sync, direct terminal, attachment direct carriers, and the coordinator-worker link are separate transports specified elsewhere; the proto's legacy streaming `Sync` method is not the live Sync implementation.

## Messages

| Domain | Proto messages / method block | Current transport note |
| --- | --- | --- |
| Workers | `Workers*`, `WorkersDeploy*`, `WorkersPrepareKeeperUpdate*`; `coordinator.proto:938-945` | Browser and worker methods share one service; principal is method-specific. |
| Sessions | `Sessions*`; `coordinator.proto:948-964` | Includes lifecycle, input, scrollback/search, direct terminal grant/signaling, attachment-peer signaling. |
| Agent | `AgentStatusGet/List/Wait`, `SessionsPrompt`; `coordinator.proto:954,967-969` | Dashboard-authorized PID-free observation and fenced ordinary PTY input. |
| Organization | `Workspaces*`, `Tasks*`, `Mcp*`; `coordinator.proto:972-989` | Durable dashboard-local coordination state. |
| Auth/devices/pair | `Auth*`, `Devices*`, `Pair*`; `coordinator.proto:992-1025` | Public identity/bootstrap redemption alternate with device-gated and split pairing authority. |
| System/audit | `Misc*`, `Audit*`; `coordinator.proto:1028-1034` | Health is public; DB export URL additionally requires on-host; metrics/diagnostics/audit require device authority. |
| Files/attachments | `Files*`, `Attachments*`, `AttachFileChunk`, `AttachmentProbe`, `ListAttachments`, `DeleteAttachment`; `coordinator.proto:1037-1041,1073-1091` | AttachFileChunk is coordinator relay fallback, not the direct-carrier ACK contract. |
| Settings/UI/push | `Transcription*`, `AgentConfig*`, `Ui*`, `Push*`; `coordinator.proto:1045-1064` | Device-authorized dashboard state; UI apply targets one current Sync-v2 socket generation. |
| Diagnostics | `DiagDebugLogBatch`, `DiagSnapshot`; `coordinator.proto:1100-1101` | Device-authorized bounded diagnostics. |
| Sync compatibility | `SyncRequest`, `FirehoseFrame`; `coordinator.proto:1068-1069` | Current path is `/ws/coord-sync`; Connect path is HTTP 410/handler `Unimplemented`. |

## State machine

1. Bun/Connect routing enters `buildConnectRouter`. The auth interceptor reads `Authorization: Bearer ...`, verifies and resolves a persisted principal, installs caller/trace/remote/on-host/listener-trust/tab context, then calls the single service implementation.
2. Each handler applies its method-specific principal and resource authorization before mutation or read. A handler spread contributes only its declared methods; a separate `router.service()` call would shadow absent methods with unimplemented stubs.
3. Durable coordinator mutations named in `WRITE_METHODS` hold the exclusive write gate. Terminal input/prompt deliberately acquire their narrower per-session FIFO later so queued input cannot hold the global drain. Read methods do not hold it.
4. Handler mutation commits first, then the domain bus/publication updates Sync projections. Optimistic concurrency, resource scope, tab identity, and worker connection readiness are enforced at the owning domain boundary.
5. Connect error codes carry the failure. The interceptor maps them to HTTP-equivalent audit status and releases its write lease in `finally`; response-aware audit policy is independent of handler business logic.
6. `PairPoll` is token-bound anonymous high-volume polling and never persists audit rows. Successful `PairConfirm` writes an explicit success audit row. Other listed low-signal successes may be skipped, while failures remain eligible under listener policy.

## Limits

| Constant / policy | Value | TypeScript source |
| --- | --- | --- |
| RPC path | `/roost.v1.CoordinatorService/<Method>` | `protocol/proto/roost/v1/coordinator.proto:936-1102` |
| Router service calls | exactly `1` | `apps/coord/src/rpc/router.ts:113-137` |
| Handler factory spreads | `18` | `apps/coord/src/rpc/router.ts:119-136` |
| Protected principal marker | `x-roost-auth-layer: device` | `apps/coord/src/auth/auth-interceptor.ts:256-277` |
| Terminal-search owner | device fingerprint + required `x-roost-tab-id` | `apps/coord/src/auth/auth-interceptor.ts:280-293` |
| Audit page maximum | `500` | `apps/coord/src/rpc/handlers-system.ts:311-314` |
| Worker list/deploy message timeout | `10,000 ms` | `apps/coord/src/attachments/handlers-attachments.ts:51-101` (representative handler timeout) |
| File read chunk maximum | `4 MiB` | `apps/coord/src/attachments/handlers-attachments.ts:65-70` |

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
| `Unimplemented` | `501` | Retired/moved surface such as Connect `Sync`. |
| `Unavailable` | `503` | Required worker/socket is offline or not routable. |
| `DeadlineExceeded` | `504` | Bounded downstream wait expired. |

Unhandled errors map to `500`. Mapping is implemented in `apps/coord/src/auth/auth-interceptor.ts:58-78`; the interceptor itself does not reject public methods, and handler helpers produce the actual Connect error.

## Reference implementation

**Handler spread index**

The method list is the `service CoordinatorService` block in `protocol/proto/roost/v1/coordinator.proto`. The implementation spreads these factories into its one `router.service()` literal:

| Domain | Handler file | Auth requirement |
| --- | --- | --- |
| Workers | `apps/coord/src/workers/handlers-workers.ts` | `WorkersRegister/Heartbeat`: worker; list/rename/delete/deploy: device. |
| Keeper update | `apps/coord/src/deploy/handlers-workers-update.ts` | Device plus update/on-host policy. |
| Sessions | `apps/coord/src/sessions/handlers-sessions.ts` | `SessionsList`: device, or worker restricted to its own open recovery; all other methods: device. |
| Agent status | `apps/coord/src/agents/handlers-agent-status.ts` | Device. |
| Agent prompt | `apps/coord/src/agents/handlers-agent-prompt.ts` | Device plus authorized open session/status fence. |
| Workspaces | `apps/coord/src/sessions/handlers-workspaces.ts` | Device. |
| Tasks | `apps/coord/src/sessions/handlers-tasks.ts` | Device. |
| MCP | `apps/coord/src/sessions/handlers-mcp.ts` | Device. |
| Auth/pair/devices | `apps/coord/src/auth/handlers-auth.ts` | Mixed: public identity/bootstrap redemption; device mint/logout/devices; pairing public-token/on-host/device rules per [`auth-and-pairing.md`](auth-and-pairing.md). |
| System/diagnostics/audit | `apps/coord/src/rpc/handlers-system.ts` | Health public; DB export URL device + on-host; remaining implemented methods device. |
| Transcription | `apps/coord/src/rpc/handlers-transcription.ts` | Device. |
| Agent config | `apps/coord/src/agents/handlers-agent-config.ts` | Device. |
| Attachments/files | `apps/coord/src/attachments/handlers-attachments.ts` | Device. |
| Attachment direct | `apps/coord/src/attachments/handlers-attachments-direct.ts` | Device plus exact tab/grant descriptor. |
| Attachment peer | `apps/coord/src/attachments/handlers-attachments-peer.ts` | Device plus exact tab/grant/worker fence. |
| UI | `apps/coord/src/ui-state/handlers-ui.ts` | Device plus persisted session/current socket generation. |
| Push | `apps/coord/src/push/handlers-push.ts` | Device. |
| Streaming compatibility | `apps/coord/src/rpc/handlers-streaming.ts` | Device then `Unimplemented`; live fetch path rejects Connect Sync with HTTP 410. |

The proto also declares managed-account, dashboard, coordinator-move/relocation, and other methods not present in the current assembled `makeAuthHandlers` implementation. Proto presence alone is not current router implementation.

### Assembly and policy

- Single service assembly: `apps/coord/src/rpc/router.ts`
- JWT/principal/context/write-gate/audit policy: `apps/coord/src/auth/auth-interceptor.ts`
- Persisted principal resolution: `apps/coord/src/auth/auth-principal.ts`
- Current Sync WebSocket: [`sync.md`](sync.md)
