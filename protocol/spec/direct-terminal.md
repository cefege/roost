<!-- Direct-terminal contract: grant-gated loopback/WebRTC carrier election, framing, and outage behavior. -->
<!-- Endpoint and channel literals are owned by @roost/protocol/terminal-peer and local-ui-door. -->
<!-- STUN is discovery only; coordinator remains grant and signaling authority. -->

# Direct terminal

## Purpose

Direct terminal is an optional carrier for the same authoritative cell protocol carried by Sync. Per session the browser prefers same-worker loopback, then a qualified authenticated WebRTC peer, then ready Sync. A candidate has no canonical effect until its grant and complete validated baseline are current.

## Messages

| Message / label | File | Meaning |
| --- | --- | --- |
| `SessionsGrantLocalTerminalRequest/Response` | `protocol/proto/roost/v1/coordinator.proto:237-254` | Account-device requests exact session/worker/tab scope; response returns secret only after worker ACK. |
| `SessionsNegotiateLocalTerminalPeerRequest/Response` | `protocol/proto/roost/v1/coordinator.proto:278-291` | Bounded offer/answer for exact worker epoch and grant. |
| `LocalTerminalHello`, `LocalTerminalReady`, `LocalTerminalClientFrame`, `LocalTerminalServerFrame` | `protocol/proto/roost/v1/local_terminal.proto:20-89` | Carrier authentication, readiness, view/input/scrollback, cells, probes, and closure. |
| `roost-terminal-control-v1`, `roost-terminal-data-v1`, `roost-terminal-history-v1` | `packages/protocol/src/terminal-peer.ts:90-109` | Ordered WebRTC lanes; protocol `roost.local-terminal.v1`. |
| `/ws/local-terminal`; `roost-local-terminal` | `apps/worker/src/local-ui-server.ts:57-59` | Worker loopback WebSocket path and subprotocol. |

## State machine

1. Browser keeps authenticated Sync connected as metadata/control/fallback. For visible demand it requests a grant naming exact sessions, worker fingerprint, and tab. Coordinator authorizes dashboard/session scope and asks the exact live worker to install a time-bounded memory-only grant; only a worker ACK reveals the secret.
2. If the browser is on the worker host, it opens loopback and presents `LocalTerminalHello` with grant, secret, tab, device, peer, and worker epoch. The worker revalidates the live grant and tuple before `LocalTerminalReady`.
3. Otherwise the browser sends an offer through the coordinator. The coordinator binds the authenticated device/tab, current grant, worker connection, and worker/process epoch, then relays only a bounded offer. The worker allocates a peer only for that admitted offer, returns answer/error, and validates the browser hello again.
4. STUN is opportunistic address discovery. The default is `stun:stun.cloudflare.com:3478`; an explicit empty operator list disables external discovery. Roost supplies no TURN relay or universal direct-path guarantee.
5. The direct candidate folds cell frames under the canonical full/delta rules. A valid complete baseline plus atomic promotion elects it. If `terminal-input-route-v1` is present, unsent input remains held until the worker acknowledges the exact route claim; older loopback retains its established no-replay behavior.
6. Visible routes republish desired view state every five seconds and require generation-matched acknowledgement within fifteen seconds. A visible WebRTC peer also runs content-free worker probes; two missed replies retire that peer.
7. On exact route loss, the browser retires only that token, stops candidate work, and repairs from a fresh Sync baseline plus worker input-route claim when available. It never replays accepted or ambiguous input. If neither direct nor Sync is ready, painted DOM remains and new input is rejected.

## Limits

| Constant | Value | TypeScript source |
| --- | ---: | --- |
| `TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER` | `32` | `packages/protocol/src/terminal-peer.ts:8` |
| `TERMINAL_PEER_MAX_NEGOTIATIONS_PER_WORKER` | `4` | `packages/protocol/src/terminal-peer.ts:9` |
| `TERMINAL_PEER_MAX_CONNECTIONS_PER_BROWSER_DOCUMENT` | `8` | `packages/protocol/src/terminal-peer.ts:10` |
| `TERMINAL_PEER_MAX_SESSIONS_PER_GRANT` | `256` | `packages/protocol/src/terminal-peer.ts:11` |
| `TERMINAL_PEER_MAX_PENDING_NEGOTIATIONS` / per device | `64 / 8` | `packages/protocol/src/terminal-peer.ts:12-13` |
| `TERMINAL_PEER_NEGOTIATION_DEADLINE_MS` | `15,000 ms` | `packages/protocol/src/terminal-peer.ts:15` |
| `TERMINAL_PEER_ICE_GATHERING_DEADLINE_MS` | `3,000 ms` | `packages/protocol/src/terminal-peer.ts:16` |
| `TERMINAL_PEER_NATIVE_ANSWER_DEADLINE_MS` | `8,000 ms` | `packages/protocol/src/terminal-peer.ts:17` |
| `TERMINAL_PEER_PACKET_STALL_MS` | `10,000 ms` | `packages/protocol/src/terminal-peer.ts:18` |
| `TERMINAL_PEER_HELLO_DEADLINE_MS` | `3,000 ms` | `packages/protocol/src/terminal-peer.ts:19` |
| `TERMINAL_PEER_HEARTBEAT_INTERVAL_MS` / probe deadline | `5,000 / 3,000 ms` | `packages/protocol/src/terminal-peer.ts:20-22` |
| `TERMINAL_PEER_SDP_MAX_UTF8_BYTES` / lines / line bytes | `65,536 / 512 / 4,096` | `packages/protocol/src/terminal-peer.ts:25-27` |
| `TERMINAL_PEER_SDP_MAX_CANDIDATES` | `64` | `packages/protocol/src/terminal-peer.ts:28` |
| `TERMINAL_PEER_MAX_MESSAGE_SIZE` | `16,384 bytes` | `packages/protocol/src/terminal-peer.ts:33` |
| Logical frame maxima: control / terminal / history | `128 KiB / 2 MiB / 64 MiB` | `packages/protocol/src/terminal-peer.ts:49-53` |
| `TERMINAL_PEER_STUN_URL_MAX_COUNT` | `4` | `packages/protocol/src/terminal-peer.ts:120` |

## Errors

- Worker terminal-peer error reason is one of `disabled`, `native_unavailable`, `invalid_offer`, `grant_unavailable`, `capacity`, `expired`, `connection_superseded`, `ice_failed` (`protocol/proto/roost/v1/worker_transport.proto:197-205`).
- `TerminalPeerPacketErrorCode`: `packet-size`, `packet-magic`, `packet-header`, `message-id`, `message-id-wrap`, `message-size`, `fragment-order`, `fragment-stalled`, `quota`, `allocation`, `clock`, `closed` (`packages/protocol/src/terminal-peer-packets.ts:25-37`).
- Invalid STUN configuration reports `ROOST_TERMINAL_PEER_STUN_URLS must contain 1 to 4 distinct stun: UDP URLs` or `... contains an invalid STUN URL` (`packages/protocol/src/terminal-peer.ts:122-123`).
- Direct diagnostics expose route state, probe age/RTT, candidate class, buffered bytes, worker epoch, and opaque peer ID; never SDP, candidate addresses, credentials, or terminal content.

## Reference implementation

- Shared limits/channels: `packages/protocol/src/terminal-peer.ts`, `packages/protocol/src/terminal-peer-packets.ts`
- Local carrier proto: `protocol/proto/roost/v1/local_terminal.proto`
- Worker socket/peer: `apps/worker/src/local-ui-server.ts`, `apps/worker/src/local-terminal-socket.ts`, `apps/worker/src/terminal-peer-owner.ts`
- Coordinator grants/signaling: `apps/coord/src/terminal/direct/`
- Browser election/peer: `apps/web/src/store/terminal-stream-transport.ts`, `apps/web/src/store/transport/terminal-peer.ts`, `apps/web/src/store/transport/terminal-input-router.ts`
