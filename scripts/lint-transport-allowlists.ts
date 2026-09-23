// Allowlists for the single-transport invariants enforced by lint-roost.ts:
// which files may construct a raw WebSocket client, and which may run a
// WebSocket-upgrading listener. Data only, kept beside the rules that read it
// so growing an allowlist is a visible, reviewable edit rather than a line
// buried in the linter.

/** Files permitted to construct `new WebSocket(...)`. */
export const WEB_SOCKET_CLIENT_ALLOW: readonly string[] = [
  // deepgramDictation dials Deepgram's live STT endpoint — a genuine external
  // WS, not an intra-Roost transport.
  "apps/web/src/lib/deepgramDictation.ts",
  "apps/worker/src/transport/coord-link.ts",
  // sync.ts holds the canonical web↔coord Sync-stream client (the only
  // web-side sync WebSocket; the SPA analog of CoordLink on the worker).
  "apps/web/src/store/sync.ts",
  // local-terminal.ts dials the worker's own loopback door for sessions whose
  // PTY is on this machine — the second intra-Roost transport the SPA owns,
  // and the only one that survives a coordinator outage.
  "apps/web/src/ws/local-terminal.ts",
  // attachment-loopback owns the browser's one direct attachment socket per upload.
  "apps/web/src/ws/attachment-loopback.ts",
];

/** Files permitted to run `Bun.serve({ websocket })`. */
export const WEB_SOCKET_LISTENER_ALLOW: readonly string[] = [
  "apps/coord/src/main.ts",
  // The worker's loopback UI door: it serves the SPA and upgrades local
  // terminal sockets, so it is the one inbound listener the worker owns.
  "apps/worker/src/local-ui-server.ts",
];
