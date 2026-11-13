import { appendFileSync } from "node:fs";
import { createServer } from "node:http";

export type ServiceTrace = (service: string, value: Record<string, unknown>) => void;

export function serviceTrace(tracePath: string): ServiceTrace {
  return (service, value) => appendFileSync(tracePath, `${JSON.stringify({ channel: "builtin.service", value: { service, ...value } })}\n`);
}

export interface LocalHttpService {
  url: string;
  port: number;
  dispose(): Promise<void>;
}

/** Bun reports no port for unix-socket listeners; every owned fixture binds loopback TCP. */
export function listeningPort(server: { port?: number }): number {
  if (server.port === undefined) throw new Error("Owned fixture listener has no TCP port");
  return server.port;
}

export function startHttpService(
  service: string,
  trace: ServiceTrace,
  respond: (request: Request) => Response | Promise<Response>,
): LocalHttpService {
  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request) {
      const bytes = new Uint8Array(await request.clone().arrayBuffer());
      trace(service, { transport: "http", direction: "request", method: request.method, url: request.url, contentType: request.headers.get("content-type"), bytes: bytes.byteLength });
      const response = await respond(request);
      trace(service, { transport: "http", direction: "response", method: request.method, url: request.url, status: response.status });
      return response;
    },
  });
  let disposed = false;
  return {
    url: server.url.origin,
    port: listeningPort(server),
    async dispose() {
      if (disposed) return;
      disposed = true;
      await server.stop(true);
    },
  };
}

export function startRefusingRelay(input: { closeCode: number; reason: string }, trace: ServiceTrace): LocalHttpService {
  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch(request, server) {
      trace("collab-relay", { transport: "websocket", direction: "request", method: request.method, url: request.url });
      if (server.upgrade(request)) return;
      return new Response("WebSocket upgrade required", { status: 426 });
    },
    websocket: {
      open(socket) {
        trace("collab-relay", { transport: "websocket", direction: "response", status: 101 });
        socket.close(input.closeCode, input.reason);
      },
      message(_socket, message) {
        trace("collab-relay", { transport: "websocket", direction: "message", bytes: typeof message === "string" ? Buffer.byteLength(message) : message.byteLength });
      },
      close(_socket, code, reason) {
        trace("collab-relay", { transport: "websocket", direction: "close", code, reason });
      },
    },
  });
  let disposed = false;
  return {
    url: `ws://127.0.0.1:${server.port}`,
    port: listeningPort(server),
    async dispose() {
      if (disposed) return;
      disposed = true;
      await server.stop(true);
    },
  };
}

export async function startDenyProxy(trace: ServiceTrace): Promise<LocalHttpService> {
  const server = createServer((request, response) => {
    trace("network-deny", { transport: "http", direction: "request", method: request.method, url: request.url });
    response.writeHead(403, { "Content-Type": "text/plain", Connection: "close" });
    response.end("Parity denies remote traffic");
  });
  server.on("connect", (request, socket) => {
    trace("network-deny", { transport: "http", direction: "request", method: "CONNECT", url: request.url });
    socket.end("HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
  });
  const listening = Promise.withResolvers<void>();
  server.once("error", listening.reject);
  server.listen(0, "127.0.0.1", () => { server.off("error", listening.reject); listening.resolve(); });
  await listening.promise;
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Parity deny proxy did not bind loopback TCP");
  let disposed = false;
  return {
    url: `http://127.0.0.1:${address.port}`,
    port: address.port,
    async dispose() {
      if (disposed) return;
      disposed = true;
      server.closeAllConnections();
      const closed = Promise.withResolvers<void>();
      server.close(error => error ? closed.reject(error) : closed.resolve());
      await closed.promise;
    },
  };
}
