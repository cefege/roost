// Sends direct-terminal authorization frames to one exact authenticated worker
// generation. Grant callers await the worker ACK before returning a browser
// secret; revocation and retirement remain synchronous best-effort controls.
// This module receives only secret digests and never logs their values.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import {
  CoordWorkerDownSchema,
  DLocalTerminalGrantSchema,
  DLocalTerminalGrantRevokeSchema,
  DTerminalDirectRetireSchema,
} from "@roost/protocol/proto/worker_transport_pb";
import {
  createPendingRpc,
  rejectPendingRpcUnavailable,
} from "../router/pending-rpcs.ts";
import type { TerminalDirectRetireReason } from "./terminal-grant-owner.ts";
import type { WorkerHandle } from "./worker-registry.ts";
import { currentRoutableWorker } from "./worker-send-target.ts";

const GRANT_ACK_TIMEOUT_MS = 10_000;

export interface LocalTerminalGrantInstall {
  readonly grantId: string;
  readonly secretSha256: string;
  readonly sessionIds: readonly string[];
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly ttlMs: number;
}

export interface PendingLocalTerminalGrantInstall {
  readonly requestId: string;
  readonly promise: Promise<unknown>;
}

/** Installs a grant only on the captured handle and worker boot epoch. */
export function sendLocalTerminalGrantRequest(
  worker: WorkerHandle,
  workerEpoch: string | null,
  message: LocalTerminalGrantInstall,
  timeoutMs = GRANT_ACK_TIMEOUT_MS,
): PendingLocalTerminalGrantInstall {
  if (!isExactRoutableWorker(worker, workerEpoch)) {
    throw new ConnectError("worker offline", Code.Unavailable);
  }
  const pending = createPendingRpc(timeoutMs, worker.workerFp);
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
          workerEpoch: workerEpoch ?? "",
        }),
      },
    }));
    if (sent === 0) throw new Error("worker dropped the local terminal grant");
  } catch {
    rejectPendingRpcUnavailable(pending.request_id, "worker transport unavailable", worker.workerFp);
  }
  return { requestId: pending.request_id, promise: pending.promise };
}

/** Broadcast callers capture each worker before invoking this exact-handle send. */
export function sendLocalTerminalGrantRevoke(
  worker: WorkerHandle,
  deviceFingerprint: string,
): boolean {
  if (!isExactRoutableWorker(worker, worker.processEpoch)) return false;
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

/** Retirement must be enqueued before the caller fences the captured worker handle. */
export function sendTerminalDirectRetire(
  worker: WorkerHandle,
  workerEpoch: string,
  reason: TerminalDirectRetireReason,
): boolean {
  if (!isExactRoutableWorker(worker, workerEpoch)) return false;
  try {
    return worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "terminalDirectRetire",
        value: create(DTerminalDirectRetireSchema, { workerEpoch, reason }),
      },
    })) !== 0;
  } catch {
    return false;
  }
}

function isExactRoutableWorker(worker: WorkerHandle, workerEpoch: string | null): boolean {
  return worker.processEpoch === workerEpoch
    && currentRoutableWorker(worker.workerFp) === worker;
}
