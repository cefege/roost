// Focused lifecycle coverage for the separate attachment direct-grant registry.
// Fake workers exercise digest-only installation and exact handle fencing through
// the production pending-RPC acknowledgement path. They exercise attachment
// grant authority only, without any direct attachment byte transport.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { Code, ConnectError } from "@connectrpc/connect";
import type {
  CoordWorkerDown,
  DLocalAttachmentGrant,
} from "@roost/protocol/proto/worker_transport_pb";
import {
  AttachmentGrantOwner,
  type AttachmentGrantRequest,
  type AttachmentGrantResult,
} from "../../src/attachments/attachment-grant-owner.ts";
import {
  __setConnectWorkerForTest,
  connectWorkers,
  fenceWorkerCredential,
  type WorkerHandle,
} from "../../src/workers/worker-registry.ts";
import {
  rejectPendingRpcsForWorker,
  resolvePendingRpc,
} from "../../src/router/pending-rpcs.ts";

const WORKER_FP = "a".repeat(64);
const OTHER_WORKER_FP = "b".repeat(64);
const DEVICE_FP = "c".repeat(64);
const OWNER_KEY = `account-device:test-account:${DEVICE_FP}`;
const TAB_ID = "attachment-tab";
const SESSION_ID = "00000000-0000-4000-8000-000000000001";
const UPLOAD_ID = "00000000-0000-4000-8000-000000000002";

interface CapturedGrant {
  readonly workerFp: string;
  readonly frame: DLocalAttachmentGrant;
}

interface StartedGrant {
  readonly result: Promise<AttachmentGrantResult>;
  readonly captured: CapturedGrant;
}

let owner: AttachmentGrantOwner;
let grantFrames: CapturedGrant[];
let revokeFrames: Array<{ workerFp: string; deviceFingerprint: string }>;
let announceGrant: ((captured: CapturedGrant) => void) | null;
let workerConnectionSequence = 0;
function stringifyTestValue(value: unknown): string {
  return JSON.stringify(value, (_key, item) =>
    typeof item === "bigint" ? item.toString() : item
  ) ?? "";
}


function installWorker(workerFp: string, workerEpoch: string): WorkerHandle {
  __setConnectWorkerForTest(workerFp, {
    workerFp,
    processEpoch: workerEpoch,
    connectionGeneration: `${workerFp.slice(0, 4)}-${workerEpoch}-${workerConnectionSequence++}`,
    send(frame: CoordWorkerDown): number {
      if (frame.frame.case === "localAttachmentGrant") {
        const captured = { workerFp, frame: frame.frame.value };
        grantFrames.push(captured);
        announceGrant?.(captured);
        announceGrant = null;
      } else if (frame.frame.case === "localAttachmentGrantRevoke") {
        revokeFrames.push({ workerFp, deviceFingerprint: frame.frame.value.deviceFingerprint });
      }
      return 1;
    },
  });
  const worker = connectWorkers.get(workerFp);
  if (!worker) throw new Error("test worker was not installed");
  return worker;
}

function grantRequest(options: {
  readonly workerFp?: string;
  readonly uploadId?: string;
  readonly authorize?: AttachmentGrantRequest["authorize"];
} = {}): AttachmentGrantRequest {
  return {
    ownerKey: OWNER_KEY,
    deviceFingerprint: DEVICE_FP,
    tabId: TAB_ID,
    workerFp: options.workerFp ?? WORKER_FP,
    descriptor: {
      sessionId: SESSION_ID,
      uploadId: options.uploadId ?? UPLOAD_ID,
      filename: "diagram.png",
      shortPath: false,
      totalBytes: 1_024,
    },
    authorize: options.authorize ?? (async () => undefined),
  };
}

async function startGrant(options: Parameters<typeof grantRequest>[0] = {}): Promise<StartedGrant> {
  const { promise: announced, resolve } = Promise.withResolvers<CapturedGrant>();
  announceGrant = resolve;
  const result = owner.grant(grantRequest(options));
  result.catch(() => {});
  const captured = await Promise.race([
    announced,
    result.then(() => {
      throw new Error("attachment grant completed before worker ACK was observed");
    }),
  ]);
  return { result, captured };
}

async function grantWithAck(options: Parameters<typeof grantRequest>[0] = {}): Promise<AttachmentGrantResult> {
  const started = await startGrant(options);
  resolvePendingRpc(started.captured.frame.requestId, {}, started.captured.workerFp);
  return started.result;
}

async function connectErrorFrom(work: Promise<unknown>): Promise<ConnectError> {
  const error = await work.then(() => null, (thrown: unknown) => thrown);
  if (!(error instanceof ConnectError)) throw new Error(`expected ConnectError, got ${String(error)}`);
  return error;
}

beforeEach(() => {
  owner = new AttachmentGrantOwner();
  grantFrames = [];
  revokeFrames = [];
  announceGrant = null;
  workerConnectionSequence = 0;
  installWorker(WORKER_FP, "attachment-epoch");
  installWorker(OTHER_WORKER_FP, "other-epoch");
});

afterEach(() => {
  owner.dispose();
  __setConnectWorkerForTest(WORKER_FP, null);
  __setConnectWorkerForTest(OTHER_WORKER_FP, null);
  rejectPendingRpcsForWorker(WORKER_FP, "attachment grant test cleanup");
  rejectPendingRpcsForWorker(OTHER_WORKER_FP, "attachment grant test cleanup");
});

describe("AttachmentGrantOwner", () => {
  test("returns a secret only after digest-only worker ACK and keeps the descriptor immutable", async () => {
    const started = await startGrant();
    expect(started.captured.frame).toMatchObject({
      sessionId: SESSION_ID,
      uploadId: UPLOAD_ID,
      filename: "diagram.png",
      totalBytes: 1_024n,
      deviceFingerprint: DEVICE_FP,
      tabId: TAB_ID,
      workerEpoch: "attachment-epoch",
    });
    resolvePendingRpc(started.captured.frame.requestId, {}, started.captured.workerFp);
    const result = await started.result;

    expect(result.secret).toMatch(/^[0-9a-f]{64}$/);
    expect(started.captured.frame.secretSha256).not.toBe(result.secret);
    expect(stringifyTestValue(started.captured.frame)).not.toContain(result.secret);
    expect(owner.ownedGrant(OWNER_KEY, TAB_ID, WORKER_FP, result.lease.grantId)).toMatchObject({
      descriptor: {
        sessionId: SESSION_ID,
        uploadId: UPLOAD_ID,
        filename: "diagram.png",
        totalBytes: 1_024,
      },
    });
    expect(owner.ownedGrant(`${OWNER_KEY}:other`, TAB_ID, WORKER_FP, result.lease.grantId)).toBeNull();
    expect(owner.ownedGrant(OWNER_KEY, "other-tab", WORKER_FP, result.lease.grantId)).toBeNull();
  });

  test("rejects an ACK after handle replacement and never rebinds that attachment grant", async () => {
    const started = await startGrant();
    installWorker(WORKER_FP, "attachment-epoch-replaced");
    resolvePendingRpc(started.captured.frame.requestId, {}, started.captured.workerFp);
    const error = await connectErrorFrom(started.result);

    expect(error.code).toBe(Code.Unavailable);
    expect(owner.ownedGrant(OWNER_KEY, TAB_ID, WORKER_FP, started.captured.frame.grantId)).toBeNull();
  });

  test("expires, revokes, and retires only separate attachment grant leases", async () => {
    const first = await grantWithAck();
    const invalidations: string[] = [];
    owner.subscribeInvalidation((event) => invalidations.push(event.kind));
    owner.sweep(first.lease.expiresAtMs);
    expect(owner.ownedGrant(OWNER_KEY, TAB_ID, WORKER_FP, first.lease.grantId)).toBeNull();

    const second = await grantWithAck({ uploadId: "00000000-0000-4000-8000-000000000003" });
    owner.revokeDevice(DEVICE_FP);
    expect(owner.ownedGrant(OWNER_KEY, TAB_ID, WORKER_FP, second.lease.grantId)).toBeNull();
    expect(revokeFrames).toEqual([
      { workerFp: WORKER_FP, deviceFingerprint: DEVICE_FP },
      { workerFp: OTHER_WORKER_FP, deviceFingerprint: DEVICE_FP },
    ]);

    const third = await grantWithAck({ uploadId: "00000000-0000-4000-8000-000000000004" });
    owner.retireWorker(WORKER_FP, "worker_deleted");
    expect(owner.ownedGrant(OWNER_KEY, TAB_ID, WORKER_FP, third.lease.grantId)).toBeNull();
    expect(invalidations).toEqual(["grant_expired", "device_revoked", "worker_retired"]);
    fenceWorkerCredential(WORKER_FP);
    expect(connectWorkers.get(WORKER_FP)).toBeUndefined();
  });
});
