import { createHash } from "node:crypto";
import * as fs from "node:fs";
import { dirname, join } from "node:path";
import type { Database } from "bun:sqlite";
import {
  sendCoordinatorMovePrepare,
  sendCoordinatorRelocate,
  sendCoordinatorSnapshotChunk,
  sendCoordinatorSnapshotStart,
} from "../connect/worker-send.ts";
import { connectWorkers } from "../connect/worker-registry.ts";
import type { CoordinatorMoveRuntime, MoveSnapshot, MoveWorker } from "./runtime.ts";
import type { MovePhase } from "./state.ts";

const CHUNK_SIZE = 1024 * 1024;

export function createBunCoordinatorMoveRuntime(options: {
  sqlite: Database;
  dbPath: string;
  coordKeyPath: string;
  authorizedKeysPath: string;
  handoffPath: string;
  publishRelocation: (state: MoveSnapshot) => void;
}): CoordinatorMoveRuntime {
  async function callTarget(state: MoveSnapshot, action: "CHECK" | "PREPARE"): Promise<void> {
    await sendCoordinatorMovePrepare(state.targetWorkerFp, {
      handoffId: state.handoffId, sourceUrl: state.sourceUrl, targetUrl: state.targetUrl,
      expectedCoordKid: state.expectedCoordKid, expectedGitSha: state.expectedGitSha,
      estimatedDbSize: BigInt(fs.statSync(options.dbPath).size), action,
    });
  }

  async function targetRequest(state: MoveSnapshot, path: string, method: "GET" | "POST"): Promise<Response> {
    let lastError: unknown;
    for (let attempt = 0; attempt < 3; attempt++) {
      try {
        const response = await fetch(`${state.targetUrl}${path}`, {
          method,
          headers: {
            "x-roost-handoff-id": state.handoffId,
            "x-roost-handoff-secret": state.secret,
          },
          signal: AbortSignal.timeout(5_000),
        });
        if (response.status < 500 || attempt === 2) return response;
      } catch (error) {
        lastError = error;
      }
      await Bun.sleep(100);
    }
    throw lastError ?? new Error(`target request failed: ${path}`);
  }

  async function reconnectWorkers(workers: MoveWorker[], timeoutMs: number): Promise<void> {
    const previous = new Map<string, ReturnType<typeof connectWorkers.get>>();
    for (const worker of workers) {
      const handle = connectWorkers.get(worker.fp);
      if (!handle?.close) throw new Error(`worker ${worker.fp} cannot reconnect to target`);
      previous.set(worker.fp, handle);
      handle.close();
    }
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      if (workers.every((worker) => {
        const current = connectWorkers.get(worker.fp);
        return current !== undefined && current !== previous.get(worker.fp);
      })) return;
      await Bun.sleep(50);
    }
    throw new Error("workers did not reconnect to target coordinator");
  }

  return {
    async checkTarget(target: MoveWorker, expectedGitSha: string, estimatedDbSize: number): Promise<string | null> {
      try {
        await sendCoordinatorMovePrepare(target.fp, {
          handoffId: crypto.randomUUID(), sourceUrl: "https://check.invalid", targetUrl: `https://${target.reachableAddr}:4102`,
          expectedCoordKid: "preflight", expectedGitSha, estimatedDbSize: BigInt(estimatedDbSize), action: "CHECK",
        });
        return null;
      } catch (error) {
        return (error as Error).message;
      }
    },
    async prepareTarget(state) { await callTarget(state, "PREPARE"); },
    async stageWorker(worker, state) {
      await sendCoordinatorRelocate(worker.fp, { handoffId: state.handoffId, sourceUrl: state.sourceUrl, targetUrl: state.targetUrl, action: "STAGE" });
    },
    async activateWorker(worker, state) {
      await sendCoordinatorRelocate(worker.fp, { handoffId: state.handoffId, sourceUrl: state.sourceUrl, targetUrl: state.targetUrl, action: "ACTIVATE" });
    },
    async commitWorker(worker, state) {
      await sendCoordinatorRelocate(worker.fp, { handoffId: state.handoffId, sourceUrl: state.sourceUrl, targetUrl: state.targetUrl, action: "COMMIT" });
    },
    async abortWorker(worker, state) {
      await sendCoordinatorRelocate(worker.fp, { handoffId: state.handoffId, sourceUrl: state.sourceUrl, targetUrl: state.targetUrl, action: "ABORT" });
    },
    async copySnapshot(state) {
      const handoffDir = join(dirname(options.handoffPath), "handoffs", state.handoffId);
      fs.mkdirSync(handoffDir, { recursive: true, mode: 0o700 });
      const snapshot = join(handoffDir, "coordinator_v2.snapshot");
      options.sqlite.prepare("VACUUM INTO ?").run(snapshot);
      const size = fs.statSync(snapshot).size;
      const hasher = createHash("sha256");
      const chunk = new Uint8Array(CHUNK_SIZE);
      const hashFd = fs.openSync(snapshot, "r");
      try {
        for (;;) {
          const read = fs.readSync(hashFd, chunk, 0, chunk.length, null);
          if (read === 0) break;
          hasher.update(chunk.subarray(0, read));
        }
      } finally {
        fs.closeSync(hashFd);
      }
      const receipt = sendCoordinatorSnapshotStart(state.targetWorkerFp, {
        handoffId: state.handoffId, totalSize: BigInt(size), sha256: hasher.digest("hex"),
        coordKeyPem: fs.readFileSync(options.coordKeyPath), authorizedKeys: fs.readFileSync(options.authorizedKeysPath),
        secretSha256: state.secretSha256, expectedWorkerFps: state.expectedWorkerFps,
      });
      const streamFd = fs.openSync(snapshot, "r");
      try {
        for (let offset = 0, seq = 0; offset < size; seq++) {
          const read = fs.readSync(streamFd, chunk, 0, Math.min(chunk.length, size - offset), null);
          if (read === 0) throw new Error("coordinator snapshot ended unexpectedly");
          offset += read;
          sendCoordinatorSnapshotChunk(state.targetWorkerFp, {
            handoffId: state.handoffId, seq, data: chunk.subarray(0, read), last: offset === size,
          });
        }
      } finally {
        fs.closeSync(streamFd);
      }
      await receipt.promise;
    },
    async reconnectWorkers(workers, timeoutMs) {
      await reconnectWorkers(workers, timeoutMs);
    },
    async waitForWorkers(state, timeoutMs) {
      const deadline = Date.now() + timeoutMs;
      while (Date.now() < deadline) {
        try {
          const response = await targetRequest(state, "/internal/coord-handoff/status", "GET");
          if (response.ok) {
            const status = await response.json() as { connected_worker_fps?: string[] };
            const connected = new Set(status.connected_worker_fps ?? []);
            if (state.expectedWorkerFps.every((workerFp) => connected.has(workerFp))) return;
          }
        } catch {
          // Target may still be launching.
        }
        await Bun.sleep(1_000);
      }
      throw new Error("expected workers did not reach target coordinator");
    },
    async targetStatus(state): Promise<MovePhase | null> {
      try {
        const response = await targetRequest(state, "/internal/coord-handoff/status", "GET");
        if (!response.ok) return null;
        const body = await response.json() as { phase?: MovePhase };
        return body.phase ?? null;
      } catch {
        return null;
      }
    },
    async commitTarget(state) {
      const response = await targetRequest(state, "/internal/coord-handoff/commit", "POST");
      if (!response.ok) throw new Error(`target commit failed: ${await response.text()}`);
    },
    async abortTarget(state) {
      try {
        const response = await targetRequest(state, "/internal/coord-handoff/abort", "POST");
        if (!response.ok) throw new Error(`target abort failed: ${await response.text()}`);
      } catch (error) {
        // Before the staged target coordinator starts, its worker is still
        // attached here. Once it starts, the authenticated internal endpoint
        // above owns the rollback. This fallback covers only the former.
        await sendCoordinatorRelocate(state.targetWorkerFp, {
          handoffId: state.handoffId, sourceUrl: state.sourceUrl, targetUrl: state.targetUrl, action: "ABORT",
        }).catch(() => { throw error; });
      }
    },
    async targetHealthy(state) {
      const health = await fetch(`${state.targetUrl}/roost.v1.CoordinatorService/MiscHealth`, { signal: AbortSignal.timeout(5_000) });
      if (!health.ok) throw new Error("target coordinator health check failed");
      const status = await targetRequest(state, "/internal/coord-handoff/status", "GET");
      if (!status.ok) throw new Error("target coordinator status check failed");
      const body = await status.json() as { connected_worker_fps?: string[] };
      const connected = new Set(body.connected_worker_fps ?? []);
      if (!state.expectedWorkerFps.every((workerFp) => connected.has(workerFp))) {
        throw new Error("target workers are not all routable");
      }
    },
    publishRelocation: options.publishRelocation,
  };
}
