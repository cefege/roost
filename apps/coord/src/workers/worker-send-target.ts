// Resolves the only worker generation allowed to receive coordinator frames.
// All outbound worker senders share this gate so an unready, revoked, or
// superseded connection can never accept work through a stale handle.

import { connectWorkers, type WorkerHandle } from "./worker-registry.ts";

export function currentRoutableWorker(workerFp: string): WorkerHandle | null {
  const worker = connectWorkers.get(workerFp);
  return worker?.ready && !worker.revoked ? worker : null;
}
