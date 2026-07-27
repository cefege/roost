import type { MovePhase } from "./state.ts";

export interface MoveWorker {
  fp: string;
  label: string;
  os: string;
  gitSha: string | null;
  reachableAddr: string | null;
  online: boolean;
}

export interface CoordinatorMoveRuntime {
  checkTarget(target: MoveWorker, expectedGitSha: string, estimatedDbSize: number): Promise<string | null>;
  prepareTarget(state: MoveSnapshot): Promise<void>;
  stageWorker(worker: MoveWorker, state: MoveSnapshot): Promise<void>;
  activateWorker(worker: MoveWorker, state: MoveSnapshot): Promise<void>;
  commitWorker(worker: MoveWorker, state: MoveSnapshot): Promise<void>;
  abortWorker(worker: MoveWorker, state: MoveSnapshot): Promise<void>;
  copySnapshot(state: MoveSnapshot): Promise<void>;
  waitForWorkers(state: MoveSnapshot, timeoutMs: number): Promise<void>;
  targetStatus(state: MoveSnapshot): Promise<MovePhase | null>;
  commitTarget(state: MoveSnapshot): Promise<void>;
  abortTarget(state: MoveSnapshot): Promise<void>;
  targetHealthy(state: MoveSnapshot): Promise<void>;
  publishRelocation(state: MoveSnapshot): void;
}

export interface MoveSnapshot {
  handoffId: string;
  phase: MovePhase;
  sourceUrl: string;
  targetUrl: string;
  targetWorkerFp: string;
  expectedWorkerFps: string[];
  expectedCoordKid: string;
  expectedGitSha: string;
  secret: string;
  secretSha256: string;
}
