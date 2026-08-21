// Worker↔coord frame handling, shared by the raw-WS transport. This is the
// stable public facade — the implementation lives in three siblings:
//   - worker-registry.ts: the connectWorkers registry + routable-set accessors
//   - worker-send.ts:      getWorkerHubSocket + browser-command / attachment senders
//   - worker-conn.ts:      makeWorkerConn (per-connection link) + respawn-if-missing
// External callers import from THIS file; the exports below re-expose the
// siblings so no import path changes.
//
// TRANSPORT: this is NOT a Connect bidi. The worker dials a raw Bun
// WebSocket at /ws/coord-worker/:fp?token=<jwt> (worker-ws-handler.ts);
// CoordWorkerUp/Down proto frames ride it as binary. NEVER re-wire this
// as a Connect/gRPC bidi under Bun — it h2-tight-loops / h1.1-stalls /
// flaps. See docs/FAILURE-INDEX.md + project_worker_coord_raw_ws_not_connect_bidi.
//
// Auth: query-param JWT verified at WS upgrade (Bun's client WebSocket
// has no custom-header API), via verifyJwt (see worker-conn.ts).

export { __setConnectWorkerForTest, listRoutableFps } from "./worker-registry.ts";
export {
  getWorkerHubSocket, sendBrowserCommand, sendAttachmentChunk,
  sendTerminalInputRequest, sendTerminalStreamStateRequest,
  sendTerminalSnapshotRequest,
} from "./worker-send.ts";
export { makeWorkerConn } from "./worker-conn.ts";
export type { WorkerServiceDeps, WorkerConn } from "./worker-conn.ts";
