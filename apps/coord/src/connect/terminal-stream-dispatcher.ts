// Coordinator-owned scheduler for desired terminal stream states. TerminalViewStreamController
// submits one semantic stream state at a time; this owner coalesces unsent work,
// verifies the current route immediately before a socket write, and limits each
// worker against its 32-request physical stream-control window. Input and snapshots bypass it.
import type { WTerminalStreamResult } from "@roost/shared/proto/worker_transport_pb";
import { log } from "@roost/shared/log";
import {
  unsentTerminalWorkerRequest,
  type TerminalWorkerRequest,
} from "./worker-send.ts";
import {
  beginRouteVerification,
  disposeWorkerDispatchLane,
  earliestRetainedSlotDeadline,
  getWorkerDispatchLane,
  releaseExpiredRetainedSlots,
  requeueAfterFullWorkerWindow,
  reserveWorkerAdmissionSlot,
  sameTerminalStreamRoute,
  type TerminalStreamTask,
  type WorkerDispatchLane,
} from "./terminal-stream-dispatcher-lane.ts";
import { TerminalStreamTaskSettler } from "./terminal-stream-dispatcher-settler.ts";
import {
  replaceWorkerDispatchGeneration,
  retireWorkerDispatchGeneration,
} from "./terminal-stream-dispatcher-worker-lifecycle.ts";
import {
  TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER,
  type ClearTerminalStreamDispatcherTimer,
  type SetTerminalStreamDispatcherTimer,
  type TerminalStreamDispatchCancellation,
  type TerminalStreamDispatchCompletion,
  type TerminalStreamDispatchRequest,
  type TerminalStreamDispatchState,
  type TerminalStreamDispatcherOptions,
  type TerminalStreamDispatcherTimer,
} from "./terminal-stream-dispatcher-types.ts";

export class TerminalStreamDispatcher {
  private readonly pendingBySession = new Map<string, TerminalStreamTask>();
  private readonly lanes = new Map<string, WorkerDispatchLane>();
  private readonly workerReplacementEpochs = new Map<string, number>();
  private replacementEpoch = 0;
  private readonly setTimer: SetTerminalStreamDispatcherTimer;
  private readonly clearTimer: ClearTerminalStreamDispatcherTimer;
  private deadlineTimer: TerminalStreamDispatcherTimer | null = null;
  private readonly settler: TerminalStreamTaskSettler;
  private drainScheduled = false;
  private disposed = false;
  constructor(private readonly options: TerminalStreamDispatcherOptions) {
    this.setTimer = options.setTimer ?? setTimeout;
    this.clearTimer = options.clearTimer ?? clearTimeout;
    this.settler = new TerminalStreamTaskSettler(this.pendingBySession, {
      isPending: (task) => this.isPending(task),
      workerGenerationChanged: (task) => this.workerGenerationChanged(task),
      removeIdleLane: (lane) => this.removeIdleLane(lane),
      scheduleDrain: () => this.scheduleDrain(),
      armDeadlineTimer: () => this.armDeadlineTimer(),
    });
  }
  enqueue(message: TerminalStreamDispatchState): TerminalStreamDispatchRequest {
    if (this.disposed) {
      return {
        accepted: false,
        completion: Promise.resolve({ kind: "cancelled", reason: "dispatcher_disposed" }),
      };
    }
    if (message.deadline.remainingMs() <= 0) {
      const request = unsentTerminalWorkerRequest<WTerminalStreamResult>(
        "terminal stream budget expired before dispatch",
        true,
      );
      void request.result.catch(() => undefined);
      return { accepted: false, completion: Promise.resolve({ kind: "request", request }) };
    }
    this.settler.cancelPending(message.sessionId, "superseded");
    const deferred = Promise.withResolvers<TerminalStreamDispatchCompletion>();
    const task: TerminalStreamTask = {
      state: "resolving",
      message,
      route: null,
      workerGeneration: null,
      lane: null,
      holdsWorkerSlot: false,
      routeVerificationPending: false,
      retainedAfterWorkerReplacement: false,
      replacementEpoch: this.replacementEpoch,
      resolve: deferred.resolve,
    };
    this.pendingBySession.set(message.sessionId, task);
    this.armDeadlineTimer();
    void this.resolveInitialRoute(task);
    return { accepted: true, completion: deferred.promise };
  }
  cancelSession(
    sessionId: string,
    reason: "session_closed" | "superseded" = "session_closed",
  ): void {
    this.settler.cancelPending(sessionId, reason);
  }
  workerReplacement(workerFp: string): void {
    this.replacementEpoch += 1;
    replaceWorkerDispatchGeneration(
      this.pendingBySession.values(), this.lanes, this.workerReplacementEpochs,
      workerFp, this.replacementEpoch, (task) => this.settler.cancelTask(task, "worker_generation_replaced"),
    );
    this.armDeadlineTimer();
  }
  workerRetired(workerFp: string, sessionIds: Iterable<string>): void {
    retireWorkerDispatchGeneration(
      this.pendingBySession.values(), this.lanes, this.workerReplacementEpochs,
      workerFp, sessionIds, (task) => this.settler.cancelTask(task, "worker_generation_replaced"),
    );
    this.armDeadlineTimer();
  }
  dispose(): void {
    if (this.disposed) return;
    for (const task of [...this.pendingBySession.values()]) this.settler.cancelTask(task, "dispatcher_disposed");
    this.disposed = true;
    if (this.deadlineTimer !== null) this.clearTimer(this.deadlineTimer);
    this.deadlineTimer = null;
    for (const lane of this.lanes.values()) disposeWorkerDispatchLane(lane);
    this.lanes.clear();
  }
  private async resolveInitialRoute(task: TerminalStreamTask): Promise<void> {
    try {
      const route = await this.options.resolveRoute(task.message.sessionId);
      if (!this.isPending(task) || this.expireIfNeeded(task)) return;
      if (!route) {
        this.settler.completeUnsent(task, "terminal worker is unavailable", false);
        return;
      }
      if ((this.workerReplacementEpochs.get(route.workerFp) ?? 0) > task.replacementEpoch) {
        this.settler.cancelTask(task, "worker_generation_replaced");
        return;
      }
      const workerGeneration = this.options.currentWorker?.(route.workerFp) ?? null;
      if (this.options.currentWorker && workerGeneration === null) {
        this.settler.completeUnsent(task, "terminal worker is unavailable", false);
        return;
      }
      task.route = route;
      task.workerGeneration = workerGeneration;
      task.state = "queued";
      const lane = getWorkerDispatchLane(this.lanes, route.workerFp);
      task.lane = lane;
      lane.ready.push(task);
      log.debug("terminal-stream-dispatcher", "queued", {
        session_id: task.message.sessionId,
        stream_id: task.message.streamId,
        worker_fp: route.workerFp,
      });
      this.armDeadlineTimer();
      this.scheduleDrain();
    } catch (error) {
      if (this.isPending(task)) {
        this.settler.completeUnsent(
          task,
          error instanceof Error ? error.message : "terminal route resolution failed",
          false,
        );
      }
    }
  }

  private scheduleDrain(): void {
    if (this.disposed || this.drainScheduled) return;
    this.drainScheduled = true;
    queueMicrotask(() => {
      this.drainScheduled = false;
      this.expirePending();
      if (this.disposed) return;
      for (const lane of [...this.lanes.values()]) this.startEligible(lane);
    });
  }

  private startEligible(lane: WorkerDispatchLane): void {
    while (
      !this.disposed
      && lane.admittedTasks.size < TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER
      && lane.ready.length > 0
    ) {
      const task = lane.ready.shift()!;
      if (!this.isPending(task) || task.state !== "queued") continue;
      task.state = "verifying";
      beginRouteVerification(task);
      void this.verifyAndSend(task);
    }
    this.removeIdleLane(lane);
  }

  private async verifyAndSend(task: TerminalStreamTask): Promise<void> {
    if (!this.isPending(task) || this.expireIfNeeded(task)) return;
    if (this.workerGenerationChanged(task)) {
      this.settler.cancelTask(task, "worker_generation_replaced");
      return;
    }

    try {
      const currentRoute = await this.options.resolveRoute(task.message.sessionId);
      if (!this.isPending(task) || this.expireIfNeeded(task)) return;
      if (!task.route || !sameTerminalStreamRoute(task.route, currentRoute)) {
        this.settler.cancelTask(task, "route_changed");
        return;
      }
      if (this.workerGenerationChanged(task)) {
        this.settler.cancelTask(task, "worker_generation_replaced");
        return;
      }
      if (!task.lane) {
        this.settler.completeUnsent(task, "terminal stream dispatch lane unavailable", false);
        return;
      }
      if (!reserveWorkerAdmissionSlot(
        task,
        TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER,
      )) {
        requeueAfterFullWorkerWindow(task, task.lane!);
        return;
      }

      const { deadline: _deadline, ...state } = task.message;
      let request: TerminalWorkerRequest<WTerminalStreamResult>;
      try {
        request = this.options.sendStream(task.route.workerFp, state, task.message.deadline);
      } catch (error) {
        this.settler.completeUnsent(
          task,
          error instanceof Error ? error.message : "terminal stream transport failed",
          false,
        );
        return;
      }
      if (!request.admitted) {
        this.settler.completeRequest(task, request);
        log.warn("terminal-stream-dispatcher", "transport_not_admitted", {
          session_id: task.message.sessionId,
          stream_id: task.message.streamId,
          worker_fp: task.route.workerFp,
          expired: request.expired,
        });
        return;
      }

      this.settler.promoteInFlight(task, request);
      log.debug("terminal-stream-dispatcher", "transport_admitted", {
        session_id: task.message.sessionId,
        stream_id: task.message.streamId,
        worker_fp: task.route.workerFp,
      });
    } catch (error) {
      if (this.isPending(task)) {
        this.settler.completeUnsent(
          task,
          error instanceof Error ? error.message : "terminal route verification failed",
          false,
        );
      }
    }
  }

  private expireIfNeeded(task: TerminalStreamTask): boolean {
    if (task.message.deadline.remainingMs() > 0) return false;
    this.settler.completeUnsent(task, "terminal stream budget expired before send", true);
    return true;
  }

  private expirePending(): void {
    for (const task of [...this.pendingBySession.values()]) this.expireIfNeeded(task);
    for (const lane of [...this.lanes.values()]) {
      if (!releaseExpiredRetainedSlots(lane)) continue;
      this.removeIdleLane(lane);
      this.scheduleDrain();
    }
    this.armDeadlineTimer();
  }

  private armDeadlineTimer(): void {
    if (this.deadlineTimer !== null) this.clearTimer(this.deadlineTimer);
    this.deadlineTimer = null;
    if (this.disposed) return;
    let shortestRemainingMs = Infinity;
    for (const task of this.pendingBySession.values()) {
      shortestRemainingMs = Math.min(shortestRemainingMs, task.message.deadline.remainingMs());
    }
    for (const lane of this.lanes.values()) {
      shortestRemainingMs = Math.min(shortestRemainingMs, earliestRetainedSlotDeadline(lane));
    }
    if (!Number.isFinite(shortestRemainingMs)) return;
    const timer = this.setTimer(() => {
      if (this.deadlineTimer !== timer) return;
      this.deadlineTimer = null;
      this.expirePending();
      this.scheduleDrain();
    }, Math.max(1, Math.ceil(shortestRemainingMs)));
    this.deadlineTimer = timer;
    timer.unref?.();
  }

  private removeIdleLane(lane: WorkerDispatchLane): void {
    if (lane.admittedTasks.size === 0 && lane.ready.length === 0 && lane.verifying === 0
      && this.lanes.get(lane.workerFp) === lane) this.lanes.delete(lane.workerFp);
  }

  private workerGenerationChanged(task: TerminalStreamTask): boolean {
    return this.options.currentWorker !== undefined
      && (this.options.currentWorker(task.route?.workerFp ?? "") ?? null) !== task.workerGeneration;
  }

  private isPending(task: TerminalStreamTask): boolean {
    return !this.disposed && this.pendingBySession.get(task.message.sessionId) === task;
  }
}
