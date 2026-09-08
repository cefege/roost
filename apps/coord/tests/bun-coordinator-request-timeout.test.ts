// Pins Bun request-timeout routing for the coordinator's loopback HTTP listener.
// The fixture avoids wall-clock sleeps by observing per-request timeout calls.
// SessionsPrompt is the only route allowed to override the 120-second default.

import { describe, expect, test } from "bun:test";
import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import { AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS } from "@roost/shared/terminal-input";
import { startBunCoordinatorListeners } from "../src/bun-coordinator-listeners.ts";

const PRIOR_IDLE_BOUNDARY_MS = 120_000;
const SESSIONS_PROMPT_PATH =
  `/${CoordinatorService.typeName}/${CoordinatorService.method.sessionsPrompt.name}`;

interface TimeoutCall {
  request: Request;
  seconds: number;
}

interface ListenerServer {
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

function makeListenerFixture() {
  const timeoutCalls: TimeoutCall[] = [];
  const delegatedListeners: string[] = [];
  const serveOptions: CapturedServeOptions[] = [];
  const server: ListenerServer = {
    port: 4103,
    requestIP: () => ({ address: "127.0.0.1" }),
    upgrade: () => false,
    timeout(request, seconds): void {
      timeoutCalls.push({ request, seconds });
    },
  };
  const serve = ((options: unknown) => {
    if (serveOptions.length > 0) throw new Error("unexpected second Bun listener");
    serveOptions.push(options as CapturedServeOptions);
    return server;
  }) as unknown as typeof Bun.serve;

  startBunCoordinatorListeners({
    cfg: {
      bind: "127.0.0.1:4103",
      trustProxy: false,
      dbPath: "/tmp/request-timeout.db",
      authorizedKeysPath: "/tmp/authorized_keys",
      webDistPath: undefined,
      jwtMaxAgeSecs: 300,
      auditRetentionDays: 90,
      corsAllowedOrigins: ["https://caller.example"],
      pushAllowedOrigins: [],
      relaxedCsp: false,
      logDir: "/tmp",
      publicUrl: "https://coord.example",
      webPublicUrl: "https://dashboard.example",
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
    workerDeps: {},
    syncDeps: {},
    workerWs: { open() {}, message() {}, close() {} },
    syncWs: { open() {}, message() {}, drain() {}, close() {} },
    spa: () => new Response("spa"),
    _serve: serve,
  } as unknown as Parameters<typeof startBunCoordinatorListeners>[0]);

  const options = serveOptions[0];
  if (!options) throw new Error("listener was never constructed");
  return { delegatedListeners, options, server, timeoutCalls };
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
  test("the listener keeps the maximum SessionsPrompt wait alive", async () => {
    expect(AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS).toBe(300_000);
    expect(AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS).toBeGreaterThan(PRIOR_IDLE_BOUNDARY_MS);
    const fixture = makeListenerFixture();
    const request = new Request(`https://coord.example${SESSIONS_PROMPT_PATH}`, {
      method: "POST",
      headers: { authorization: "Bearer test-device" },
    });

    await dispatch(fixture.options, fixture.server, request);

    expect(fixture.options.idleTimeout).toBe(120);
    expect(fixture.delegatedListeners).toEqual(["direct"]);
    expect(fixture.timeoutCalls).toEqual([{ request, seconds: 0 }]);
  });

  test("unrelated requests inherit the listener's 120-second default", async () => {
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

    for (const request of unrelatedRequests) {
      await dispatch(fixture.options, fixture.server, request);
    }

    expect(fixture.options.idleTimeout).toBe(120);
    expect(fixture.timeoutCalls).toEqual([]);
  });
});
