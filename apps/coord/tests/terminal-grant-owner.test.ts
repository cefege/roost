// Focused lifecycle coverage for the composition-owned direct terminal grant
// registry. Fake worker handles exercise exact-generation ACK fencing without a
// network listener, while pending RPC correlation remains the production path.
// Handler request validation and response fields stay in local-terminal-grant.test.ts.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { Code, ConnectError } from "@connectrpc/connect";
import type {
  CoordWorkerDown,
  DLocalTerminalGrant,
  DTerminalDirectRetire,
} from "@roost/shared/proto/worker_transport_pb";
import {
  TerminalGrantOwner,
  type TerminalGrantRequest,
  type TerminalGrantResult,
} from "../src/connect/terminal-grant-owner.ts";
import {
  __setConnectWorkerForTest,
  connectWorkers,
  fenceWorkerCredential,
  type WorkerHandle,
} from "../src/connect/worker-registry.ts";
import {
  rejectPendingRpc,
  rejectPendingRpcsForWorker,
  resolvePendingRpc,
} from "../src/router/pending-rpcs.ts";

const WORKER_A = "a".repeat(64);
const WORKER_B = "b".repeat(64);
const DEVICE_FP = "c".repeat(64);
const OWNER_KEY = "account-device:test-account:" + DEVICE_FP;
const TAB_ID = "terminal-tab";
const SESSION_A = "00000000-0000-4000-8000-000000000001";
const SESSION_B = "00000000-0000-4000-8000-000000000002";

interface CapturedGrant {
  readonly workerFp: string;
  readonly frame: DLocalTerminalGrant;
}

interface StartedGrant {
  readonly result: Promise<TerminalGrantResult>;
  readonly captured: CapturedGrant;
}

interface GrantOptions {
  readonly workerFp?: string;
  readonly sessionIds?: readonly string[];
  readonly authorize?: TerminalGrantRequest["authorize"];
}

let owner: TerminalGrantOwner;
let grantFrames: CapturedGrant[];
let revokeFrames: Array<{ workerFp: string; deviceFingerprint: string }>;
let retireFrames: Array<{ workerFp: string; frame: DTerminalDirectRetire }>;
let sendOrder: string[];
let announceGrant: ((captured: CapturedGrant) => void) | null;
let workerConnectionGeneration = 0;

function installWorker(workerFp: string, processEpoch: string | null): WorkerHandle {
  __setConnectWorkerForTest(workerFp, {
    workerFp,
    processEpoch,
    connectionGeneration: `${workerFp.slice(0, 4)}-${processEpoch ?? "legacy"}-${workerConnectionGeneration++}`,
    send(frame: CoordWorkerDown): number {
      sendOrder.push(frame.frame.case ?? "unknown");
      if (frame.frame.case === "localTerminalGrant") {
        const captured = { workerFp, frame: frame.frame.value };
        grantFrames.push(captured);
        announceGrant?.(captured);
        announceGrant = null;
      } else if (frame.frame.case === "localTerminalGrantRevoke") {
        revokeFrames.push({ workerFp, deviceFingerprint: frame.frame.value.deviceFingerprint });
      } else if (frame.frame.case === "terminalDirectRetire") {
        retireFrames.push({ workerFp, frame: frame.frame.value });
      }
      return 1;
    },
  });
  const handle = connectWorkers.get(workerFp);
  if (!handle) throw new Error("test worker was not installed");
  return handle;
}

function grantRequest(options: GrantOptions = {}): TerminalGrantRequest {
  return {
    ownerKey: OWNER_KEY,
    deviceFingerprint: DEVICE_FP,
    tabId: TAB_ID,
    workerFp: options.workerFp ?? WORKER_A,
    sessionIds: options.sessionIds ?? [SESSION_A],
    authorize: options.authorize ?? (async () => undefined),
  };
}

async function startGrant(options: GrantOptions = {}): Promise<StartedGrant> {
  const { promise: announced, resolve } = Promise.withResolvers<CapturedGrant>();
  announceGrant = resolve;
  const result = owner.grant(grantRequest(options));
  result.catch(() => {});
  const captured = await Promise.race([
    announced,
    result.then(() => {
      throw new Error("grant completed before worker install was observed");
    }),
  ]);
  return { result, captured };
}

async function grantWithAck(options: GrantOptions = {}): Promise<TerminalGrantResult> {
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
  owner = new TerminalGrantOwner();
  grantFrames = [];
  revokeFrames = [];
  retireFrames = [];
  sendOrder = [];
  announceGrant = null;
  workerConnectionGeneration = 0;
  installWorker(WORKER_A, "epoch-a");
  installWorker(WORKER_B, "epoch-b");
});

afterEach(() => {
  owner.dispose();
  __setConnectWorkerForTest(WORKER_A, null);
  __setConnectWorkerForTest(WORKER_B, null);
  rejectPendingRpcsForWorker(WORKER_A, "grant owner test cleanup");
  rejectPendingRpcsForWorker(WORKER_B, "grant owner test cleanup");
});

describe("TerminalGrantOwner", () => {
  test("keeps simultaneous same-tab leases separate by worker", async () => {
    const first = await grantWithAck({ workerFp: WORKER_A, sessionIds: [SESSION_A] });
    const second = await grantWithAck({ workerFp: WORKER_B, sessionIds: [SESSION_B] });

    expect(first.lease.grantId).not.toBe(second.lease.grantId);
    expect(owner.list()).toMatchObject([
      { workerFp: WORKER_A, tabId: TAB_ID, sessionIds: [SESSION_A] },
      { workerFp: WORKER_B, tabId: TAB_ID, sessionIds: [SESSION_B] },
    ]);
  });

  test("renews on one exact worker with a stable id and rotated browser secret", async () => {
    const initial = await grantWithAck({ sessionIds: [SESSION_A] });
    const renewal = await grantWithAck({ sessionIds: [SESSION_A, SESSION_B] });

    expect(renewal.lease.grantId).toBe(initial.lease.grantId);
    expect(renewal.secret).not.toBe(initial.secret);
    expect(grantFrames).toHaveLength(2);
    expect(grantFrames[1]!.frame.grantId).toBe(grantFrames[0]!.frame.grantId);
    expect(grantFrames[1]!.frame.secretSha256).not.toBe(grantFrames[0]!.frame.secretSha256);
    expect(owner.list()).toMatchObject([{ sessionIds: [SESSION_A, SESSION_B] }]);
  });

  test("coalesces a slow renewal flood into one pending install", async () => {
    let authorizationCalls = 0;
    const authorize = async (_sessionIds: readonly string[]): Promise<void> => {
      authorizationCalls += 1;
    };
    const started = await startGrant({ authorize });
    const joined = Array.from({ length: 512 }, () => owner.grant(grantRequest({ authorize })));

    expect(joined.every((promise) => promise === started.result)).toBe(true);
    expect(grantFrames).toHaveLength(1);
    resolvePendingRpc(started.captured.frame.requestId, {}, started.captured.workerFp);
    const result = await started.result;
    expect(result.lease.sessionIds).toEqual([SESSION_A]);
    expect(authorizationCalls).toBe(2);
    expect(owner.list()).toHaveLength(1);
  });

  test("sends one union follow-up when demand grows during an install", async () => {
    const authorizedSessionSets: string[][] = [];
    const authorize = async (sessionIds: readonly string[]): Promise<void> => {
      authorizedSessionSets.push([...sessionIds]);
    };
    const started = await startGrant({ sessionIds: [SESSION_A], authorize });
    const joined = owner.grant(grantRequest({ sessionIds: [SESSION_B], authorize }));
    const { promise: followUpArrived, resolve } = Promise.withResolvers<CapturedGrant>();
    announceGrant = resolve;

    expect(joined).toBe(started.result);
    resolvePendingRpc(started.captured.frame.requestId, {}, started.captured.workerFp);
    const followUp = await followUpArrived;
    expect(followUp.frame.grantId).toBe(started.captured.frame.grantId);
    expect(followUp.frame.sessionIds).toEqual([SESSION_A, SESSION_B]);
    expect(grantFrames).toHaveLength(2);
    resolvePendingRpc(followUp.frame.requestId, {}, followUp.workerFp);
    const result = await started.result;
    expect(result.lease.sessionIds).toEqual([SESSION_A, SESSION_B]);
    expect(authorizedSessionSets).toContainEqual([SESSION_A, SESSION_B]);
  });

  test("preserves the acknowledged predecessor when a renewal install fails", async () => {
    const initial = await grantWithAck({ sessionIds: [SESSION_A] });
    const renewal = await startGrant({ sessionIds: [SESSION_A, SESSION_B] });
    rejectPendingRpc(renewal.captured.frame.requestId, "worker refused", renewal.captured.workerFp);
    const error = await connectErrorFrom(renewal.result);

    expect(error.code).toBe(Code.Internal);
    expect(renewal.captured.frame.grantId).toBe(initial.lease.grantId);
    expect(owner.list()).toMatchObject([{
      grantId: initial.lease.grantId,
      sessionIds: [SESSION_A],
    }]);
  });

  test("rebinds a same-process reconnect and narrows scope without changing its id", async () => {
    const broad = await grantWithAck({ sessionIds: [SESSION_A, SESSION_B] });
    const reconnected = installWorker(WORKER_A, "epoch-a");
    const rebound = owner.ownedGrant(OWNER_KEY, TAB_ID, WORKER_A, broad.lease.grantId);

    expect(rebound).not.toBeNull();
    expect(rebound!.workerHandle).toBe(reconnected);
    expect(rebound!.sessionIds).toEqual([SESSION_A, SESSION_B]);
    const narrowed = await grantWithAck({ sessionIds: [SESSION_A] });
    expect(narrowed.lease.grantId).toBe(broad.lease.grantId);
    expect(narrowed.lease.workerHandle).toBe(reconnected);
    expect(grantFrames[1]!.frame.grantId).toBe(broad.lease.grantId);
    expect(grantFrames[1]!.frame.sessionIds).toEqual([SESSION_A]);
    expect(owner.list()).toMatchObject([{ sessionIds: [SESSION_A] }]);
  });

  test("mints a new id only when the worker process epoch changes", async () => {
    const initial = await grantWithAck();
    expect(owner.ownedGrant(OWNER_KEY, TAB_ID, WORKER_A, initial.lease.grantId)).not.toBeNull();
    installWorker(WORKER_A, "epoch-a-restarted");
    expect(owner.ownedGrant(OWNER_KEY, TAB_ID, WORKER_A, initial.lease.grantId)).toBeNull();
    const replacement = await grantWithAck();

    expect(replacement.lease.grantId).not.toBe(initial.lease.grantId);
    expect(replacement.lease.workerEpoch).toBe("epoch-a-restarted");
    expect(grantFrames[1]!.frame.workerEpoch).toBe("epoch-a-restarted");
  });

  test("rechecks live session authority after ACK before committing a lease", async () => {
    let authorized = true;
    let authorizationChecks = 0;
    const started = await startGrant({
      authorize: async () => {
        authorizationChecks += 1;
        if (!authorized) throw new ConnectError("session unavailable", Code.NotFound);
      },
    });
    authorized = false;
    resolvePendingRpc(started.captured.frame.requestId, {}, started.captured.workerFp);
    const error = await connectErrorFrom(started.result);

    expect(error.code).toBe(Code.NotFound);
    expect(authorizationChecks).toBe(2);
    expect(owner.list()).toEqual([]);
  });

  test("rejects an ACK that arrives after its captured worker handle changes", async () => {
    const started = await startGrant();
    installWorker(WORKER_A, "epoch-a-replaced-before-ack");
    resolvePendingRpc(started.captured.frame.requestId, {}, started.captured.workerFp);
    const error = await connectErrorFrom(started.result);

    expect(error.code).toBe(Code.Unavailable);
    expect(owner.list()).toEqual([]);
  });

  test("broadcasts device revocation after coordinator lease state is restarted", async () => {
    await grantWithAck();
    owner.dispose();
    owner = new TerminalGrantOwner();
    owner.revokeDevice(DEVICE_FP);

    expect(owner.list()).toEqual([]);
    expect(revokeFrames).toEqual([
      { workerFp: WORKER_A, deviceFingerprint: DEVICE_FP },
      { workerFp: WORKER_B, deviceFingerprint: DEVICE_FP },
    ]);
  });

  test("retires direct transport before the current worker handle is fenced", async () => {
    await grantWithAck();
    sendOrder = [];
    owner.retireWorker(WORKER_A, "worker_deleted");

    expect(sendOrder).toEqual(["terminalDirectRetire"]);
    expect(retireFrames).toHaveLength(1);
    expect(retireFrames[0]!.workerFp).toBe(WORKER_A);
    expect(retireFrames[0]!.frame.workerEpoch).toBe("epoch-a");
    expect(retireFrames[0]!.frame.reason).toBe("worker_deleted");
    expect(owner.list()).toEqual([]);
    fenceWorkerCredential(WORKER_A);
    expect(connectWorkers.get(WORKER_A)).toBeUndefined();
  });

  test("expires only its coordinator lease record and notifies subscribers", async () => {
    const result = await grantWithAck();
    const events: string[] = [];
    owner.subscribeInvalidation((event) => events.push(event.kind));
    owner.sweep(result.lease.expiresAtMs);

    expect(owner.list()).toEqual([]);
    expect(events).toEqual(["grant_expired"]);
  });
});
