// Gates a browser on the worker's own machine must pass before it reaches this
// machine's PTYs: Host and Origin refusal, the bootstrap payload, the SPA
// hand-off, the terminal socket upgrade, and the non-loopback bind refusal.
// Drives the real listener over loopback HTTP/WS; no worker boot, no sessions.

import { afterEach, expect, test } from "bun:test";
import { DEFAULT_WORKER_LOCAL_UI_BIND } from "@roost/host/config";
import { loadWorkerConfig } from "../../src/host/config.ts";
import {
  LOCAL_BOOTSTRAP_PATH,
  LOCAL_TERMINAL_PATH,
  LOCAL_TERMINAL_SUBPROTOCOL,
  startLocalUiServer,
  type LocalUiServer,
} from "../../src/local-door/local-ui-server.ts";
import type { TerminalPacketPort } from "../../src/terminal/peer/terminal-packet-port.ts";

const COORDINATOR_URL = "http://coord.test:4102";
const WORKER_FP = "a".repeat(64);
const SPA_BODY = "<!doctype html><title>spa</title>";
const CONFIG_ENV = {
  ROOST_WORKER_DATA_DIR: "/tmp/roost-local-ui-config-test",
  ROOST_WORKER_LOG_DIR: "/tmp/roost-local-ui-config-test/logs",
};

interface SpaCall {
  path: string;
  method: string;
  acceptEncoding: string;
}

/** Awaits the callbacks the listener drives instead of a guessed duration:
 * `take()` resolves on the next delivery, `seen` answers "nothing arrived". */
interface Signals<T> {
  push(value: T): void;
  take(): Promise<T>;
  readonly seen: readonly T[];
}

interface Door {
  server: LocalUiServer;
  origin: string;
  spaCalls: SpaCall[];
  opened: Signals<TerminalPacketPort>;
  frames: Signals<Uint8Array>;
  closed: Signals<string>;
}

const started: LocalUiServer[] = [];

afterEach(() => {
  for (const server of started.splice(0)) server.close();
});

function signals<T>(): Signals<T> {
  const seen: T[] = [];
  const waiters: ((value: T) => void)[] = [];
  let read = 0;
  return {
    seen,
    push(value: T): void {
      seen.push(value);
      waiters.shift()?.(value);
    },
    take(): Promise<T> {
      if (read < seen.length) return Promise.resolve(seen[read++]!);
      return new Promise<T>((resolve) => {
        waiters.push((value) => {
          read++;
          resolve(value);
        });
      });
    },
  };
}

function startDoor(
  options: {
    bind?: string;
    coordinatorUrl?: string;
    allowedBrowserOrigins?: readonly string[];
  } = {},
): Door {
  const spaCalls: SpaCall[] = [];
  const opened = signals<TerminalPacketPort>();
  const frames = signals<Uint8Array>();
  const closed = signals<string>();
  const server = startLocalUiServer({
    bind: options.bind ?? "127.0.0.1:0",
    coordinatorUrl: options.coordinatorUrl ?? COORDINATOR_URL,
    workerFingerprint: WORKER_FP,
    allowedBrowserOrigins: options.allowedBrowserOrigins ?? [],
    spa: async (url, method, acceptEncoding) => {
      spaCalls.push({ path: url.pathname, method, acceptEncoding });
      return new Response(SPA_BODY, { status: 200, headers: { "content-type": "text/html" } });
    },
    terminal: {
      onOpen: (socket) => opened.push(socket),
      onMessage: (_socket, data) => frames.push(new Uint8Array(data)),
      onClose: (socket) => closed.push(socket.socketId),
    },
  });
  started.push(server);
  return { server, origin: `http://127.0.0.1:${server.port}`, spaCalls, opened, frames, closed };
}

async function openTerminalSocket(door: Door, protocols: string[]): Promise<WebSocket> {
  const client = new WebSocket(
    `ws://127.0.0.1:${door.server.port}${LOCAL_TERMINAL_PATH}`,
    protocols,
  );
  client.binaryType = "arraybuffer";
  await new Promise<void>((resolve, reject) => {
    client.onopen = () => resolve();
    client.onerror = () => reject(new Error("local terminal socket refused"));
  });
  return client;
}

test("a Host this door does not answer on is refused before routing", async () => {
  const door = startDoor();

  const rebound = await fetch(`${door.origin}${LOCAL_BOOTSTRAP_PATH}`, {
    headers: { host: "evil.example" },
  });

  expect(rebound.status).toBe(403);
  expect(await rebound.text()).toBe("");
  expect(door.spaCalls).toEqual([]);
});

test("a cross-origin caller is refused on every route", async () => {
  const door = startDoor();

  for (const path of ["/", LOCAL_BOOTSTRAP_PATH]) {
    const response = await fetch(`${door.origin}${path}`, {
      headers: { origin: "https://attacker.test" },
    });
    expect(response.status).toBe(403);
    expect(await response.text()).toBe("");
  }
  expect(door.spaCalls).toEqual([]);
});

test("every loopback authority is served, with or without an Origin", async () => {
  const door = startDoor();

  const sameOrigin = await fetch(`${door.origin}${LOCAL_BOOTSTRAP_PATH}`, {
    headers: { origin: door.origin },
  });
  const noOrigin = await fetch(`${door.origin}${LOCAL_BOOTSTRAP_PATH}`);
  // Each name that reaches a loopback listener is served, so a user who types
  // `localhost:<port>` gets the page. Driven as a header, never through DNS:
  // `localhost` resolves to ::1 first on some hosts, where this v4-only
  // listener would refuse the connection before the gate ever ran.
  const viaLocalhost = await fetch(`${door.origin}${LOCAL_BOOTSTRAP_PATH}`, {
    headers: { host: `localhost:${door.server.port}` },
  });
  const viaLocalhostOrigin = await fetch(`${door.origin}${LOCAL_BOOTSTRAP_PATH}`, {
    headers: {
      host: `localhost:${door.server.port}`,
      origin: `http://localhost:${door.server.port}`,
    },
  });

  expect(sameOrigin.status).toBe(200);
  expect(noOrigin.status).toBe(200);
  expect(viaLocalhost.status).toBe(200);
  expect(viaLocalhostOrigin.status).toBe(200);
});

test("bootstrap advertises exactly the coordinator and fingerprint, uncached", async () => {
  const door = startDoor();

  const response = await fetch(`${door.origin}${LOCAL_BOOTSTRAP_PATH}`);

  expect(response.status).toBe(200);
  expect(await response.json()).toEqual({
    coordinatorUrl: COORDINATOR_URL,
    workerFingerprint: WORKER_FP,
  });
  expect(response.headers.get("cache-control")).toBe("no-store");
  expect(response.headers.get("content-type")).toBe("application/json");
});

test("responses name only this door and its coordinator in connect-src", async () => {
  const plaintext = startDoor();
  const tunnelled = startDoor({ coordinatorUrl: "https://coord.example.test" });

  const overLoopback = await fetch(`${plaintext.origin}${LOCAL_BOOTSTRAP_PATH}`);
  const overTls = await fetch(`${tunnelled.origin}${LOCAL_BOOTSTRAP_PATH}`);

  expect(overLoopback.headers.get("content-security-policy"))
    .toContain("connect-src 'self' http://coord.test:4102 ws://coord.test:4102;");
  expect(overTls.headers.get("content-security-policy"))
    .toContain("connect-src 'self' https://coord.example.test wss://coord.example.test;");
  expect(overLoopback.headers.get("x-frame-options")).toBe("DENY");
});

test("the coordinator's own origin is admitted and answered with CORS", async () => {
  const door = startDoor();

  const response = await fetch(`${door.origin}${LOCAL_BOOTSTRAP_PATH}`, {
    headers: { origin: COORDINATOR_URL },
  });

  expect(response.status).toBe(200);
  expect(await response.json()).toEqual({
    coordinatorUrl: COORDINATOR_URL,
    workerFingerprint: WORKER_FP,
  });
  expect(response.headers.get("access-control-allow-origin")).toBe(COORDINATOR_URL);
  expect(response.headers.get("vary")).toContain("origin");
});

test("a local-network preflight from the coordinator's origin is answered", async () => {
  const door = startDoor();

  const response = await fetch(`${door.origin}${LOCAL_BOOTSTRAP_PATH}`, {
    method: "OPTIONS",
    headers: { origin: COORDINATOR_URL },
  });

  expect(response.status).toBe(204);
  expect(response.headers.get("access-control-allow-origin")).toBe(COORDINATOR_URL);
  expect(response.headers.get("access-control-allow-methods")).toBe("GET");
  expect(response.headers.get("access-control-allow-private-network")).toBe("true");
});

test("a configured extra origin is admitted and nothing else is", async () => {
  const door = startDoor({ allowedBrowserOrigins: ["https://dash.example"] });

  const admitted = await fetch(`${door.origin}${LOCAL_BOOTSTRAP_PATH}`, {
    headers: { origin: "https://dash.example" },
  });
  const refused = await fetch(`${door.origin}${LOCAL_BOOTSTRAP_PATH}`, {
    headers: { origin: "https://other.example" },
  });

  expect(admitted.status).toBe(200);
  expect(admitted.headers.get("access-control-allow-origin")).toBe("https://dash.example");
  expect(refused.status).toBe(403);
  expect(await refused.text()).toBe("");
  expect(door.spaCalls).toEqual([]);
});

test("unknown paths reach the injected SPA responder; writes do not", async () => {
  const door = startDoor();

  const page = await fetch(`${door.origin}/w/some-workspace`, {
    headers: { "accept-encoding": "gzip" },
  });
  const posted = await fetch(`${door.origin}/w/some-workspace`, { method: "POST" });

  expect(page.status).toBe(200);
  expect(await page.text()).toBe(SPA_BODY);
  expect(door.spaCalls).toHaveLength(1);
  expect(door.spaCalls[0]!.path).toBe("/w/some-workspace");
  expect(door.spaCalls[0]!.method).toBe("GET");
  expect(door.spaCalls[0]!.acceptEncoding).toContain("gzip");
  expect(posted.status).toBe(405);
});

test("the terminal socket carries binary frames both ways and reports close", async () => {
  const door = startDoor();
  const received = signals<Uint8Array>();

  const client = await openTerminalSocket(door, [LOCAL_TERMINAL_SUBPROTOCOL]);
  client.onmessage = (event) => received.push(new Uint8Array(event.data as ArrayBuffer));
  const socket = await door.opened.take();
  // A text frame carries no local terminal command. Frame order is guaranteed,
  // so the binary frame behind it arriving is what proves a drop, not a stall.
  client.send("not a frame");
  client.send(new Uint8Array([1, 2, 3]));
  const inbound = await door.frames.take();
  socket.send(new Uint8Array([9, 8]), "control");
  const outbound = await received.take();

  expect(client.protocol).toBe(LOCAL_TERMINAL_SUBPROTOCOL);
  expect([...inbound]).toEqual([1, 2, 3]);
  expect([...outbound]).toEqual([9, 8]);
  expect(door.frames.seen).toHaveLength(1);
  expect(socket.open).toBe(true);

  client.close();
  expect(await door.closed.take()).toBe(socket.socketId);
});

test("the terminal path refuses a plain request and a foreign subprotocol", async () => {
  const door = startDoor();

  const plain = await fetch(`${door.origin}${LOCAL_TERMINAL_PATH}`);
  const outcome = await new Promise<string>((resolve) => {
    const client = new WebSocket(
      `ws://127.0.0.1:${door.server.port}${LOCAL_TERMINAL_PATH}`,
      ["mallory-terminal"],
    );
    client.onopen = () => {
      client.close();
      resolve("opened");
    };
    client.onerror = () => resolve("refused");
    client.onclose = () => resolve("refused");
  });

  expect(plain.status).toBe(400);
  expect(outcome).toBe("refused");
  expect(door.opened.seen).toEqual([]);
});

test("a non-loopback bind throws and never takes the port", async () => {
  const probe = startDoor();
  const port = probe.server.port;
  probe.server.close();

  for (const bind of ["0.0.0.0:4104", "[::]:4104", "localhost:4104", "10.0.0.7:4104", "127.0.0.1"]) {
    expect(() => startDoor({ bind })).toThrow("ROOST_WORKER_LOCAL_UI_BIND must be 127.0.0.1:<port>");
  }
  expect(() => startDoor({ bind: `0.0.0.0:${port}` })).toThrow();

  const reachable = await fetch(`http://127.0.0.1:${port}${LOCAL_BOOTSTRAP_PATH}`).catch(() => null);
  expect(reachable).toBeNull();
});

test("worker config feeds the bind, admitted origins and SPA root from the env", () => {
  expect(loadWorkerConfig(CONFIG_ENV).localUiBind).toBe(DEFAULT_WORKER_LOCAL_UI_BIND);
  expect(loadWorkerConfig(CONFIG_ENV).localUiAllowedOrigins).toEqual([]);
  expect(loadWorkerConfig(CONFIG_ENV).webDistPath).toBeUndefined();

  const configured = loadWorkerConfig({
    ...CONFIG_ENV,
    ROOST_WORKER_LOCAL_UI_BIND: "127.0.0.1:4999",
    ROOST_WORKER_LOCAL_UI_ALLOWED_ORIGINS: " https://dash.example , https://alt.example ,, ",
    ROOST_WEB_DIST_PATH: "/opt/roost/web/dist",
  });

  expect(configured.localUiBind).toBe("127.0.0.1:4999");
  expect(configured.localUiAllowedOrigins).toEqual([
    "https://dash.example",
    "https://alt.example",
  ]);
  expect(configured.webDistPath).toBe("/opt/roost/web/dist");
});
