// Retains browser-route retirements that cannot reach an unroutable worker.
// TerminalInputRouteResults records closed Sync sockets here; worker-ready
// composition flushes only the same process epoch, because a restart cleared
// the worker-owned route table that the retirement would otherwise mutate.

import { currentRoutableWorker } from "../../workers/worker-send-target.ts";
import { sendTerminalInputRouteConnectionClosed } from "./worker-send-terminal-route.ts";

interface PendingTerminalRouteRetirement {
  readonly workerFp: string;
  readonly workerEpoch: string;
  readonly connectionIds: Set<string>;
}

export class TerminalInputRouteRetirements {
  readonly #pendingByTarget = new Map<string, PendingTerminalRouteRetirement>();

  retire(workerFp: string, workerEpoch: string, connectionId: string): void {
    const worker = currentRoutableWorker(workerFp);
    if (worker?.processEpoch === workerEpoch
      && sendTerminalInputRouteConnectionClosed(worker, workerEpoch, connectionId)) return;
    if (worker && worker.processEpoch !== workerEpoch) {
      this.dropReplacedEpoch(workerFp, worker.processEpoch);
      return;
    }
    const key = retirementKey(workerFp, workerEpoch);
    const pending = this.#pendingByTarget.get(key) ?? {
      workerFp,
      workerEpoch,
      connectionIds: new Set<string>(),
    };
    pending.connectionIds.add(connectionId);
    this.#pendingByTarget.set(key, pending);
  }

  flush(workerFp: string): void {
    const worker = currentRoutableWorker(workerFp);
    if (!worker?.processEpoch) return;
    this.dropReplacedEpoch(workerFp, worker.processEpoch);
    const key = retirementKey(workerFp, worker.processEpoch);
    const pending = this.#pendingByTarget.get(key);
    if (!pending) return;
    for (const connectionId of [...pending.connectionIds]) {
      if (sendTerminalInputRouteConnectionClosed(worker, pending.workerEpoch, connectionId)) {
        pending.connectionIds.delete(connectionId);
      }
    }
    if (pending.connectionIds.size === 0) this.#pendingByTarget.delete(key);
  }

  private dropReplacedEpoch(workerFp: string, currentEpoch: string | null): void {
    for (const [key, pending] of this.#pendingByTarget) {
      if (pending.workerFp === workerFp && pending.workerEpoch !== currentEpoch) {
        this.#pendingByTarget.delete(key);
      }
    }
  }
}

function retirementKey(workerFp: string, workerEpoch: string): string {
  return JSON.stringify([workerFp, workerEpoch]);
}
