// Private task and lane state for TerminalStreamDispatcher.
// The dispatcher uses these records to preserve a lane while asynchronous route
// verification is outstanding and to retain actual admissions across replacement.
import type { TerminalStreamRoute } from "./terminal-view-stream-controller-types.ts";
import type {
  TerminalStreamDispatchCompletion,
  TerminalStreamDispatchState,
} from "./terminal-stream-dispatcher-types.ts";

export type TerminalStreamTaskState =
  | "resolving"
  | "queued"
  | "verifying"
  | "in_flight"
  | "settled";

export interface TerminalStreamTask {
  state: TerminalStreamTaskState;
  message: TerminalStreamDispatchState;
  route: TerminalStreamRoute | null;
  workerGeneration: unknown | null;
  lane: WorkerDispatchLane | null;
  holdsWorkerSlot: boolean;
  routeVerificationPending: boolean;
  retainedAfterWorkerReplacement: boolean;
  replacementEpoch: number;
  resolve: (completion: TerminalStreamDispatchCompletion) => void;
}

export interface WorkerDispatchLane {
  workerFp: string;
  ready: TerminalStreamTask[];
  verifying: number;
  admittedTasks: Set<TerminalStreamTask>;
}

export function sameTerminalStreamRoute(
  left: TerminalStreamRoute,
  right: TerminalStreamRoute | null,
): boolean {
  return right !== null
    && left.workerFp === right.workerFp
    && left.channel === right.channel;
}

export function beginRouteVerification(task: TerminalStreamTask): void {
  const lane = task.lane;
  if (!lane) return;
  task.routeVerificationPending = true;
  lane.verifying += 1;
}

export function finishRouteVerification(task: TerminalStreamTask): void {
  if (!task.routeVerificationPending) return;
  task.routeVerificationPending = false;
  const lane = task.lane;
  if (lane && lane.verifying > 0) lane.verifying -= 1;
}

export function requeueAfterFullWorkerWindow(
  task: TerminalStreamTask,
  lane: WorkerDispatchLane,
): void {
  task.state = "queued";
  lane.ready.push(task);
  finishRouteVerification(task);
}

export function reserveWorkerAdmissionSlot(
  task: TerminalStreamTask,
  maximumInFlight: number,
): boolean {
  const lane = task.lane;
  if (!lane || lane.admittedTasks.size >= maximumInFlight) return false;
  task.holdsWorkerSlot = true;
  lane.admittedTasks.add(task);
  finishRouteVerification(task);
  return true;
}

export function releaseWorkerAdmissionSlot(task: TerminalStreamTask): WorkerDispatchLane | null {
  if (!task.holdsWorkerSlot) return null;
  task.holdsWorkerSlot = false;
  const lane = task.lane;
  task.lane = null;
  if (lane) lane.admittedTasks.delete(task);
  return lane;
}

export function releaseExpiredRetainedSlots(lane: WorkerDispatchLane): boolean {
  let released = false;
  for (const task of [...lane.admittedTasks]) {
    if (
      !task.retainedAfterWorkerReplacement
      || task.message.deadline.remainingMs() > 0
    ) continue;
    task.state = "settled";
    releaseWorkerAdmissionSlot(task);
    released = true;
  }
  return released;
}

export function earliestRetainedSlotDeadline(lane: WorkerDispatchLane): number {
  let shortestRemainingMs = Infinity;
  for (const task of lane.admittedTasks) {
    if (!task.retainedAfterWorkerReplacement) continue;
    shortestRemainingMs = Math.min(shortestRemainingMs, task.message.deadline.remainingMs());
  }
  return shortestRemainingMs;
}

export function disposeWorkerDispatchLane(lane: WorkerDispatchLane): void {
  for (const task of lane.admittedTasks) {
    task.holdsWorkerSlot = false;
    task.lane = null;
    task.state = "settled";
  }
  lane.admittedTasks.clear();
  lane.ready.length = 0;
  lane.verifying = 0;
}

export function getWorkerDispatchLane(
  lanes: Map<string, WorkerDispatchLane>,
  workerFp: string,
): WorkerDispatchLane {
  const existing = lanes.get(workerFp);
  if (existing) return existing;
  const lane: WorkerDispatchLane = { workerFp, ready: [], verifying: 0, admittedTasks: new Set() };
  lanes.set(workerFp, lane);
  return lane;
}
