// Source coordinator preflight lives here so move orchestration can focus on phase control.
// CoordinatorMoveOrchestrator calls it while holding the durable move transaction.
// It depends on the worker registry, target runtime probe, coordinator version, and database size.
// Blocker ordering, codes, and messages are part of the coordinator-move API and must remain stable.

import * as fs from "node:fs";
import { COORD_GIT_SHA } from "../git-sha.ts";
import type { MoveWorker } from "./runtime.ts";
import { isTerminalPhase } from "./state.ts";
import type { CoordinatorMoveOptions } from "./target-orchestrator.ts";

export type MoveBlockerCode =
  | "move_in_progress" | "public_url_unavailable" | "target_same_as_source"
  | "target_offline" | "target_address_missing"
  | "target_version_mismatch" | "worker_offline" | "worker_version_mismatch"
  | "target_coord_active" | "target_prepare_failed" | "insufficient_disk";

export interface MoveBlocker { code: MoveBlockerCode; message: string; workerFp?: string }
export interface MovePreflight { eligible: boolean; sourceUrl: string; targetUrl: string; blockers: MoveBlocker[] }

const SHA8 = COORD_GIT_SHA.slice(0, 8);

function publicTargetUrl(worker: MoveWorker): string {
  return worker.reachableAddr ? `https://${worker.reachableAddr}:4102` : "";
}

function blocker(code: MoveBlockerCode, message: string, workerFp?: string): MoveBlocker {
  return workerFp ? { code, message, workerFp } : { code, message };
}

function targetProbeBlocker(target: MoveWorker, detail: string): MoveBlocker {
  if (detail.includes("target already has an active coordinator")) {
    return blocker("target_coord_active", `${target.label} already runs a different active coordinator.`, target.fp);
  }
  if (detail.includes("target URL does not match this worker's Tailscale address")) {
    return blocker(
      "target_address_missing",
      `${target.label} does not recognise ${publicTargetUrl(target)} as its own Tailscale address.`,
      target.fp,
    );
  }
  const disk = /insufficient disk: required (\d+), available (\d+)/.exec(detail);
  if (disk) {
    return blocker(
      "insufficient_disk",
      `${target.label} needs at least ${disk[1]} bytes free; ${disk[2]} bytes are available.`,
      target.fp,
    );
  }
  return blocker("target_prepare_failed", `${target.label} could not prepare: ${detail}`, target.fp);
}

export async function preflightSourceMove(
  options: CoordinatorMoveOptions,
  dashboardId: string,
  targetWorkerFp: string,
): Promise<MovePreflight> {
  const sourceUrl = options.cfg.publicUrl ?? "";
  const workers = await options.workers(dashboardId);
  const target = workers.find((worker) => worker.fp === targetWorkerFp);
  const targetUrl = target ? publicTargetUrl(target) : "";
  const blockers: MoveBlocker[] = [];
  const previous = options.store.load();
  if (previous && !isTerminalPhase(previous.phase)) blockers.push(blocker("move_in_progress", "Another coordinator move is already in progress."));
  if (!sourceUrl) blockers.push(blocker("public_url_unavailable", "Set ROOST_COORDINATOR_PUBLIC_URL on the current coordinator and restart it."));
  if (!target) {
    blockers.push(blocker("target_offline", "Bring the selected machine online before moving the coordinator.", targetWorkerFp));
  } else {
    const sourceHost = sourceUrl ? new URL(sourceUrl).hostname.toLowerCase() : "";
    if (sourceHost && target.reachableAddr?.toLowerCase() === sourceHost) blockers.push(blocker("target_same_as_source", "This machine already hosts the coordinator.", target.fp));
    if (!target.online) blockers.push(blocker("target_offline", `Bring ${target.label} online before moving the coordinator.`, target.fp));
    if (!target.reachableAddr) blockers.push(blocker("target_address_missing", `${target.label} has not reported a Tailscale address.`, target.fp));
    if (target.gitSha !== COORD_GIT_SHA) blockers.push(blocker("target_version_mismatch", `Deploy coordinator version ${SHA8} to ${target.label} first.`, target.fp));
    if (target.online && target.reachableAddr && target.gitSha === COORD_GIT_SHA) {
      // PREPARE stats dbPath size; the target sizes its disk check off it.
      const check = await options.runtime.checkTarget(dashboardId, target, COORD_GIT_SHA, fs.statSync(options.cfg.dbPath).size);
      if (check) blockers.push(targetProbeBlocker(target, check));
    }
  }
  for (const worker of workers) {
    if (worker.fp === targetWorkerFp) continue;
    if (!worker.online) blockers.push(blocker("worker_offline", `Bring ${worker.label} online or remove it from Machines before moving.`, worker.fp));
    else if (worker.gitSha !== COORD_GIT_SHA) blockers.push(blocker("worker_version_mismatch", `Deploy coordinator version ${SHA8} to ${worker.label} first.`, worker.fp));
  }
  return { eligible: blockers.length === 0, sourceUrl, targetUrl, blockers };
}
