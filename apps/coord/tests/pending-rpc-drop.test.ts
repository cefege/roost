// A5 regression: a dropped worker socket must reject THAT worker's in-flight
// pending RPCs immediately (browser fast-fails) instead of leaving them to
// hang until the 15-30s deadline. Drives the pending-rpcs table directly:
// createPendingRpc tags each entry with its worker_fp;
// rejectPendingRpcsForWorker(fp) walks + rejects only that fp's entries.
//
// Guards: worker-service.ts close() calls this when a WS drops AND no fresh
// connection re-registered the fp. From wf_728b67c1 Table A #5.

import { describe, test, expect } from "bun:test";
import {
  createPendingRpc,
  createPendingRpcWithId,
  rejectPendingRpcsForWorker,
  resolvePendingRpc,
  _pendingRpcStats,
} from "../src/router/pending-rpcs.ts";

const FP_A = "a".repeat(64);
const FP_B = "b".repeat(64);

const SHARED_UPLOAD_ID = "shared-upload-id";
describe("A5 — rejectPendingRpcsForWorker", () => {
  test("rejects only the dropped worker's RPCs; others survive", async () => {
    const a = createPendingRpc<{ ok: boolean }>(30_000, FP_A);
    const b = createPendingRpc<{ ok: boolean }>(30_000, FP_B);

    const n = rejectPendingRpcsForWorker(FP_A, "worker disconnected");
    expect(n).toBe(1);

    await expect(a.promise).rejects.toThrow("worker disconnected");

    // B is untouched — still pending, resolvable.
    const resolved = resolvePendingRpc(b.request_id, { ok: true });
    expect(resolved).toBe(true);
    await expect(b.promise).resolves.toEqual({ ok: true });
  });

  test("rejecting clears the entries (no leak, no double-reject)", async () => {
    const before = _pendingRpcStats().pending;
    const a = createPendingRpc<unknown>(30_000, FP_A);
    expect(_pendingRpcStats().pending).toBe(before + 1);
    rejectPendingRpcsForWorker(FP_A, "gone");
    a.promise.catch(() => { /* expected */ });
    expect(_pendingRpcStats().pending).toBe(before);
    // Second reject finds nothing.
    expect(rejectPendingRpcsForWorker(FP_A, "gone again")).toBe(0);
  });

  test("untagged RPCs are not collateral", async () => {
    const untagged = createPendingRpc<unknown>(30_000); // no fp
    expect(rejectPendingRpcsForWorker(FP_A, "gone")).toBe(0);
    // still resolvable
    expect(resolvePendingRpc(untagged.request_id, 1)).toBe(true);
    await expect(untagged.promise).resolves.toBe(1);
  });
});

describe("worker-scoped explicit request ids", () => {
  test("equal upload ids resolve independently across workers", async () => {
    const a = createPendingRpcWithId<{ worker: string }>(
      SHARED_UPLOAD_ID,
      30_000,
      FP_A,
    );
    const b = createPendingRpcWithId<{ worker: string }>(
      SHARED_UPLOAD_ID,
      30_000,
      FP_B,
    );
    expect(resolvePendingRpc(SHARED_UPLOAD_ID, { worker: "b" }, FP_B)).toBe(true);
    await expect(b.promise).resolves.toEqual({ worker: "b" });
    expect(resolvePendingRpc(SHARED_UPLOAD_ID, { worker: "a" }, FP_A)).toBe(true);
    await expect(a.promise).resolves.toEqual({ worker: "a" });
  });

  test("duplicate explicit ids for one worker reject instead of overwriting", () => {
    const first = createPendingRpcWithId(SHARED_UPLOAD_ID, 30_000, FP_A);
    expect(() => createPendingRpcWithId(
      SHARED_UPLOAD_ID,
      30_000,
      FP_A,
    )).toThrow("request_id is already pending for this worker");
    rejectPendingRpcsForWorker(FP_A, "test cleanup");
    void first.promise.catch(() => undefined);
  });
});
