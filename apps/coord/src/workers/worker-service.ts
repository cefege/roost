// Stable public facade for worker↔coord frame handling; the exports below
// re-expose the siblings owning the registry, senders, and connection
// lifecycle. Transport is a raw Bun WebSocket at /ws/coord-worker/:fp authed
// by a two-value subprotocol JWT at upgrade (every query string is rejected).
// NEVER re-wire it as a Connect/gRPC bidi under Bun: it h2-tight-loops,
// h1.1-stalls, and flaps — see docs/FAILURE-INDEX.md's raw-WS entry.

export { __setConnectWorkerForTest, listRoutableFps } from "./worker-registry.ts";
export {
  getWorkerHubSocket, sendBrowserCommand, sendAttachmentChunk,
  sendTerminalInputRequest, sendTerminalStreamStateRequest,
  sendKeeperUpdatePreparation,
  sendTerminalSnapshotRequest,
} from "./worker-send.ts";
export { makeWorkerConn } from "./worker-conn.ts";
export type { WorkerServiceDeps, WorkerConn } from "./worker-conn-types.ts";
