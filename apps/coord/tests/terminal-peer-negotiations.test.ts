// Focused coordinator terminal-peer signaling coverage. These tests fence
// admission, exact worker identity, cancellation, and typed answer handling
// without retaining SDP in diagnostic state or exercising generic RPC JSON.
// Lease and worker handles are disposable in-memory fixtures.

import { afterEach, describe, expect, test } from "bun:test";
import { Code } from "@connectrpc/connect";
import type { TerminalPeerNegotiations } from "../src/connect/terminal-peer-negotiations.ts";
import type {
  TerminalPeerNegotiationClock,
  TerminalPeerNegotiationTimer,
} from "../src/connect/terminal-peer-negotiation-state.ts";
import type { WorkerHandle } from "../src/connect/worker-registry.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import {
  TEST_CALLER,
  TEST_TAB_ID,
  TestTerminalGrants,
  VALID_TERMINAL_PEER_SDP,
  beginPeerNegotiation,
  createPeerNegotiations,
  installLease,
  installTestWorker,
  peerAnswer,
  peerRequest,
} from "./terminal-peer-negotiation-test-fixture.ts";

const openedOwners: TerminalPeerNegotiations[] = [];
const workerFingerprints = new Set<string>();

interface PeerOwnerOptions {
  readonly clock?: TerminalPeerNegotiationClock;
  readonly answerTimeoutMs?: number;
  readonly authorizeSessions?: () => Promise<void>;
  readonly peerEnabled?: boolean;
}

function openOwner(grants: TestTerminalGrants, options: PeerOwnerOptions = {}) {
  const owner = createPeerNegotiations(grants, options);
  openedOwners.push(owner);
  return owner;
}

function openWorker(fingerprint = "a".repeat(64), epoch = "worker-epoch") {
  workerFingerprints.add(fingerprint);
  return installTestWorker(fingerprint, epoch);
}

afterEach(() => {
  for (const owner of openedOwners.splice(0)) owner.dispose();
  for (const fingerprint of workerFingerprints) __setConnectWorkerForTest(fingerprint, null);
  workerFingerprints.clear();
});

describe("TerminalPeerNegotiations admission", () => {
  test("rejects tab, grant, SDP, epoch, and capability failures before an offer", async () => {
    const grants = new TestTerminalGrants();
    const available = openWorker();
    installLease(grants, available.worker);
    const owner = openOwner(grants);

    await expect(owner.negotiate(
      TEST_CALLER,
      TEST_TAB_ID,
      peerRequest(available.worker, { tabId: "different-tab" }),
      new AbortController().signal,
    )).rejects.toMatchObject({ code: Code.PermissionDenied });
    await expect(owner.negotiate(
      TEST_CALLER,
      TEST_TAB_ID,
      peerRequest(available.worker, { grantId: "unknown-grant" }),
      new AbortController().signal,
    )).rejects.toMatchObject({ code: Code.PermissionDenied });
    await expect(owner.negotiate(
      TEST_CALLER,
      TEST_TAB_ID,
      peerRequest(available.worker, { offerSdp: "not-sdp" }),
      new AbortController().signal,
    )).rejects.toMatchObject({ code: Code.InvalidArgument });
    await expect(owner.negotiate(
      TEST_CALLER,
      TEST_TAB_ID,
      peerRequest(available.worker, { workerEpoch: "different-epoch" }),
      new AbortController().signal,
    )).rejects.toMatchObject({ code: Code.Unavailable });

    const unsupported = openWorker("b".repeat(64), "unsupported-epoch");
    unsupported.worker.capabilities = new Set();
    installLease(grants, unsupported.worker);
    await expect(owner.negotiate(
      TEST_CALLER,
      TEST_TAB_ID,
      peerRequest(unsupported.worker),
      new AbortController().signal,
    )).rejects.toMatchObject({ code: Code.Unavailable });
    expect(available.sent).toEqual([]);
    expect(unsupported.sent).toEqual([]);
  });

  test("accepts only an answer from the exact current worker handle", async () => {
    const grants = new TestTerminalGrants();
    const testWorker = openWorker();
    installLease(grants, testWorker.worker);
    const owner = openOwner(grants);
    const started = await beginPeerNegotiation(owner, testWorker);
    const answer = peerAnswer(started.offer, testWorker.worker);
    const rogue: WorkerHandle = {
      ...testWorker.worker,
      send: () => 1,
    };

    expect(owner.acceptAnswer(rogue, answer)).toBe(false);
    expect(owner.acceptAnswer(testWorker.worker, answer)).toBe(true);
    await expect(started.operation).resolves.toMatchObject({
      peerId: started.offer.peerId,
      answerSdp: VALID_TERMINAL_PEER_SDP,
    });
  });

  test("ignores a late old-handle answer after replacement and cancels its pending offer", async () => {
    const grants = new TestTerminalGrants();
    const first = openWorker();
    installLease(grants, first.worker);
    const owner = openOwner(grants);
    const started = await beginPeerNegotiation(owner, first);
    const replacement = openWorker(first.worker.workerFp, first.worker.processEpoch ?? "worker-epoch");

    expect(owner.acceptAnswer(first.worker, peerAnswer(started.offer, first.worker))).toBe(false);
    owner.cancelForWorkerHandle(first.worker, "connection_superseded");
    await expect(started.operation).rejects.toMatchObject({ code: Code.Unavailable });
    expect(replacement.sent).toEqual([]);
  });
});

describe("TerminalPeerNegotiations lifecycle", () => {
  test("sends cancellation on browser abort and grant revocation", async () => {
    const grants = new TestTerminalGrants();
    const testWorker = openWorker();
    const lease = installLease(grants, testWorker.worker);
    const owner = openOwner(grants);
    const abort = new AbortController();
    const aborted = await beginPeerNegotiation(owner, testWorker, abort.signal);
    abort.abort();
    await expect(aborted.operation).rejects.toMatchObject({ code: Code.Canceled });
    expect(testWorker.sent.at(-1)?.frame.case).toBe("localTerminalPeerCancel");

    const revoked = await beginPeerNegotiation(owner, testWorker, new AbortController().signal, peerRequest(
      testWorker.worker,
      { peerId: "00000000-0000-4000-8000-000000000011" },
    ));
    grants.invalidate({
      kind: "device_revoked",
      lease,
      workerFp: lease.workerFp,
      workerEpoch: lease.workerEpoch,
      deviceFingerprint: lease.deviceFingerprint,
      removedSessionIds: lease.sessionIds,
      reason: null,
    });
    await expect(revoked.operation).rejects.toMatchObject({ code: Code.PermissionDenied });
    expect(testWorker.sent.at(-1)?.frame.case).toBe("localTerminalPeerCancel");
  });

  test("rejects a duplicate in-flight flood before extra authorization or offers", async () => {
    let authorizationCalls = 0;
    const grants = new TestTerminalGrants();
    const testWorker = openWorker();
    installLease(grants, testWorker.worker);
    const owner = openOwner(grants, {
      authorizeSessions: async () => {
        authorizationCalls += 1;
      },
    });
    const first = owner.negotiate(
      TEST_CALLER,
      TEST_TAB_ID,
      peerRequest(testWorker.worker),
      new AbortController().signal,
    );
    await expect(owner.negotiate(
      TEST_CALLER,
      TEST_TAB_ID,
      peerRequest(testWorker.worker, {
        offerSdp: VALID_TERMINAL_PEER_SDP.replace("peer-offer", "different-offer"),
      }),
      new AbortController().signal,
    )).rejects.toMatchObject({ code: Code.InvalidArgument });
    first.catch(() => {});
    const duplicates = Array.from({ length: 96 }, () => owner.negotiate(
      TEST_CALLER,
      TEST_TAB_ID,
      peerRequest(testWorker.worker),
      new AbortController().signal,
    ));
    await Promise.all(duplicates.map(async (operation) => {
      await expect(operation).rejects.toMatchObject({ code: Code.AlreadyExists });
    }));
    for (let attempt = 0; attempt < 4; attempt += 1) await Promise.resolve();
    expect(authorizationCalls).toBe(1);
    expect(testWorker.sent).toHaveLength(1);
    owner.cancelForWorkerHandle(testWorker.worker, "test_cleanup");
    await expect(first).rejects.toMatchObject({ code: Code.Unavailable });
  });

  test("times out and disposes pending offers without retaining them", async () => {
    const timers: Array<{ callback: () => void }> = [];
    const clock: TerminalPeerNegotiationClock = {
      now: () => 0,
      setTimeout(callback) {
        const timer = { callback };
        timers.push(timer);
        return timer as unknown as TerminalPeerNegotiationTimer;
      },
      clearTimeout() {},
    };
    const grants = new TestTerminalGrants();
    const testWorker = openWorker();
    installLease(grants, testWorker.worker);
    const owner = openOwner(grants, { clock, answerTimeoutMs: 8_000 });
    const timedOut = await beginPeerNegotiation(owner, testWorker);
    const [timeout] = timers;
    if (!timeout) throw new Error("expected peer answer timeout");
    timeout.callback();
    await expect(timedOut.operation).rejects.toMatchObject({ code: Code.DeadlineExceeded });

    const disposed = await beginPeerNegotiation(owner, testWorker, new AbortController().signal, peerRequest(
      testWorker.worker,
      { peerId: "00000000-0000-4000-8000-000000000012" },
    ));
    owner.dispose();
    await expect(disposed.operation).rejects.toMatchObject({ code: Code.Unavailable });
  });
});
