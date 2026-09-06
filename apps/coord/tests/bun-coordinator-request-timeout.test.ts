// Pins Bun request-timeout routing for the coordinator's two HTTP listeners.
// The fixture avoids wall-clock sleeps by observing per-request timeout calls.
// SessionsPrompt is the only route allowed to override the 120-second default.

import { describe, expect, test } from "bun:test";
import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import { AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS } from "@roost/shared/terminal-input";
import { startBunCoordinatorListeners } from "../src/bun-coordinator-listeners.ts";

const PRIOR_IDLE_BOUNDARY_MS = 120_000;
const SESSIONS_PROMPT_PATH =
  `/${CoordinatorService.typeName}/${CoordinatorService.method.sessionsPrompt.name}`;

type ListenerName = "private" | "public";

interface TimeoutCall {
  listener: ListenerName;
  request: Request;
  seconds: number;
}

interface ListenerServer {
  listener: ListenerName;
  port: number;
  requestIP(request: Request): { address: string };
  upgrade(): boolean;
  timeout(request: Request, seconds: number): void;
}

interface CapturedServeOptions {
  idleTimeout?: number;
  fetch?: (
    request: Request,
    server: ListenerServer,
  ) => Response | undefined | Promise<Response | undefined>;
}

function makeListenerServer(
  listener: ListenerName,
  timeoutCalls: TimeoutCall[],
): ListenerServer {
  return {
    listener,
    port: listener === "private" ? 4102 : 4104,
    requestIP: () => ({ address: "127.0.0.1" }),
    upgrade: () => false,
    timeout(request, seconds): void {
      timeoutCalls.push({ listener, request, seconds });
    },
  };
}

function makeListenerFixture() {
  const timeoutCalls: TimeoutCall[] = [];
  const delegatedListeners: string[] = [];
  const serveOptions: CapturedServeOptions[] = [];
  const servers = [
    makeListenerServer("private", timeoutCalls),
    makeListenerServer("public", timeoutCalls),
  ];
  const serve = ((options: unknown) => {
    const server = servers[serveOptions.length];
    if (!server) throw new Error("unexpected third Bun listener");
    serveOptions.push(options as CapturedServeOptions);
    return server;
  }) as unknown as typeof Bun.serve;

  startBunCoordinatorListeners({
    cfg: {
      bind: "127.0.0.1:4102",
      publicBind: "127.0.0.1:4104",
      trustProxy: false,
      saasMode: true,
      managedContainer: true,
      dbPath: "/tmp/request-timeout.db",
      authorizedKeysPath: "/tmp/authorized_keys",
      webDistPath: undefined,
      coordKeyPath: "/tmp/coord-key",
      jwtMaxAgeSecs: 300,
      auditRetentionDays: 90,
      corsAllowedOrigins: ["https://caller.example"],
      pushAllowedOrigins: [],
      relaxedCsp: false,
      logDir: "/tmp",
      tlsCertPath: undefined,
      tlsKeyPath: undefined,
      publicUrl: "https://private.example",
      webPublicUrl: "https://public.example",
      cfAccessTeamDomain: undefined,
      cfAccessAud: undefined,
      handoffPath: "/tmp/handoff.json",
    },
    coord: {
      async fetch(
        _request: Request,
        context?: { origin: { listener: string } },
      ): Promise<Response> {
        delegatedListeners.push(context?.origin.listener ?? "missing");
        return new Response("coord");
      },
      dispose() {},
    },
    sqlite: {},
    move: { gate: { mode: "active" } },
    workerDeps: {},
    syncDeps: {},
    workerWs: { open() {}, message() {}, close() {} },
    syncWs: { open() {}, message() {}, drain() {}, close() {} },
    spa: () => new Response("spa"),
    _serve: serve,
  } as unknown as Parameters<typeof startBunCoordinatorListeners>[0]);

  expect(serveOptions).toHaveLength(2);
  return { delegatedListeners, serveOptions, servers, timeoutCalls };
}

async function dispatch(
  options: CapturedServeOptions,
  server: ListenerServer,
  request: Request,
): Promise<Response> {
  if (!options.fetch) throw new Error("captured listener has no fetch handler");
  const response = await options.fetch(request, server);
  if (!response) throw new Error("request unexpectedly upgraded");
  return response;
}

describe("coordinator Bun request idle timeout", () => {
  test("both actual listeners keep the maximum SessionsPrompt wait alive", async () => {
    expect(AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS).toBe(300_000);
    expect(AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS).toBeGreaterThan(PRIOR_IDLE_BOUNDARY_MS);
    const fixture = makeListenerFixture();

    for (let idx = 0; idx < fixture.serveOptions.length; idx++) {
      const request = new Request(`https://coord.example${SESSIONS_PROMPT_PATH}`, {
        method: "POST",
        headers: { authorization: "Bearer test-device" },
      });
      await dispatch(fixture.serveOptions[idx]!, fixture.servers[idx]!, request);
      expect(fixture.timeoutCalls[idx]?.request).toBe(request);
    }

    expect(fixture.serveOptions.map((options) => options.idleTimeout))
      .toEqual([120, 120]);
    expect(fixture.delegatedListeners).toEqual(["direct", "public-edge"]);
    expect(fixture.timeoutCalls.map(({ listener, seconds }) => ({ listener, seconds })))
      .toEqual([
        { listener: "private", seconds: 0 },
        { listener: "public", seconds: 0 },
      ]);
  });

  test("unrelated requests inherit both listeners' 120-second default", async () => {
    const fixture = makeListenerFixture();
    const unrelatedRequests = [
      new Request(`https://coord.example${SESSIONS_PROMPT_PATH}`, { method: "GET" }),
      new Request("https://coord.example/roost.v1.CoordinatorService/SessionsInput", {
        method: "POST",
        headers: { authorization: "Bearer test-device" },
      }),
      new Request(`https://coord.example${SESSIONS_PROMPT_PATH}Extra`, {
        method: "POST",
        headers: { authorization: "Bearer test-device" },
      }),
      new Request("https://coord.example/api/health"),
    ];

    for (let idx = 0; idx < fixture.serveOptions.length; idx++) {
      for (const request of unrelatedRequests) {
        await dispatch(fixture.serveOptions[idx]!, fixture.servers[idx]!, request);
      }
    }

    expect(fixture.serveOptions.map((options) => options.idleTimeout))
      .toEqual([120, 120]);
    expect(fixture.timeoutCalls).toEqual([]);
  });
});
