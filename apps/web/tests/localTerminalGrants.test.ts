// Worker-scoped direct grants must coalesce demand without letting a removed
// fingerprint mint credentials again. These tests drive the coordinator boundary
// directly so delayed credential responses remain fenced at auth and retirement.

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";

interface SessionRow { status: string; worker_fp: string; }
interface GrantRequest { sessionIds: string[]; workerFp: string; tabId: string; }
interface GrantResponse {
  grantId: string; secret: string; ttlMs: number; workerEpoch: string;
  peerSupported: boolean; stunUrls: string[]; inputRouteSupported: boolean;
}

let authGeneration = 0;
let sessions: Record<string, SessionRow> = {};
let workers: Record<string, object> = {};
let requests: GrantRequest[] = [];
let mint: (request: GrantRequest) => Promise<GrantResponse>;

mock.module("../src/store/root.ts", () => ({
  rootStore: {
    get auth_generation() { return authGeneration; },
    get sessions() { return sessions; },
    get workers() { return workers; },
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

// Module mocks must install before this owner creates its document singleton.
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
  workers = { "worker-a": {}, "worker-b": {} };
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
    expect(requests.map((request) => request.sessionIds)).toEqual([["session-a"], ["session-a", "session-a2"]]);
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
    expect(requests.map((request) => request.sessionIds)).toEqual([["session-a"], ["session-a", "session-a2"]]);
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

  test("removal fences an in-flight mint and a previously valid grant", async () => {
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
    expect(owner.isWorkerRetired("worker-a")).toBe(true);

    const valid = new grants.TerminalGrantOwner();
    valid.setDemand("worker-b", "session-b", true);
    await settle();
    expect(valid.current("worker-b")).not.toBeNull();
    valid.retireWorker("worker-b");
    valid.setDemand("worker-b", "session-b", true);
    await valid.refresh("worker-b", "renewal");
    expect(valid.current("worker-b")).toBeNull();
    valid.reset(); owner.reset();
  });

  test("retiring an absent worker blocks new demand until auth reset while another worker remains usable", async () => {
    const owner = new grants.TerminalGrantOwner();
    owner.retireWorker("worker-a");
    owner.setDemand("worker-a", "session-a", true);
    await owner.refresh("worker-a", "demand_added");
    await settle();
    expect(requests).toEqual([]);
    owner.setDemand("worker-b", "session-b", true);
    await settle();
    expect(requests).toEqual([{ workerFp: "worker-b", sessionIds: ["session-b"], tabId: "tab-test" }]);
    owner.reset();
    owner.setDemand("worker-a", "session-a", true);
    await settle();
    expect(requests.at(-1)).toEqual({ workerFp: "worker-a", sessionIds: ["session-a"], tabId: "tab-test" });
    owner.reset();
  });

  test("mints an initial grant without a workers projection", async () => {
    workers = {};
    const owner = new grants.TerminalGrantOwner();
    owner.setDemand("worker-a", "session-a", true);
    await settle();
    expect(requests).toEqual([{ workerFp: "worker-a", sessionIds: ["session-a"], tabId: "tab-test" }]);
    owner.reset();
  });
});
