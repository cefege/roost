// Settles pending and admitted terminal stream dispatch tasks.
// TerminalStreamDispatcher owns route resolution and worker lanes; this owner
// removes task state, releases physical admission slots, and resolves the
// controller-facing completion without retaining terminal payloads.
import type { WTerminalStreamResult } from "@roost/shared/proto/worker_transport_pb";
import { log } from "@roost/shared/log";
import {
  unsentTerminalWorkerRequest,
  type TerminalWorkerRequest,
} from "./worker-send.ts";
import {
  finishRouteVerification,
  releaseWorkerAdmissionSlot,
  type TerminalStreamTask,
  type WorkerDispatchLane,
} from "./terminal-stream-dispatcher-lane.ts";
import type { TerminalStreamDispatchCancellation } from "./terminal-stream-dispatcher-types.ts";

interface TerminalStreamTaskSettlerDependencies {
  isPending(task: TerminalStreamTask): boolean;
  workerGenerationChanged(task: TerminalStreamTask): boolean;
  removeIdleLane(lane: WorkerDispatchLane): void;
  scheduleDrain(): void;
  armDeadlineTimer(): void;
}

export class TerminalStreamTaskSettler {
  constructor(
    private readonly pendingBySession: Map<string, TerminalStreamTask>,
    private readonly dependencies: TerminalStreamTaskSettlerDependencies,
  ) {}

  promoteInFlight(
    task: TerminalStreamTask,
    request: TerminalWorkerRequest<WTerminalStreamResult>,
  ): void {
    if (!this.dependencies.isPending(task)) {
      void request.result.catch(() => undefined);
      this.releaseWorkerSlot(task);
      return;
    }
    this.pendingBySession.delete(task.message.sessionId);
    task.state = "in_flight";
    this.dependencies.armDeadlineTimer();
    task.resolve({
      kind: "request", request, workerGenerationReplaced: () => task.retainedAfterWorkerReplacement,
    });
    void request.result.then(
      () => this.finishInFlight(task),
      () => this.finishInFlight(task),
    );
  }

  completeUnsent(task: TerminalStreamTask, reason: string, expired: boolean): void {
    this.completeRequest(task, unsentTerminalWorkerRequest(reason, expired));
  }

  completeRequest(
    task: TerminalStreamTask,
    request: TerminalWorkerRequest<WTerminalStreamResult>,
  ): void {
    if (!this.dependencies.isPending(task)) {
      void request.result.catch(() => undefined);
      return;
    }
    this.removePending(task);
    task.state = "settled";
    void request.result.catch(() => undefined);
    task.resolve({ kind: "request", request });
  }

  cancelPending(sessionId: string, reason: TerminalStreamDispatchCancellation): void {
    const task = this.pendingBySession.get(sessionId);
    if (task) this.cancelTask(task, reason);
  }

  cancelTask(task: TerminalStreamTask, reason: TerminalStreamDispatchCancellation): void {
    if (!this.dependencies.isPending(task)) return;
    this.removePending(task);
    task.state = "settled";
    task.resolve({ kind: "cancelled", reason });
    log.debug("terminal-stream-dispatcher", "cancelled", {
      session_id: task.message.sessionId,
      stream_id: task.message.streamId,
      worker_fp: task.route?.workerFp,
      reason,
    });
  }

  private finishInFlight(task: TerminalStreamTask): void {
    if (task.state !== "in_flight") return;
    if (task.retainedAfterWorkerReplacement || this.dependencies.workerGenerationChanged(task)) {
      task.retainedAfterWorkerReplacement = true;
      this.dependencies.armDeadlineTimer();
      return;
    }
    task.state = "settled";
    this.releaseWorkerSlot(task);
  }

  private removePending(task: TerminalStreamTask): void {
    const lane = task.lane;
    if (this.pendingBySession.get(task.message.sessionId) === task) {
      this.pendingBySession.delete(task.message.sessionId);
    }
    if (task.state === "queued" && task.lane) {
      const index = task.lane.ready.indexOf(task);
      if (index >= 0) task.lane.ready.splice(index, 1);
    }
    finishRouteVerification(task);
    this.releaseWorkerSlot(task);
    if (lane) this.dependencies.removeIdleLane(lane);
    this.dependencies.armDeadlineTimer();
  }

  private releaseWorkerSlot(task: TerminalStreamTask): void {
    const lane = releaseWorkerAdmissionSlot(task);
    if (!lane) return;
    this.dependencies.removeIdleLane(lane);
    this.dependencies.scheduleDrain();
  }
}
