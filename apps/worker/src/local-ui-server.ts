// Worker-local loopback door: serves the SPA bundle and upgrades this machine's
// browser onto a direct terminal socket, so keystrokes and cell frames for local
// PTYs never traverse the coordinator. Worker boot owns its lifetime and injects
// the shared SPA responder plus the handlers that own local frame routing.
// Every rule here is fail-closed: a non-loopback bind never listens, a request
// whose Host is not this door's own is refused unread, and only this door's own
// loopback origins plus the coordinator's browser front door are admitted as an
// Origin.

import type { Server, ServerWebSocket } from "bun";
import { randomUUID } from "node:crypto";
import { applySecurityHeaders } from "@roost/shared/http-security";
import { log } from "@roost/shared/log";

export interface LocalTerminalSocket {
  readonly socketId: string;
  send(bytes: Uint8Array): number;
  close(code?: number, reason?: string): void;
  readonly open: boolean;
}

export interface LocalTerminalSocketHandlers {
  /** A newly upgraded local terminal socket. The handler owns capability
   * verification and refuses by closing the socket. */
  onOpen(socket: LocalTerminalSocket): void;
  onMessage(socket: LocalTerminalSocket, data: Uint8Array): void;
  onClose(socket: LocalTerminalSocket): void;
}

export interface LocalUiServer {
  port: number;
  close(): void;
}

export interface LocalUiServerDeps {
  bind: string;
  coordinatorUrl: string;
  workerFingerprint: string;
  /** Browser origins beyond this door's own loopback names that may discover it
   * and dial its terminal socket. The coordinator this worker dials is always
   * admitted; these cover a deployment whose browser front door differs. */
  readonly allowedBrowserOrigins: readonly string[];
  spa: (url: URL, method: string, acceptEncoding: string) => Promise<Response>;
  terminal: LocalTerminalSocketHandlers;
}

export const LOCAL_TERMINAL_SUBPROTOCOL = "roost-local-terminal";
export const LOCAL_TERMINAL_PATH = "/ws/local-terminal";
export const LOCAL_BOOTSTRAP_PATH = "/api/local-bootstrap";

/** The largest legitimate client frame is a MAX_INPUT_BYTES (64 KiB) paste
 * inside a protobuf envelope; 1 MiB leaves headroom for view and scrollback
 * commands while keeping a compromised page from queueing megabyte frames. */
const LOCAL_TERMINAL_MAX_PAYLOAD_BYTES = 1024 * 1024;
const WEBSOCKET_OPEN = 1;

interface LocalTerminalWsData {
  socketId: string;
  socket: LocalTerminalSocket | null;
}

export function startLocalUiServer(deps: LocalUiServerDeps): LocalUiServer {
  const { hostname, port: requestedPort } = parseLoopbackBind(deps.bind);
  const connectOrigins = coordinatorConnectOrigins(deps.coordinatorUrl);
  const bootstrapBody = JSON.stringify({
    coordinatorUrl: deps.coordinatorUrl,
    workerFingerprint: deps.workerFingerprint,
  });

  // Filled once the listener owns a port, so a `:0` bind still validates
  // against the port the browser actually dialed.
  const allowedHosts = new Set<string>();
  const allowedOrigins = new Set<string>();

  function secured(response: Response): Response {
    const headers = new Headers(response.headers);
    applySecurityHeaders(headers, false, false, connectOrigins);
    return new Response(response.body, {
      status: response.status,
      statusText: response.statusText,
      headers,
    });
  }

  function refused(status: number, reason: string, req: Request, url: URL): Response {
    log.warn("local-ui", "local_ui_rejected", {
      reason,
      status,
      method: req.method,
      path: url.pathname,
      host: req.headers.get("host"),
      origin: req.headers.get("origin"),
    });
    return secured(new Response(null, { status }));
  }

  /** A page on the coordinator's origin reads the bootstrap answer
   * cross-origin, so the allowance must be on the response. The probe sends no
   * credentials, so no allow-credentials is emitted. */
  function crossOriginHeaders(origin: string | null): Record<string, string> {
    if (origin === null || !allowedOrigins.has(origin)) return {};
    return { "access-control-allow-origin": origin, vary: "origin" };
  }

  /** Chrome's local-network-access check may preflight a public→loopback GET,
   * and a 405 there fails the probe with nothing to diagnose. */
  function preflightHeaders(origin: string | null): Record<string, string> {
    const cors = crossOriginHeaders(origin);
    if (Object.keys(cors).length === 0) return {};
    return {
      ...cors,
      "access-control-allow-methods": "GET",
      "access-control-allow-private-network": "true",
      "access-control-max-age": "600",
    };
  }

  /** A throw out of a frame handler must cost that socket, never the worker
   * process: a local page is authorized to reach PTYs, not to end them. */
  function guarded(socket: LocalTerminalSocket, stage: string, run: () => void): void {
    try {
      run();
    } catch (error) {
      log.error("local-ui", "local_terminal_socket_rejected", {
        socket_id: socket.socketId,
        reason: stage,
        error: error instanceof Error ? error.message : String(error),
      });
      try {
        socket.close(1011, "local terminal handler failed");
      } catch { /* socket already gone */ }
    }
  }

  const server = Bun.serve({
    hostname,
    port: requestedPort,
    // A loopback peer dies with an immediate FIN or RST, so this door has no
    // half-open zombie class to reap — and an idle local pane must survive a
    // night without traffic. Dead sockets are still caught by the websocket
    // handler's ping-backed default timeout.
    idleTimeout: 0,
    fetch: async (
      req: Request,
      listener: Server<LocalTerminalWsData>,
    ): Promise<Response | undefined> => {
      const url = new URL(req.url);
      // DNS rebinding: a page on any other name resolves to 127.0.0.1 and then
      // talks to this door with its own Host, so the Host itself is the gate.
      if (!allowedHosts.has(req.headers.get("host") ?? "")) {
        return refused(403, "host_not_local", req, url);
      }
      const origin = req.headers.get("origin");
      if (origin !== null && !allowedOrigins.has(origin)) {
        return refused(403, "origin_not_local", req, url);
      }

      if (url.pathname === LOCAL_BOOTSTRAP_PATH) {
        if (req.method === "OPTIONS") {
          return secured(new Response(null, {
            status: 204,
            headers: preflightHeaders(origin),
          }));
        }
        if (req.method !== "GET" && req.method !== "HEAD") {
          return refused(405, "bootstrap_method", req, url);
        }
        return secured(new Response(req.method === "HEAD" ? null : bootstrapBody, {
          status: 200,
          headers: {
            "content-type": "application/json",
            // The advertised coordinator follows the worker's own config; a
            // cached copy would outlive a redeploy that moved it.
            "cache-control": "no-store",
            ...crossOriginHeaders(origin),
          },
        }));
      }

      if (url.pathname === LOCAL_TERMINAL_PATH) {
        if (req.method !== "GET") return refused(405, "terminal_method", req, url);
        const offered = req.headers.get("sec-websocket-protocol")?.split(",") ?? [];
        if (!offered.some((value) => value.trim() === LOCAL_TERMINAL_SUBPROTOCOL)) {
          return refused(400, "terminal_subprotocol", req, url);
        }
        const socketId = randomUUID();
        const upgraded = listener.upgrade(req, {
          data: { socketId, socket: null } satisfies LocalTerminalWsData,
          headers: { "Sec-WebSocket-Protocol": LOCAL_TERMINAL_SUBPROTOCOL },
        });
        if (upgraded) return undefined;
        return refused(400, "terminal_not_upgradable", req, url);
      }

      if (req.method !== "GET" && req.method !== "HEAD") {
        return refused(405, "method_not_allowed", req, url);
      }
      return secured(await deps.spa(url, req.method, req.headers.get("accept-encoding") ?? ""));
    },
    websocket: {
      // One protobuf envelope per frame: compression would only add a
      // per-message allocation on a loopback hop that is already free.
      perMessageDeflate: false,
      maxPayloadLength: LOCAL_TERMINAL_MAX_PAYLOAD_BYTES,
      open(ws: ServerWebSocket<LocalTerminalWsData>): void {
        const socket: LocalTerminalSocket = {
          socketId: ws.data.socketId,
          send: (bytes: Uint8Array) => ws.send(bytes),
          close: (code?: number, reason?: string) => ws.close(code, reason),
          get open(): boolean {
            return ws.readyState === WEBSOCKET_OPEN;
          },
        };
        ws.data.socket = socket;
        log.info("local-ui", "local_terminal_socket_opened", { socket_id: socket.socketId });
        guarded(socket, "open", () => deps.terminal.onOpen(socket));
      },
      message(
        ws: ServerWebSocket<LocalTerminalWsData>,
        message: string | ArrayBuffer | Uint8Array,
      ): void {
        const socket = ws.data.socket;
        if (!socket) return;
        // Bun hands binary frames over as Buffer (a Uint8Array) or ArrayBuffer;
        // a text frame carries no local terminal command at all.
        const bytes = typeof message === "string"
          ? null
          : message instanceof Uint8Array
            ? message
            : new Uint8Array(message);
        if (!bytes) {
          log.warn("local-ui", "local_terminal_socket_rejected", {
            socket_id: socket.socketId,
            reason: "non_binary_frame",
          });
          return;
        }
        guarded(socket, "message", () => deps.terminal.onMessage(socket, bytes));
      },
      close(ws: ServerWebSocket<LocalTerminalWsData>): void {
        const socket = ws.data.socket;
        if (!socket) return;
        ws.data.socket = null;
        log.info("local-ui", "local_terminal_socket_closed", { socket_id: socket.socketId });
        guarded(socket, "close", () => deps.terminal.onClose(socket));
      },
    },
  });

  // Bun types `port` as optional because a unix-socket listener has none; this
  // one always binds loopback TCP, and a `:0` bind resolves here.
  const port = server.port as number;
  // The three names that reach a loopback listener. All are served: a user who
  // types `localhost:<port>` must get the page, not a blanket 403. The gate is
  // that any OTHER name — an attacker's domain pointed at 127.0.0.1 — arrives
  // with its own Host and is refused before routing.
  const canonicalHost = hostname === "::1" ? `[::1]:${port}` : `127.0.0.1:${port}`;
  for (const host of [`127.0.0.1:${port}`, `localhost:${port}`, `[::1]:${port}`]) {
    allowedHosts.add(host);
    allowedOrigins.add(`http://${host}`);
  }
  // Origins only, never hosts: a foreign page may discover this door and dial
  // it, but the Host it sends must still be this loopback listener's own.
  const admittedBrowserOrigins = browserOrigins(
    deps.coordinatorUrl,
    deps.allowedBrowserOrigins,
  );
  for (const origin of admittedBrowserOrigins) allowedOrigins.add(origin);
  log.info("local-ui", "local_ui_listening", {
    bind: canonicalHost,
    coordinator_url: deps.coordinatorUrl,
    browser_origins: admittedBrowserOrigins.length,
  });

  return {
    port,
    close: () => {
      server.stop(true);
    },
  };
}

/** This door upgrades sockets that write to this machine's PTYs, so it may only
 * ever answer on loopback: any other interface offers those terminals to every
 * host that can route to it. Refuse before listening rather than after. */
function parseLoopbackBind(bind: string): { hostname: string; port: number } {
  const match = /^(127\.0\.0\.1|\[::1\]):(\d{1,5})$/.exec(bind);
  const port = match ? Number(match[2]) : -1;
  if (!match || port > 65535) {
    throw new Error(
      `ROOST_WORKER_LOCAL_UI_BIND must be 127.0.0.1:<port> or [::1]:<port>; got ${bind}`,
    );
  }
  return { hostname: match[1] === "[::1]" ? "::1" : "127.0.0.1", port };
}

/** The local page talks to exactly two origins: this door and the coordinator
 * the worker itself dials, over HTTP and its WebSocket twin. */
function coordinatorConnectOrigins(coordinatorUrl: string): string[] {
  const url = new URL(coordinatorUrl);
  const wsScheme = url.protocol === "https:" ? "wss:" : "ws:";
  return [url.origin, `${wsScheme}//${url.host}`];
}

/** The dashboard is served by the coordinator this worker already dials, so that
 * origin is the one foreign page allowed to discover this door and dial its
 * terminal socket. Exact match only: a prefix or pattern on the origin would
 * admit an attacker's host that merely starts the same way. */
function browserOrigins(coordinatorUrl: string, extra: readonly string[]): string[] {
  const admitted: string[] = [];
  for (const raw of [coordinatorUrl, ...extra]) {
    let origin: string;
    try {
      origin = new URL(raw).origin;
    } catch {
      continue;
    }
    if (origin === "null") continue;
    admitted.push(origin);
  }
  return admitted;
}

