// Sends the two local-terminal authorization frames to the worker generation
// currently authoritative in the shared registry: the acknowledged grant
// install and the fire-and-forget device revocation.
// Only a secret's SHA-256 digest ever reaches this module — the secret itself
// stays in the coordinator's authenticated RPC response to the browser.
// Called by connect/local-terminal-grants.ts, which owns the lease registry.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import {
  CoordWorkerDownSchema,
  DLocalTerminalGrantSchema,
  DLocalTerminalGrantRevokeSchema,
} from "@roost/shared/proto/worker_transport_pb";
import {
  createPendingRpc,
  rejectPendingRpcUnavailable,
} from "../router/pending-rpcs.ts";
import { currentRoutableWorker } from "./worker-send-target.ts";

// A browser is blocked on this install, so the ack waits well inside the
// browser's own RPC patience instead of the 30s pending-RPC default.
const GRANT_ACK_TIMEOUT_MS = 10_000;

export interface LocalTerminalGrantInstall {
  readonly grantId: string;
  readonly secretSha256: string;
  readonly sessionIds: readonly string[];
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly ttlMs: number;
}

/** Install one grant and wait for the worker's WRpcOk/WRpcError. Transport
 * loss rejects this exact install, so the coordinator can never hand a browser
 * a secret for a grant the worker does not hold. */
export function sendLocalTerminalGrantRequest(
  workerFp: string,
  message: LocalTerminalGrantInstall,
  timeoutMs = GRANT_ACK_TIMEOUT_MS,
): { requestId: string; promise: Promise<unknown> } {
  const worker = currentRoutableWorker(workerFp);
  if (!worker) throw new ConnectError("worker offline", Code.Unavailable);
  const pending = createPendingRpc(timeoutMs, workerFp);
  try {
    const sent = worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "localTerminalGrant",
        value: create(DLocalTerminalGrantSchema, {
          requestId: pending.request_id,
          grantId: message.grantId,
          secretSha256: message.secretSha256,
          sessionIds: [...message.sessionIds],
          deviceFingerprint: message.deviceFingerprint,
          tabId: message.tabId,
          ttlMs: message.ttlMs,
        }),
      },
    }));
    if (sent === 0) throw new Error("worker dropped the local terminal grant");
  } catch (error) {
    rejectPendingRpcUnavailable(
      pending.request_id,
      error instanceof Error ? error.message : String(error),
      workerFp,
    );
  }
  return { requestId: pending.request_id, promise: pending.promise };
}

/** Revocation is fire-and-forget: a consumed credential must not wait on the
 * worker whose reachability is exactly what is in doubt. Returns whether this
 * worker's transport admitted the frame. */
export function sendLocalTerminalGrantRevoke(
  workerFp: string,
  deviceFingerprint: string,
): boolean {
  const worker = currentRoutableWorker(workerFp);
  if (!worker) return false;
  try {
    return worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "localTerminalGrantRevoke",
        value: create(DLocalTerminalGrantRevokeSchema, { deviceFingerprint }),
      },
    })) !== 0;
  } catch {
    return false;
  }
}
