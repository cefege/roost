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
    return fetch(`${state.targetUrl}${path}`, {
      method,
      headers: {
        "x-roost-handoff-id": state.handoffId,
        "x-roost-handoff-secret": state.secret,
      },
      signal: AbortSignal.timeout(5_000),
    });
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
      const data = fs.readFileSync(snapshot);
      const sha256 = createHash("sha256").update(data).digest("hex");
      const receipt = sendCoordinatorSnapshotStart(state.targetWorkerFp, {
        handoffId: state.handoffId, totalSize: BigInt(data.length), sha256,
        coordKeyPem: fs.readFileSync(options.coordKeyPath), authorizedKeys: fs.readFileSync(options.authorizedKeysPath),
        secretSha256: state.secretSha256, expectedWorkerFps: state.expectedWorkerFps,
      });
      for (let offset = 0, seq = 0; offset < data.length; offset += CHUNK_SIZE, seq++) {
        const end = Math.min(offset + CHUNK_SIZE, data.length);
        sendCoordinatorSnapshotChunk(state.targetWorkerFp, { handoffId: state.handoffId, seq, data: data.subarray(offset, end), last: end === data.length });
      }
      await receipt.promise;
    },
    async waitForWorkers(state) {
      const deadline = Date.now() + 60_000;
      while (Date.now() < deadline) {
        const status = await targetRequest(state, "/internal/coord-handoff/status", "GET");
        if (status.ok) return;
        await Bun.sleep(1_000);
      }
      throw new Error("target coordinator did not become reachable");
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
      const response = await targetRequest(state, "/internal/coord-handoff/abort", "POST");
      if (!response.ok) throw new Error(`target abort failed: ${await response.text()}`);
    },
    async targetHealthy(state) {
      const response = await fetch(`${state.targetUrl}/roost.v1.CoordinatorService/MiscHealth`, { signal: AbortSignal.timeout(5_000) });
      if (!response.ok) throw new Error("target coordinator health check failed");
    },
    publishRelocation: options.publishRelocation,
  };
}
