// Focused coordinator attachment-peer coverage. These tests pin separate grant
// ownership, exact worker generation correlation, fixed worker failures, and
// cancellation without exercising direct byte transport or terminal signaling.
// Fake leases are disposable and contain only immutable descriptor metadata.

import { afterEach, describe, expect, test } from "bun:test";
import { Code } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { WLocalAttachmentPeerErrorSchema } from "@roost/shared/proto/worker_transport_pb";
import type { AttachmentPeerNegotiations } from "../src/connect/attachment-peer-negotiations.ts";
import type {
  AttachmentPeerNegotiationClock,
  AttachmentPeerNegotiationTimer,
} from "../src/connect/attachment-peer-negotiation-state.ts";
import type { WorkerHandle } from "../src/connect/worker-registry.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import {
  TEST_ATTACHMENT_CALLER,
  TEST_ATTACHMENT_TAB_ID,
  TestAttachmentGrants,
  VALID_ATTACHMENT_PEER_SDP,
  attachmentPeerAnswer,
  attachmentPeerRequest,
  beginAttachmentPeerNegotiation,
  createAttachmentPeerNegotiations,
  installAttachmentLease,
  installAttachmentTestWorker,
} from "./attachment-peer-negotiation-test-fixture.ts";

const openedOwners: AttachmentPeerNegotiations[] = [];
const workerFingerprints = new Set<string>();

function openOwner(
  grants: TestAttachmentGrants,
  options: {
    readonly clock?: AttachmentPeerNegotiationClock;
    readonly answerTimeoutMs?: number;
    readonly peerEnabled?: boolean;
  } = {},
): AttachmentPeerNegotiations {
  const owner = createAttachmentPeerNegotiations(grants, options);
  openedOwners.push(owner);
  return owner;
}

function openWorker(fingerprint = "a".repeat(64), epoch = "attachment-worker-epoch") {
  workerFingerprints.add(fingerprint);
  return installAttachmentTestWorker(fingerprint, epoch);
}

afterEach(() => {
  for (const owner of openedOwners.splice(0)) owner.dispose();
  for (const fingerprint of workerFingerprints) __setConnectWorkerForTest(fingerprint, null);
  workerFingerprints.clear();
});

describe("AttachmentPeerNegotiations admission", () => {
  test("requires the exact separate grant, device, tab, worker epoch, and handle", async () => {
    const grants = new TestAttachmentGrants();
    const available = openWorker();
    installAttachmentLease(grants, available.worker);
    const owner = openOwner(grants);

    await expect(owner.negotiate(
      TEST_ATTACHMENT_CALLER,
      TEST_ATTACHMENT_TAB_ID,
      attachmentPeerRequest(available.worker, { grantId: "unknown" }),
      new AbortController().signal,
    )).rejects.toMatchObject({ code: Code.PermissionDenied });
    await expect(owner.negotiate(
      TEST_ATTACHMENT_CALLER,
      TEST_ATTACHMENT_TAB_ID,
      attachmentPeerRequest(available.worker, { tabId: "other-tab" }),
      new AbortController().signal,
    )).rejects.toMatchObject({ code: Code.PermissionDenied });
    await expect(owner.negotiate(
      { ...TEST_ATTACHMENT_CALLER, fingerprint: "e".repeat(64) },
      TEST_ATTACHMENT_TAB_ID,
      attachmentPeerRequest(available.worker),
      new AbortController().signal,
    )).rejects.toMatchObject({ code: Code.PermissionDenied });
    await expect(owner.negotiate(
      TEST_ATTACHMENT_CALLER,
      TEST_ATTACHMENT_TAB_ID,
      attachmentPeerRequest(available.worker, { workerEpoch: "stale-epoch" }),
      new AbortController().signal,
    )).rejects.toMatchObject({ code: Code.Unavailable });

    expect(available.sent).toEqual([]);
  });

  test("sends and accepts only the exact current handle, epoch, peer, and request correlation", async () => {
    const grants = new TestAttachmentGrants();
    const testWorker = openWorker();
    installAttachmentLease(grants, testWorker.worker);
    const owner = openOwner(grants);
    const started = await beginAttachmentPeerNegotiation(owner, testWorker);
    const answer = attachmentPeerAnswer(started.offer, testWorker.worker);
    const rogue: WorkerHandle = { ...testWorker.worker, send: () => 1 };

    expect(started.offer).toMatchObject({
      grantId: "attachment-peer-test-grant",
      connectionGeneration: testWorker.worker.connectionGeneration,
      workerEpoch: testWorker.worker.processEpoch,
      deviceFingerprint: TEST_ATTACHMENT_CALLER.fingerprint,
      tabId: TEST_ATTACHMENT_TAB_ID,
    });
    expect(owner.acceptAnswer(rogue, answer)).toBe(false);
    expect(owner.acceptAnswer(testWorker.worker, { ...answer, workerEpoch: "wrong-epoch" })).toBe(false);
    expect(owner.acceptAnswer(testWorker.worker, { ...answer, peerId: "00000000-0000-4000-8000-000000000099" })).toBe(false);
    expect(owner.acceptAnswer(testWorker.worker, answer)).toBe(true);
    await expect(started.operation).resolves.toMatchObject({
      peerId: started.offer.peerId,
      answerSdp: VALID_ATTACHMENT_PEER_SDP,
      workerEpoch: testWorker.worker.processEpoch,
    });
  });
});

describe("AttachmentPeerNegotiations lifecycle", () => {
  test("does not let a stale replacement answer or cancellation settle another negotiation", async () => {
    const grants = new TestAttachmentGrants();
    const first = openWorker();
    installAttachmentLease(grants, first.worker);
    const owner = openOwner(grants);
    const original = await beginAttachmentPeerNegotiation(owner, first);
    const replacement = openWorker(first.worker.workerFp, first.worker.processEpoch ?? "attachment-worker-epoch");
    installAttachmentLease(grants, replacement.worker);

    expect(owner.acceptAnswer(first.worker, attachmentPeerAnswer(original.offer, first.worker))).toBe(false);
    owner.cancelForWorkerHandle(first.worker, "connection_superseded");
    await expect(original.operation).rejects.toMatchObject({ code: Code.Unavailable });

    const current = await beginAttachmentPeerNegotiation(
      owner,
      replacement,
      new AbortController().signal,
      attachmentPeerRequest(replacement.worker, { peerId: "00000000-0000-4000-8000-000000000011" }),
    );
    owner.cancelForWorkerHandle(first.worker, "late_cancel");
    expect(owner.acceptAnswer(first.worker, attachmentPeerAnswer(original.offer, first.worker))).toBe(false);
    expect(owner.acceptAnswer(replacement.worker, attachmentPeerAnswer(current.offer, replacement.worker))).toBe(true);
    await expect(current.operation).resolves.toMatchObject({ peerId: current.offer.peerId });
  });

  test("sends a cancel at timeout and maps only fixed worker errors", async () => {
    const timers: Array<{ callback: () => void; unref(): void }> = [];
    const clock: AttachmentPeerNegotiationClock = {
      now: () => 0,
      setTimeout(callback) {
        const timer = { callback, unref() {} };
        timers.push(timer);
        return timer as unknown as AttachmentPeerNegotiationTimer;
      },
      clearTimeout() {},
    };
    const grants = new TestAttachmentGrants();
    const testWorker = openWorker();
    installAttachmentLease(grants, testWorker.worker);
    const owner = openOwner(grants, { clock, answerTimeoutMs: 8_000 });
    const timedOut = await beginAttachmentPeerNegotiation(owner, testWorker);
    timers[0]!.callback();
    await expect(timedOut.operation).rejects.toMatchObject({ code: Code.DeadlineExceeded });
    expect(testWorker.sent.at(-1)?.frame.case).toBe("localAttachmentPeerCancel");

    for (const [reason, code, peerId] of [
      ["grant_unavailable", Code.PermissionDenied, "00000000-0000-4000-8000-000000000012"],
      ["capacity", Code.ResourceExhausted, "00000000-0000-4000-8000-000000000013"],
      ["ice_failed", Code.Unavailable, "00000000-0000-4000-8000-000000000014"],
    ] as const) {
      const started = await beginAttachmentPeerNegotiation(
        owner,
        testWorker,
        new AbortController().signal,
        attachmentPeerRequest(testWorker.worker, { peerId }),
      );
      const error = create(WLocalAttachmentPeerErrorSchema, {
        requestId: started.offer.requestId,
        connectionGeneration: testWorker.worker.connectionGeneration,
        workerEpoch: testWorker.worker.processEpoch ?? "",
        peerId: started.offer.peerId,
        reason,
      });
      expect(owner.acceptError(testWorker.worker, error)).toBe(true);
      await expect(started.operation).rejects.toMatchObject({ code });
    }
  });
});
