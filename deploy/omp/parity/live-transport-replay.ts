import { appendFileSync, readFileSync, realpathSync } from "node:fs";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { assertOwned } from "./profile-manifest";

const signaling = "https://chatgpt.com/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas";
let installed = false;

export function installLiveTransportReplay(): void {
  const destination = process.env.OMP_PARITY_LIVE_URL;
  if (!destination) return;
  if (installed) throw new Error("Live transport replay was installed twice");
  if (process.platform !== "linux") throw new Error("Positive live transport fixtures require Linux");
  const workspace = realpathSync(process.env.OMP_PARITY_WORKSPACE ?? "");
  const home = realpathSync(process.env.HOME ?? "");
  const profile = JSON.parse(readFileSync(join(workspace, ".parity-profile.json"), "utf8"));
  if (profile.version !== 1 || realpathSync(profile.workspace) !== workspace || home !== realpathSync(profile.home) || home !== join(workspace, ".parity-home")) throw new Error("Live replay requires its owned profile");
  assertOwned(home, realpathSync(process.env.ALSA_CONFIG_PATH ?? ""));
  assertOwned(home, realpathSync(process.env.OMP_PARITY_LIVE_WAV ?? ""));
  const trace = realpathSync(process.env.OMP_PARITY_NETWORK_TRACE ?? "");
  assertOwned(home, trace);
  const local = new URL(destination);
  if (local.protocol !== "http:" || !["127.0.0.1", "[::1]"].includes(local.hostname) || local.username || local.password || local.search || local.hash || local.pathname !== "/") throw new Error("Live fixture URL must be an explicit loopback HTTP origin");
  if (!(process.env.OMP_PARITY_ALLOWED_ORIGINS ?? "").split(",").includes(local.origin)) throw new Error("Live fixture origin is not allowed by the network guard");
  const guardedFetch = globalThis.fetch;
  const ActualWebSocket = globalThis.WebSocket;
  let events = 0;
  const record = (value: Record<string, unknown>): void => {
    if (++events > 1024) throw new Error("Live replay exceeded bounded wire trace");
    appendFileSync(trace, `${JSON.stringify({ type: "live-replay", ...value })}\n`);
  };
  const wire = (data: string): Record<string, unknown> => {
    if (Buffer.byteLength(data) > 2 * 1024 * 1024) throw new Error("Live replay payload exceeded bound");
    return { raw: data, sha256: createHash("sha256").update(data).digest("hex") };
  };
  const mapped = (original: string, socket: boolean): string | undefined => {
    const url = new URL(original);
    if (!socket && url.href === signaling) return new URL(`${url.pathname}${url.search}`, local).href;
    if (socket && url.protocol === "wss:" && url.hostname === "api.openai.com" && !url.port && !url.username && !url.password && !url.search && !url.hash && /^\/v1\/live\/rtc_[\w-]+$/.test(url.pathname)) {
      const target = new URL(url.pathname, local); target.protocol = "ws:"; return target.href;
    }
    return undefined;
  };
  const replayFetch: typeof fetch = Object.assign(async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const original = input instanceof Request ? input.url : String(input);
    const target = mapped(original, false);
    if (!target) return guardedFetch(input, init);
    const request = new Request(input, init);
    const routedOptions = { ...init } as RequestInit & { proxy?: unknown };
    const originalProxy = Boolean(routedOptions.proxy);
    delete routedOptions.proxy;
    record({ transport: "fetch", phase: "request", original, mapped: target, method: request.method, originalProxy, ...wire(await request.clone().text()) });
    const response = await guardedFetch(new Request(target, request), routedOptions);
    record({ transport: "fetch", phase: "response", original, mapped: target, status: response.status, contentType: response.headers.get("content-type"), location: response.headers.get("location"), ...wire(await response.clone().text()) });
    return response;
  }, { preconnect: guardedFetch.preconnect.bind(guardedFetch) });
  const ReplayWebSocket = new Proxy(ActualWebSocket, {
    construct(target, args, newTarget) {
      const original = String(args[0]);
      const destination = mapped(original, true);
      if (!destination) {
        const url = new URL(original);
        const httpOrigin = `${url.protocol === "wss:" ? "https:" : url.protocol === "ws:" ? "http:" : url.protocol}//${url.host}`;
        if (!(process.env.OMP_PARITY_ALLOWED_ORIGINS ?? "").split(",").includes(httpOrigin)) throw new Error(`Unexpected OMP parity WebSocket destination: ${url.origin}${url.pathname}`);
        return Reflect.construct(target, args, newTarget);
      }
      const options = args.slice(1).map(value => {
        if (!value || typeof value !== "object" || Array.isArray(value)) return value;
        const routed = { ...value }; delete routed.proxy; return routed;
      });
      record({ transport: "websocket", phase: "connect", original, mapped: destination, originalProxy: args.slice(1).some(value => Boolean(value && typeof value === "object" && value.proxy)) });
      const socket = Reflect.construct(target, [destination, ...options], newTarget) as WebSocket;
      socket.addEventListener("message", event => {
        const data = typeof event.data === "string" ? event.data : ArrayBuffer.isView(event.data) ? Buffer.from(event.data.buffer, event.data.byteOffset, event.data.byteLength).toString("utf8") : event.data instanceof ArrayBuffer ? Buffer.from(event.data).toString("utf8") : undefined;
        if (data === undefined) throw new Error("Live replay expected text or byte sideband protocol");
        record({ transport: "websocket", phase: "message", original, mapped: destination, ...wire(data) });
      });
      socket.addEventListener("open", () => record({ transport: "websocket", phase: "open", original, mapped: destination }));
      socket.addEventListener("close", event => record({ transport: "websocket", phase: "close", original, mapped: destination, code: event.code, reason: event.reason }));
      socket.addEventListener("error", () => record({ transport: "websocket", phase: "error", original, mapped: destination }));
      const send = socket.send;
      socket.send = function(data) {
        if (typeof data === "string") record({ transport: "websocket", phase: "send", original, mapped: destination, ...wire(data) });
        return send.call(this, data);
      };
      return socket;
    },
  });
  globalThis.fetch = replayFetch;
  globalThis.WebSocket = ReplayWebSocket;
  installed = true;
  record({ phase: "installed", localOrigin: local.origin, signaling });
}
