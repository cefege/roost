// Sync-WS handshake constants shared by every client of the coordinator
// sync stream (web store, roost-cli, coord handler). These values are wire
// protocol: the coord upgrade path matches on them exactly, so they change
// only with a negotiation bump (sync_v), never in place.

export const SYNC_WS_PATH = "/ws/coord-sync";
export const SYNC_AUTH_SUBPROTOCOL = "roost-auth";

/** Capability-negotiation query values understood by sync-ws-handler.ts. */
export const SYNC_QUERY_FLOW_V1 = "1";
export const SYNC_QUERY_V2 = "2";
