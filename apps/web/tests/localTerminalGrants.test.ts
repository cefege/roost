// Worker-scoped direct grants must coalesce demand without letting one worker's
// renewal replace another. These tests use the coordinator boundary directly so
// delayed credential responses can be fenced at the browser auth generation.

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";

interface SessionRow { status: string; worker_fp: string; }
interface GrantRequest { sessionIds: string[]; workerFp: string; tabId: string; }
interface GrantResponse {
  grantId: string; secret: string; ttlMs: number; workerEpoch: string;
  peerSupported: boolean; stunUrls: string[]; inputRouteSupported: boolean;
}

let authGeneration = 0;
let sessions: Record<string, SessionRow> = {};
let requests: GrantRequest[] = [];
let mint: (request: GrantRequest) => Promise<GrantResponse>;

mock.module("../src/store/root.ts", () => ({
  rootStore: {
    get auth_generation() { return authGeneration; },
    get sessions() { return sessions; },
    get workers() { return { "worker-a": {}, "worker-b": {} }; },
  },
}));
mock.module("../src/auth/tab-id.ts", () => ({ getTabId: () => "tab-test" }));
mock.module("../src/auth/web-key.ts", () => ({
  getCurrentWebKeyInfo: () => Promise.resolve({ fingerprint: "device-test", extractable: false }),
  getPublicKeyB64: () => Promise.resolve("test-key"),
  signCoordinatorJwt: () => Promise.resolve("test-jwt"),
}));
mock.module("../src/connect.ts", () => ({
  coordinatorBaseUrl: () => "http://coord.test",
  coordinatorRpcUrl: (path: string) => `http://coord.test${path}`,
  coordClient: { sessionsGrantLocalTerminal: (request: GrantRequest) => { requests.push(request); return mint(request); } },
}));

const grants = await import("../src/ws/local-terminal-grants.ts");

async function settle(): Promise<void> {
  for (let turn = 0; turn < 12; turn += 1) await Promise.resolve();
}
function response(workerFp: string, sessionIds: string[]): GrantResponse {
  return {
    grantId: `grant-${workerFp}-${sessionIds.join("-")}`,
    secret: `secret-${workerFp}`,
    ttlMs: 43_200_000,
    workerEpoch: `epoch-${workerFp}`,
    peerSupported: true,
    stunUrls: [],
    inputRouteSupported: true,
  };
}

beforeEach(() => {
  authGeneration = 0;
  sessions = {
    "session-a": { status: "open", worker_fp: "worker-a" },
    "session-a2": { status: "open", worker_fp: "worker-a" },
    "session-b": { status: "open", worker_fp: "worker-b" },
  };
  requests = [];
  mint = (request) => Promise.resolve(response(request.workerFp, request.sessionIds));
});
afterEach(() => grants._releaseTerminalGrantRenewalForTest());

describe("TerminalGrantOwner", () => {
  test("keeps grants and renewals scoped to their worker", async () => {
    const owner = new grants.TerminalGrantOwner();
    owner.setDemand("worker-a", "session-a", true);
    owner.setDemand("worker-b", "session-b", true);
    await settle();

    expect(requests).toEqual([
      { workerFp: "worker-a", sessionIds: ["session-a"], tabId: "tab-test" },
      { workerFp: "worker-b", sessionIds: ["session-b"], tabId: "tab-test" },
    ]);
    expect(owner.current("worker-a")?.workerEpoch).toBe("epoch-worker-a");
    expect(owner.current("worker-b")?.workerEpoch).toBe("epoch-worker-b");

    await owner.refresh("worker-a", "renewal");
    expect(requests.at(-1)).toEqual({ workerFp: "worker-a", sessionIds: ["session-a"], tabId: "tab-test" });
    expect(owner.current("worker-b")?.grantId).toBe("grant-worker-b-session-b");
    owner.reset();
  });

  test("coalesces added demand into a follow-up mint", async () => {
    const owner = new grants.TerminalGrantOwner();
    const first = Promise.withResolvers<GrantResponse>();
    let calls = 0;
    mint = (request) => ++calls === 1 ? first.promise : Promise.resolve(response(request.workerFp, request.sessionIds));

    owner.setDemand("worker-a", "session-a", true);
    await settle();
    owner.setDemand("worker-a", "session-a2", true);
    first.resolve(response("worker-a", ["session-a"]));
    await settle();

    expect(requests.map((request) => request.sessionIds)).toEqual([
      ["session-a"],
      ["session-a", "session-a2"],
    ]);
    expect(owner.current("worker-a")?.sessionIds).toEqual(["session-a", "session-a2"]);
    owner.reset();
  });

  test("retains an open authorized session when demand moves within one worker", async () => {
    const owner = new grants.TerminalGrantOwner();
    owner.setDemand("worker-a", "session-a", true);
    await settle();
    owner.setDemand("worker-a", "session-a", false);
    owner.setDemand("worker-a", "session-a2", true);
    await settle();

    expect(requests.map((request) => request.sessionIds)).toEqual([
      ["session-a"],
      ["session-a", "session-a2"],
    ]);
    owner.reset();
  });

  test("does not install a grant minted before an auth boundary", async () => {
    const owner = new grants.TerminalGrantOwner();
    const delayed = Promise.withResolvers<GrantResponse>();
    mint = () => delayed.promise;

    owner.setDemand("worker-a", "session-a", true);
    await settle();
    authGeneration += 1;
    delayed.resolve(response("worker-a", ["session-a"]));
    await settle();

    expect(owner.current("worker-a")).toBeNull();
    owner.reset();
  });

  test("removal clears a worker grant and fences its in-flight mint", async () => {
    const owner = new grants.TerminalGrantOwner();
    const delayed = Promise.withResolvers<GrantResponse>();
    const published: Array<string | null> = [];
    mint = () => delayed.promise;
    owner.subscribe("worker-a", (grant) => published.push(grant?.grantId ?? null));
    owner.setDemand("worker-a", "session-a", true);
    await settle();

    owner.retireWorker("worker-a");
    delayed.resolve(response("worker-a", ["session-a"]));
    await settle();

    expect(owner.current("worker-a")).toBeNull();
    expect(published).toEqual([null]);
    owner.reset();
  });
});
