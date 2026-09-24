// The worker's loopback UI door: its default bind and the origin a browser
// reaches it at. Imported by the coordinator, the worker, and the SPA, so this
// file has no imports at all — the browser bundle must not pull `zod` or
// `node:path` in to learn a port number.

/** The bind an unset `ROOST_WORKER_LOCAL_UI_BIND` resolves to, and the origin a
 * browser reaches it at. Loopback only: this door upgrades terminal sockets for
 * the PTYs on the worker's own machine, so any non-loopback interface would hand
 * them to the network. The coordinator pre-allowlists the origin below for CORS,
 * Sync WS, and the SPA's CSP `connect-src`, and the SPA probes it to discover a
 * worker on the browser's own machine. An operator who moves the port must both
 * allowlist the new origin on the coordinator and set the browser-side
 * `roost.localWorkerOrigin` override the SPA reads. */
export const DEFAULT_WORKER_LOCAL_UI_BIND = "127.0.0.1:4104";
export const DEFAULT_WORKER_LOCAL_UI_ORIGIN = "http://127.0.0.1:4104";
