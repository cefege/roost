// Worker generation lifecycle mutations for TerminalStreamDispatcher. The dispatcher
// owns call ordering; this collaborator fences obsolete pending tasks before retirement
// removes a per-worker generation marker, and releases a permanently retired lane.
import {
  disposeWorkerDispatchLane,
  type TerminalStreamTask,
  type WorkerDispatchLane,
} from "./terminal-stream-dispatcher-lane.ts";

type CancelTask = (task: TerminalStreamTask) => void;

export function replaceWorkerDispatchGeneration(
  pendingTasks: Iterable<TerminalStreamTask>,
  lanes: Map<string, WorkerDispatchLane>,
  workerReplacementEpochs: Map<string, number>,
  workerFp: string,
  replacementEpoch: number,
  cancelTask: CancelTask,
): void {
  workerReplacementEpochs.set(workerFp, replacementEpoch);
  const lane = lanes.get(workerFp);
  if (lane) for (const task of lane.admittedTasks) task.retainedAfterWorkerReplacement = true;
  for (const task of [...pendingTasks]) {
    if (task.route?.workerFp === workerFp) cancelTask(task);
  }
}

export function retireWorkerDispatchGeneration(
  pendingTasks: Iterable<TerminalStreamTask>,
  lanes: Map<string, WorkerDispatchLane>,
  workerReplacementEpochs: Map<string, number>,
  workerFp: string,
  sessionIds: Iterable<string>,
  cancelTask: CancelTask,
): void {
  const retiredSessionIds = new Set(sessionIds);
  for (const task of [...pendingTasks]) {
    if (task.route?.workerFp === workerFp || retiredSessionIds.has(task.message.sessionId)) {
      cancelTask(task);
    }
  }
  const lane = lanes.get(workerFp);
  if (lane) {
    for (const task of lane.admittedTasks) task.retainedAfterWorkerReplacement = true;
    disposeWorkerDispatchLane(lane);
    lanes.delete(workerFp);
  }
  workerReplacementEpochs.delete(workerFp);
}
