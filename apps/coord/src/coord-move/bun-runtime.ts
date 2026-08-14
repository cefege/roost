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
import { createSqliteSnapshot } from "../db/snapshot.ts";
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
        // CHECK on the worker is only statfs + a service-liveness probe, so a
        // wedged target must not hold the move dialog open for the 180s
        // PREPARE default.
        await sendCoordinatorMovePrepare(target.fp, {
          handoffId: crypto.randomUUID(), sourceUrl: "https://check.invalid", targetUrl: `https://${target.reachableAddr}:4102`,
          expectedCoordKid: "preflight", expectedGitSha, estimatedDbSize: BigInt(estimatedDbSize), action: "CHECK",
        }, 10_000);
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
      // The worker's own commit() waits up to 30s for its event drain before
      // rewriting its service definition; coord must outlast that.
      await sendCoordinatorRelocate(worker.fp, { handoffId: state.handoffId, sourceUrl: state.sourceUrl, targetUrl: state.targetUrl, action: "COMMIT" }, 60_000);
    },
    async abortWorker(worker, state) {
      // Matches commitWorker: the worker's rollback also waits on its drain,
      // and a 30s budget records FAILED on a rollback that actually worked.
      await sendCoordinatorRelocate(worker.fp, { handoffId: state.handoffId, sourceUrl: state.sourceUrl, targetUrl: state.targetUrl, action: "ABORT" }, 60_000);
    },
    async copySnapshot(state) {
      const handoffDir = join(dirname(options.handoffPath), "handoffs", state.handoffId);
      fs.mkdirSync(handoffDir, { recursive: true, mode: 0o700 });
      const snapshot = join(handoffDir, "coordinator_v2.snapshot");
      const { size, sha256 } = createSqliteSnapshot(options.sqlite, snapshot);
      const chunk = new Uint8Array(CHUNK_SIZE);
      try {
        // One budget covers the whole transfer plus the target's fsync, sha256,
        // integrity_check, renames and installer run — scale it with payload.
        const receipt = sendCoordinatorSnapshotStart(state.targetWorkerFp, {
          handoffId: state.handoffId, totalSize: BigInt(size), sha256,
          coordKeyPem: fs.readFileSync(options.coordKeyPath),
          // authorized_keys.roost is an OPTIONAL bootstrap-import file — main.ts
          // guards its read with existsSync and the authoritative keys live in
          // the DB we just vacuumed. A coordinator that never had one must
          // still be movable.
          authorizedKeys: fs.existsSync(options.authorizedKeysPath)
            ? fs.readFileSync(options.authorizedKeysPath)
            : new Uint8Array(),
          secretSha256: state.secretSha256, expectedWorkerFps: state.expectedWorkerFps,
        }, 120_000 + Math.ceil(size / 1_000_000) * 1_000);
        const handleAtStart = connectWorkers.get(state.targetWorkerFp);
        const streamFd = fs.openSync(snapshot, "r");
        try {
          for (let offset = 0, seq = 0; offset < size; seq++) {
            const read = fs.readSync(streamFd, chunk, 0, Math.min(chunk.length, size - offset), null);
            if (read === 0) throw new Error("coordinator snapshot ended unexpectedly");
            offset += read;
            // Bun's ws.send: 0 = dropped, -1 = enqueued under backpressure, >0 = bytes.
            if (sendCoordinatorSnapshotChunk(state.targetWorkerFp, {
              handoffId: state.handoffId, seq, data: chunk.subarray(0, read), last: offset === size,
            }) === 0) throw new Error(`coordinator snapshot chunk ${seq} was dropped`);
            // sendCoordinatorSnapshotChunk re-resolves the registry every call,
            // so a mid-transfer reconnect splits the stream across two sockets
            // while we'd measure backpressure on the dead one. Fail instead:
            // execute()'s catch turns this into a clean rollback.
            const live = connectWorkers.get(state.targetWorkerFp);
            if (live !== handleAtStart) throw new Error("target worker reconnected mid-snapshot");
            // Without this the whole DB buffers in coord's heap before a byte
            // drains — an OOM at the least recoverable phase.
            while ((live?.bufferedAmount?.() ?? 0) > 8 * CHUNK_SIZE) await Bun.sleep(5);
          }
        } finally {
          fs.closeSync(streamFd);
        }
        await receipt.promise;
      } finally {
        // Otherwise every move leaves a full DB copy behind, and the target's
        // insufficient_disk preflight sizes only the live DB.
        fs.rmSync(snapshot, { force: true });
      }
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
      let response: Response;
      try {
        response = await targetRequest(state, "/internal/coord-handoff/abort", "POST");
      } catch (error) {
        // Transport error only: before the staged target coordinator starts,
        // its worker is still attached here and owns the rollback. A 4xx/5xx
        // answer means the target coord IS up and has ruled on the abort —
        // uninstalling it then would tear down a live coordinator.
        await sendCoordinatorRelocate(state.targetWorkerFp, {
          handoffId: state.handoffId, sourceUrl: state.sourceUrl, targetUrl: state.targetUrl, action: "ABORT",
        }).catch(() => { throw error; });
        return;
      }
      if (!response.ok) throw new Error(`target abort failed: ${await response.text()}`);
    },
    async targetHealthy(state) {
      // MiscHealth is a Connect unary method: bun-handler.ts rejects non-POST with 405.
      const health = await fetch(`${state.targetUrl}/roost.v1.CoordinatorService/MiscHealth`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: "{}",
        signal: AbortSignal.timeout(5_000),
      });
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
