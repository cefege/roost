// Focused capacity coverage for coordinator terminal-peer signaling. Every
// request reaches the real pending-owner reservation after a valid grant and
// exact worker handle are installed; no worker answer is needed for admission.
// Pending operations are disposed at the end of each assertion.

import { expect, test } from "bun:test";
import { Code } from "@connectrpc/connect";
import type { AccountDeviceCaller } from "../../../src/auth/auth-interceptor.ts";
import type { TerminalPeerNegotiations } from "../../../src/terminal/direct/terminal-peer-negotiations.ts";
import { captureOwnerKey } from "../../../src/terminal/capture/terminal-capture-lease.ts";
import { __setConnectWorkerForTest, type WorkerHandle } from "../../../src/workers/worker-registry.ts";
import {
  TEST_CALLER,
  TestTerminalGrants,
  createPeerNegotiations,
  installLease,
  installTestWorker,
  peerRequest,
} from "./terminal-peer-negotiation-test-fixture.ts";

function syntheticFingerprint(index: number, fill: string): string {
  return `${index.toString(16).padStart(2, "0")}${fill.repeat(62)}`;
}

function syntheticPeerId(index: number): string {
  return `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`;
}

function deviceCaller(index: number): AccountDeviceCaller {
  return { ...TEST_CALLER, fingerprint: syntheticFingerprint(index, "d") };
}

function reserve(
  owner: TerminalPeerNegotiations,
  grants: TestTerminalGrants,
  caller: AccountDeviceCaller,
  worker: WorkerHandle,
  tabId: string,
  peerId: string,
) {
  installLease(grants, worker, {
    ownerKey: captureOwnerKey(caller),
    deviceFingerprint: caller.fingerprint,
    tabId,
  });
  const operation = owner.negotiate(caller, tabId, peerRequest(worker, { tabId, peerId }), new AbortController().signal);
  operation.catch(() => {});
  return operation;
}

async function allowReservation(): Promise<void> {
  for (let attempt = 0; attempt < 4; attempt += 1) await Promise.resolve();
}

test("bounds four worker, eight device, and sixty-four global peer negotiations", async () => {
  const workerFingerprints = new Set<string>();
  const owners: TerminalPeerNegotiations[] = [];
  try {
    const workerGrants = new TestTerminalGrants();
    const workerOwner = createPeerNegotiations(workerGrants);
    owners.push(workerOwner);
    const worker = installTestWorker(syntheticFingerprint(1, "a"));
    workerFingerprints.add(worker.worker.workerFp);
    for (let index = 0; index < 4; index += 1) {
      reserve(workerOwner, workerGrants, TEST_CALLER, worker.worker, `worker-tab-${index}`, syntheticPeerId(index));
      await allowReservation();
    }
    const workerOverflow = reserve(
      workerOwner,
      workerGrants,
      TEST_CALLER,
      worker.worker,
      "worker-tab-overflow",
      syntheticPeerId(4),
    );
    await allowReservation();
    await expect(workerOverflow).rejects.toMatchObject({ code: Code.ResourceExhausted });

    const deviceGrants = new TestTerminalGrants();
    const deviceOwner = createPeerNegotiations(deviceGrants);
    owners.push(deviceOwner);
    for (let index = 0; index < 8; index += 1) {
      const deviceWorker = installTestWorker(syntheticFingerprint(10 + index, "b"));
      workerFingerprints.add(deviceWorker.worker.workerFp);
      reserve(
        deviceOwner,
        deviceGrants,
        TEST_CALLER,
        deviceWorker.worker,
        `device-tab-${index}`,
        syntheticPeerId(10 + index),
      );
      await allowReservation();
    }
    const ninthWorker = installTestWorker(syntheticFingerprint(19, "b"));
    workerFingerprints.add(ninthWorker.worker.workerFp);
    const deviceOverflow = reserve(
      deviceOwner,
      deviceGrants,
      TEST_CALLER,
      ninthWorker.worker,
      "device-tab-overflow",
      syntheticPeerId(19),
    );
    await allowReservation();
    await expect(deviceOverflow).rejects.toMatchObject({ code: Code.ResourceExhausted });

    const globalGrants = new TestTerminalGrants();
    const globalOwner = createPeerNegotiations(globalGrants);
    owners.push(globalOwner);
    const workers = Array.from({ length: 16 }, (_, index) => {
      const testWorker = installTestWorker(syntheticFingerprint(30 + index, "c"));
      workerFingerprints.add(testWorker.worker.workerFp);
      return testWorker.worker;
    });
    for (let index = 0; index < 64; index += 1) {
      const caller = deviceCaller(30 + Math.floor(index / 8));
      reserve(
        globalOwner,
        globalGrants,
        caller,
        workers[index % workers.length]!,
        `global-tab-${index}`,
        syntheticPeerId(30 + index),
      );
      await allowReservation();
    }
    const globalCaller = deviceCaller(39);
    const globalOverflow = reserve(
      globalOwner,
      globalGrants,
      globalCaller,
      workers[0]!,
      "global-tab-overflow",
      syntheticPeerId(95),
    );
    await allowReservation();
    await expect(globalOverflow).rejects.toMatchObject({ code: Code.ResourceExhausted });
  } finally {
    for (const owner of owners) owner.dispose();
    for (const fingerprint of workerFingerprints) __setConnectWorkerForTest(fingerprint, null);
  }
});

test("counts multi-tab worker admissions before authorization resolves", async () => {
  const workerFingerprints = new Set<string>();
  const grants = new TestTerminalGrants();
  const authorizationGate = Promise.withResolvers<void>();
  let authorizationCalls = 0;
  const owner = createPeerNegotiations(grants, {
    authorizeSessions: () => {
      authorizationCalls += 1;
      return authorizationGate.promise;
    },
  });
  const workers = Array.from({ length: 16 }, (_, index) => {
    const testWorker = installTestWorker(syntheticFingerprint(130 + index, "e"));
    workerFingerprints.add(testWorker.worker.workerFp);
    return testWorker.worker;
  });
  const operations: Promise<unknown>[] = [];
  try {
    for (let index = 0; index < 64; index += 1) {
      const caller = deviceCaller(50 + Math.floor(index / 8));
      operations.push(reserve(
        owner,
        grants,
        caller,
        workers[index % workers.length]!,
        `admission-tab-${index}`,
        syntheticPeerId(130 + index),
      ));
    }
    expect(authorizationCalls).toBe(64);
    const overflow = reserve(
      owner,
      grants,
      deviceCaller(59),
      workers[0]!,
      "admission-tab-overflow",
      syntheticPeerId(194),
    );
    await expect(overflow).rejects.toMatchObject({ code: Code.ResourceExhausted });
    expect(authorizationCalls).toBe(64);
  } finally {
    owner.dispose();
    authorizationGate.resolve();
    await Promise.all(operations.map((operation) => operation.then(
      () => undefined,
      () => undefined,
    )));
    for (const fingerprint of workerFingerprints) __setConnectWorkerForTest(fingerprint, null);
  }
});

test("keeps aborted preauthorization work charged until its authorizer exits", async () => {
  const workerFingerprints = new Set<string>();
  const grants = new TestTerminalGrants();
  const authorizationGate = Promise.withResolvers<void>();
  let authorizationCalls = 0;
  const owner = createPeerNegotiations(grants, {
    authorizeSessions: () => {
      authorizationCalls += 1;
      return authorizationGate.promise;
    },
  });
  const testWorker = installTestWorker(syntheticFingerprint(200, "f"));
  workerFingerprints.add(testWorker.worker.workerFp);
  const controllers = Array.from({ length: 4 }, () => new AbortController());
  const operations: Promise<unknown>[] = [];
  try {
    for (let index = 0; index < 4; index += 1) {
      const tabId = `aborted-admission-tab-${index}`;
      installLease(grants, testWorker.worker, { tabId });
      const operation = owner.negotiate(
        TEST_CALLER,
        tabId,
        peerRequest(testWorker.worker, { tabId, peerId: syntheticPeerId(200 + index) }),
        controllers[index]!.signal,
      );
      operation.catch(() => {});
      operations.push(operation);
    }
    expect(authorizationCalls).toBe(4);
    controllers[0]!.abort();
    const overflowTabId = "aborted-admission-overflow";
    installLease(grants, testWorker.worker, { tabId: overflowTabId });
    const overflow = owner.negotiate(
      TEST_CALLER,
      overflowTabId,
      peerRequest(testWorker.worker, { tabId: overflowTabId, peerId: syntheticPeerId(204) }),
      new AbortController().signal,
    );
    await expect(overflow).rejects.toMatchObject({ code: Code.ResourceExhausted });
    expect(authorizationCalls).toBe(4);
  } finally {
    owner.dispose();
    authorizationGate.resolve();
    await Promise.all(operations.map((operation) => operation.then(
      () => undefined,
      () => undefined,
    )));
    for (const fingerprint of workerFingerprints) __setConnectWorkerForTest(fingerprint, null);
  }
});
