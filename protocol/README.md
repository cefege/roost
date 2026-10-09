<!-- Protocol contract index: source of truth for language-neutral Roost v1 behavior. -->
<!-- Rust implementations build on roost-proto and roost-protocol; this tree defines their wire meaning. -->
<!-- Client implementers start with the endpoint manifest, then follow the linked surface spec. -->

# Roost protocol contract

## Layout

| Path | Owns |
| --- | --- |
| `protocol/proto/roost/v1/` | Language-neutral protobuf sources. Package `roost.v1`; `crates/roost-proto/build.rs` compiles them into the Rust bindings on every build. |
| `protocol/spec/` | Normative state machines, limits, errors, and implementation anchors for each client-facing surface. |
| `protocol/conformance/` | Language-neutral vectors consumed by the final conformance runner. |

## Versioning

`protocol/proto/buf.yaml` configures Buf `version: v2`, lint `STANDARD` except `PACKAGE_VERSION_SUFFIX`, and breaking changes with `use: FILE`.

| Literal | Meaning | Rust source |
| --- | --- | --- |
| `attachment-transfer-peer-webrtc-v1` | Worker capability for the attachment-specific WebRTC carrier. | `crates/roost-protocol/src/versioning.rs` |
| `agent_tool_tunnel_v1` | Worker capability for the shared agent-tool tunnel. | `crates/roost-protocol/src/versioning.rs` |
| `roost-local-attachment-transfer-v1` | Worker loopback WebSocket subprotocol for attachment transfer. | `crates/roost-protocol/src/versioning.rs` |
| `roost-attachment-control-v1` | Ordered attachment WebRTC control-channel label. | `crates/roost-protocol/src/versioning.rs` |
| `roost-attachment-data-v1` | Ordered attachment WebRTC data-channel label. | `crates/roost-protocol/src/versioning.rs` |
| `terminal-peer-webrtc-v1` | Worker capability for direct terminal WebRTC. | `crates/roost-protocol/src/versioning.rs` |
| `terminal-input-route-v1` | Worker capability for acknowledged direct-terminal input-route handoff. | `crates/roost-protocol/src/versioning.rs` |
| `roost-terminal-control-v1` | Direct-terminal WebRTC control-channel label. | `crates/roost-protocol/src/versioning.rs` |
| `roost-terminal-data-v1` | Direct-terminal WebRTC terminal-lane label. | `crates/roost-protocol/src/versioning.rs` |
| `roost-terminal-history-v1` | Direct-terminal WebRTC history-lane label. | `crates/roost-protocol/src/versioning.rs` |
| `schema_version` | Explicit schema discriminator for versioned non-proto payloads and observations. | `crates/roost-protocol/src/versioning.rs`; `crates/roost-protocol/src/layout/document.rs` |
| `protocol_version` | Keeper contract compatibility discriminator. | `crates/roost-protocol/src/versioning.rs`; `crates/roost-protocol/src/keeper_update/contract.rs` |
| `sync_v` | Sync WebSocket negotiation discriminator; `2` selects domain generations and socket identity. | `crates/roost-protocol/src/wire/sync_ws.rs`; `crates/roost-coord/src/http/upgrade.rs` |

## Client endpoint manifest

| Surface | Path / label | Transport / auth | Defined in | Spec |
| --- | --- | --- | --- | --- |
| Connect-RPC | `POST /roost.v1.CoordinatorService/<Method>` | HTTPS; account-device or legacy-self-hosted JWT for protected methods; bootstrap/pair entry methods are separately authorized. | `protocol/proto/roost/v1/coordinator.proto`; `crates/roost-coord/src/rpc/` | [`spec/coordinator-rpc.md`](spec/coordinator-rpc.md) |
| Sync WS | `/ws/coord-sync?tab=<id>&since=<id>&flow=1&sync_v=2` | WebSocket subprotocol `roost-auth`; account-device JWT; no `tab` means read-only. | `crates/roost-protocol/src/wire/sync_ws.rs`; `crates/roost-coord/src/sync_ws/upgrade_admission.rs` | [`spec/sync.md`](spec/sync.md) |
| Worker link WS | `/ws/coord-worker/<64-hex fingerprint>` | Raw full-duplex WebSocket; worker JWT and exact worker identity; not a client surface. | `protocol/proto/roost/v1/worker_transport.proto`; `crates/roost-coord/src/worker_link/upgrade_admission.rs` | [`spec/worker-link.md`](spec/worker-link.md) |
| DB export | `GET /api/db-export` | Operator HTTP surface; on-host/account-device gate. | `crates/roost-coord/src/http/db_export.rs` | [`spec/coordinator-rpc.md`](spec/coordinator-rpc.md) |
| Worker local door | `127.0.0.1:4114`; `GET /api/local-bootstrap`; WS `/ws/local-terminal` with subprotocol `roost-local-terminal`; WS `/ws/local-attachment-transfer` with subprotocol `roost-local-attachment-transfer-v1` | Loopback only; exact coordinator-issued direct grant and descriptor. | `crates/roost-protocol/src/local_ui_door.rs`; `crates/roost-protocol/src/attachment_transfer/mod.rs`; `crates/roost-worker/src/door/` | [`spec/direct-terminal.md`](spec/direct-terminal.md), [`spec/attachments.md`](spec/attachments.md) |
| Terminal WebRTC | `roost-terminal-control-v1`, `roost-terminal-data-v1`, `roost-terminal-history-v1`; protocol `roost.local-terminal.v1` | DTLS/SCTP; coordinator-admitted exact grant, worker epoch, peer tuple, and hello. | `crates/roost-protocol/src/versioning.rs`; `protocol/proto/roost/v1/local_terminal.proto` | [`spec/direct-terminal.md`](spec/direct-terminal.md) |
| Attachment WebRTC | `roost-attachment-control-v1`, `roost-attachment-data-v1`; protocol `roost.attachment-transfer.v1` | DTLS/SCTP; short-lived exact upload descriptor. | `crates/roost-protocol/src/attachment_transfer/`; `protocol/proto/roost/v1/attachment_transfer.proto` | [`spec/attachments.md`](spec/attachments.md) |

## Rust owners

- `crates/roost-proto` — the generated messages and Connect service stubs, compiled from `protocol/proto/roost/v1/` by `connectrpc-build`. The only protobuf runtime.
- `crates/roost-protocol` — the I/O-free wire logic over those messages: the one event fold, wire constants (`src/versioning.rs`), framing, and the keeper-update and layout contracts. Builds for `wasm32`.

Crate import direction is the allowlist in `xtask/src/crate_dag.rs`, enforced by `cargo xtask lint`; the graph is in [`crates/README.md`](../crates/README.md). A future native client depends only on `roost-client-core` and `roost-protocol`.
