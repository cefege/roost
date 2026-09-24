// Sends attachment status control to the exact current worker handle. The
// coordinator registers its typed result waiter first, then sends this bounded
// request without attachment bytes. A status result never travels through the
// legacy generic JSON reply path.

import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerDownSchema,
  DAttachmentDirectStatusRequestSchema,
} from "@roost/protocol/proto/worker_transport_pb";
import { currentRoutableWorker } from "./worker-send-target.ts";
import type { WorkerHandle } from "./worker-registry.ts";

export interface AttachmentDirectStatusRequestSend {
  readonly requestId: string;
  readonly sessionId: string;
  readonly uploadId: string;
}

/** Sends only while the captured source object remains the routable worker generation. */
export function sendAttachmentDirectStatusRequest(
  worker: WorkerHandle,
  message: AttachmentDirectStatusRequestSend,
): boolean {
  if (currentRoutableWorker(worker.workerFp) !== worker) return false;
  try {
    return worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "attachmentDirectStatusRequest",
        value: create(DAttachmentDirectStatusRequestSchema, {
          requestId: message.requestId,
          sessionId: message.sessionId,
          uploadId: message.uploadId,
        }),
      },
    })) !== 0;
  } catch {
    return false;
  }
}
