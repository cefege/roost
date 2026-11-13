import { setTimeout as sleep } from "node:timers/promises";
import type { BrowserContext, Route } from "playwright";
import { agentOmpUiPath, NativeOmpUiChannel } from "./native-ompui";

export type TransportDirection = "incoming" | "outgoing";
export type BrowserEventRecorder = (channel: string, value: unknown) => Promise<void>;
/** `roost_protocol::local_ui_door::DEFAULT_WORKER_LOCAL_UI_ORIGIN`: the door every coordinator-served page probes. */
export const hostDefaultDoorOrigin = "http://127.0.0.1:4114";

export class BrowserTransport {
  readonly #pending = new Set<Promise<void>>();
  /** The real client's latest AgentOmpUi identity, for the native-contract-conformance lane. */
  readonly ompUi = new NativeOmpUiChannel();
  readonly #delays: Record<TransportDirection, number> = { incoming: 0, outgoing: 0 };
  readonly #connectTails: Record<TransportDirection, Promise<void>> = { incoming: Promise.resolve(), outgoing: Promise.resolve() };
  readonly #origins: Set<string>;
  #closing = false;

  constructor(
    origins: string[],
    readonly record: BrowserEventRecorder,
    readonly hostEffect: BrowserEventRecorder,
    readonly fail: (error: Error) => void,
  ) {
    this.#origins = new Set(origins.map(origin => new URL(origin).origin));
  }

  async install(context: BrowserContext): Promise<void> {
    await context.route("**/*", route => {
      const task = this.#routeHTTP(route);
      this.#track(task);
      return task.catch(() => undefined);
    });
    await context.routeWebSocket(/.*/, socket => {
      const url = new URL(socket.url());
      url.protocol = url.protocol === "wss:" ? "https:" : "http:";
      if (!this.#origins.has(url.origin)) {
        this.fail(new Error(`Unapproved browser WebSocket origin: ${url.origin}`));
        this.#track(socket.close({ code: 1008, reason: "Unapproved parity origin" }));
        return;
      }
      const server = socket.connectToServer();
      if (url.pathname !== "/ws/coord-sync") return;
      const tails: Record<TransportDirection, Promise<void>> = { incoming: Promise.resolve(), outgoing: Promise.resolve() };
      socket.onMessage(message => this.#enqueue(tails, "outgoing", async () => {
        server.send(message);
        await this.record("websocket", { direction: "outgoing", bytes: Buffer.byteLength(message), binary: typeof message !== "string" });
      }));
      server.onMessage(message => this.#enqueue(tails, "incoming", async () => {
        socket.send(message);
        await this.record("websocket", { direction: "incoming", bytes: Buffer.byteLength(message), binary: typeof message !== "string" });
      }));
      socket.onClose((code, reason) => this.#enqueue(tails, "outgoing", () => server.close({ code, reason })));
      server.onClose((code, reason) => this.#enqueue(tails, "incoming", () => socket.close({ code, reason })));
    });
  }

  async setDelay(milliseconds: number, direction: TransportDirection | "both" = "incoming"): Promise<void> {
    if (!Number.isSafeInteger(milliseconds) || milliseconds < 0) throw new Error("Transport delay must be a nonnegative integer");
    if (direction === "both" || direction === "incoming") this.#delays.incoming = milliseconds;
    if (direction === "both" || direction === "outgoing") this.#delays.outgoing = milliseconds;
    await this.record("delay", { milliseconds, direction });
  }

  async drain(): Promise<void> {
    while (this.#pending.size > 0) await Promise.all([...this.#pending]);
  }

  beginClose(): void {
    this.#closing = true;
    this.#delays.incoming = 0;
    this.#delays.outgoing = 0;
  }

  async #routeHTTP(route: Route): Promise<void> {
    const request = route.request();
    const url = new URL(request.url());
    if (!this.#origins.has(url.origin)) {
      if (request.isNavigationRequest() && request.resourceType() === "document" && (url.protocol === "http:" || url.protocol === "https:")) {
        await this.hostEffect("builtin.host-open-url", url.href);
        await route.fulfill({
          status: 200,
          contentType: "text/html; charset=utf-8",
          headers: { "Content-Security-Policy": "default-src 'none'; img-src data:; base-uri 'none'; form-action 'none'" },
          body: "<!doctype html><html><head><meta charset=\"utf-8\"><link rel=\"icon\" href=\"data:,\"><title>Parity external navigation</title></head><body>External navigation captured by the isolated parity browser.</body></html>",
        });
        return;
      }
      // The product probes the machine's default worker door; this host's real worker must stay unreachable
      // under deny-all networking, which the client handles as its genuine "unreachable door" outcome.
      if (url.origin === hostDefaultDoorOrigin && url.pathname === "/api/local-bootstrap" && request.method() === "GET") {
        await this.record("door-probe-refused", { origin: url.origin });
        await route.abort("connectionrefused");
        return;
      }
      const error = new Error(`Unapproved browser network request: ${request.method()} ${url.origin}${url.pathname}`);
      this.fail(error);
      await route.abort("blockedbyclient");
      return;
    }
    if (!url.pathname.startsWith("/roost.v1.CoordinatorService/")) {
      await route.continue();
      return;
    }
    if (url.pathname === agentOmpUiPath && request.method() === "POST") await this.ompUi.observe(request);
    const dispatched = await this.#enqueue(this.#connectTails, "outgoing", async () => {
      const response = route.fetch({ maxRedirects: 0, timeout: 60_000 });
      await this.record("connect-request", { method: request.method(), path: url.pathname });
      return { response };
    });
    const received = await dispatched.response;
    try {
      await this.#enqueue(this.#connectTails, "incoming", async () => {
        await route.fulfill({ response: received });
        await this.record("connect-response", { path: url.pathname, status: received.status() });
      });
    } finally {
      await received.dispose();
    }
  }

  #enqueue<Value>(tails: Record<TransportDirection, Promise<void>>, direction: TransportDirection, deliver: () => Value | Promise<Value>): Promise<Value> {
    const due = performance.now() + this.#delays[direction];
    const delivery = tails[direction].then(async () => {
      const remaining = due - performance.now();
      if (remaining > 0) await sleep(remaining);
      return await deliver();
    });
    const retained = delivery.then(() => undefined, error => {
      if (!this.#closing) this.fail(error instanceof Error ? error : new Error(String(error)));
    });
    tails[direction] = retained;
    this.#pending.add(retained);
    void retained.then(() => this.#pending.delete(retained));
    return delivery;
  }

  #track(task: Promise<void>): void {
    const retained = task.catch(error => {
      if (!this.#closing) this.fail(error instanceof Error ? error : new Error(String(error)));
    });
    this.#pending.add(retained);
    void retained.then(() => this.#pending.delete(retained));
  }
}
