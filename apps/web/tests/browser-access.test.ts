// Browser access-state transitions behind App.tsx's gate. Only the protected
// sessions snapshot may authorize, only a device-classified rejection may
// deny, and transient failures or a successful workers refresh never move the
// verdict. Drives refreshCoordAndWorkers against a fake coordinator client.

import { expect, mock, test } from "bun:test";

const storage = new Map<string, string>();
const memoryStorage = {
  get length() { return storage.size; },
  key: (index: number) => [...storage.keys()][index] ?? null,
  getItem: (key: string) => storage.get(key) ?? null,
  setItem: (key: string, value: string) => { storage.set(key, value); },
  removeItem: (key: string) => { storage.delete(key); },
};
Object.assign(globalThis, {
  localStorage: memoryStorage,
  sessionStorage: memoryStorage,
  location: {
    origin: "http://127.0.0.1:65000",
    protocol: "http:",
    host: "127.0.0.1:65000",
    hostname: "127.0.0.1",
    href: "http://127.0.0.1:65000/",
    pathname: "/",
    search: "",
    hash: "",
  },
});

class DeviceRejection extends Error {}

type WorkersOutcome = "listed" | "offline" | "device-rejected";
let workersOutcome: WorkersOutcome = "listed";
let agentConfigLoads = 0;

const coordClient = {
  authCoordIdentity: async () => ({ gitSha: "test", publicUrl: "http://127.0.0.1:65000" }),
  workersList: async () => {
    if (workersOutcome === "offline") throw new TypeError("Failed to fetch");
    if (workersOutcome === "device-rejected") throw new DeviceRejection("unknown device");
    return {
      routableFps: ["worker-a"],
      workers: [{
        fp: "worker-a",
        label: "Worker A",
        os: "linux",
        registeredAtMs: 1n,
        lastSeenMs: 2n,
      }],
    };
  },
  agentConfigGet: async () => {
    agentConfigLoads += 1;
    return { selected: "omp", customCommand: "", autoLaunch: false };
  },
};

mock.module("../src/connect.ts", () => ({
  coordClient,
  publicCoordClient: coordClient,
  makeCoordinatorClientForSigner: () => coordClient,
  coordBase: () => "http://127.0.0.1:65000",
  coordinatorBaseUrl: () => "http://127.0.0.1:65000",
  coordinatorRpcUrl: (path: string) => `http://127.0.0.1:65000${path}`,
  reconcileCoordinatorOverrideAfterDiscovery: () => false,
  classifyAuthFailure: (error: unknown) => (error instanceof DeviceRejection ? "device" : "retryable"),
}));

// Fakes above must bind before the store graph evaluates (module-loading
// boundary — ts-no-dynamic-import exception).
const root = await import("../src/store/root.ts");
const access = await import("../src/store/browser-access.ts");
const { refreshCoordAndWorkers } = await import("../src/store/sync-bootstrap.ts");

async function refreshWorkers(outcome: WorkersOutcome): Promise<void> {
  workersOutcome = outcome;
  await refreshCoordAndWorkers();
}

test("only the protected snapshot releases checking; device rejection denies; recovery re-authorizes", async () => {
  expect(root.rootStore.browser_access_state).toBe("checking");
  root.setRootStore("coord_identity", { git_sha: "test", public_url: "http://127.0.0.1:65000" });

  // Workers answering is not authority: the gate keeps showing "checking".
  await refreshWorkers("listed");
  expect(Object.keys(root.rootStore.workers)).toEqual(["worker-a"]);
  expect(root.rootStore.browser_access_state).toBe("checking");

  await refreshWorkers("offline");
  expect(root.rootStore.browser_access_state).toBe("checking");

  access.markProtectedSnapshotPublished();
  expect(root.rootStore.browser_access_state).toBe("authorized");

  await refreshWorkers("listed");
  await refreshWorkers("offline");
  expect(root.rootStore.browser_access_state).toBe("authorized");

  const generationBeforeRejection = root.rootStore.auth_generation;
  await refreshWorkers("device-rejected");
  expect(root.rootStore.browser_access_state).toBe("unauthorized");
  // The loss edge tears down every authenticated replica and token.
  expect(root.rootStore.workers).toEqual({});
  expect(root.rootStore.auth_generation).toBeGreaterThan(generationBeforeRejection);

  // A persistently rejected browser is not torn down again on every refresh,
  // and neither a listed nor an offline refresh re-authorizes it.
  const generationAfterRejection = root.rootStore.auth_generation;
  await refreshWorkers("device-rejected");
  await refreshWorkers("offline");
  await refreshWorkers("listed");
  expect(root.rootStore.auth_generation).toBe(generationAfterRejection);
  expect(root.rootStore.browser_access_state).toBe("unauthorized");

  // Re-authorization restores the runtimes the teardown dropped.
  const loadsBeforeRecovery = agentConfigLoads;
  access.markProtectedSnapshotPublished();
  expect(root.rootStore.browser_access_state).toBe("authorized");
  expect(agentConfigLoads).toBe(loadsBeforeRecovery + 1);
});

test("a credential boundary returns the gate to checking", () => {
  access.markProtectedSnapshotPublished();
  expect(root.rootStore.browser_access_state).toBe("authorized");
  root.clearAccountRootStateForLogout();
  expect(root.rootStore.browser_access_state).toBe("checking");
});
