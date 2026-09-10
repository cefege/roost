// Public contracts for coordinator terminal stream-state dispatch. TerminalViewHub
// wires the production worker sender and route resolver; tests can inject both
// without admitting terminal input or snapshot traffic into this scheduler.
import type { WTerminalStreamResult } from "@roost/shared/proto/worker_transport_pb";
import type { HopDeadline, TerminalWorkerRequest } from "./worker-send.ts";
import type { TerminalStreamRoute } from "./terminal-view-stream-controller-types.ts";

export const TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER = 32;

export interface TerminalStreamDispatchState {
  sessionId: string;
  streamId: string;
  enabled: boolean;
  cols: number;
  rows: number;
  /** Started when the controller first desired this exact stream generation. */
  deadline: HopDeadline;
}

export type TerminalStreamDispatchCancellation =
  | "dispatcher_disposed"
  | "superseded"
  | "session_closed"
  | "worker_generation_replaced"
  | "route_changed";

export type TerminalStreamDispatchCompletion =
  | {
      kind: "request";
      request: TerminalWorkerRequest<WTerminalStreamResult>;
      /** Reads true if this admitted request's worker generation was replaced before it settled. */
      workerGenerationReplaced?(): boolean;
    }
  | { kind: "cancelled"; reason: TerminalStreamDispatchCancellation };

export interface TerminalStreamDispatchRequest {
  /** True only when the dispatcher retained this desired state locally. */
  accepted: boolean;
  /** Socket admission is available only in this later completion. */
  completion: Promise<TerminalStreamDispatchCompletion>;
}

export type TerminalStreamDispatcherTimer = NodeJS.Timeout;

export type SetTerminalStreamDispatcherTimer = (
  callback: () => void,
  delayMs: number,
) => TerminalStreamDispatcherTimer;

export type ClearTerminalStreamDispatcherTimer = (
  timer: TerminalStreamDispatcherTimer,
) => void;

export interface TerminalStreamDispatcherOptions {
  resolveRoute(sessionId: string): Promise<TerminalStreamRoute | null>;
  sendStream(
    workerFp: string,
    state: Omit<TerminalStreamDispatchState, "deadline">,
    deadline: HopDeadline,
  ): TerminalWorkerRequest<WTerminalStreamResult>;
  /** Production captures the exact routable WorkerHandle; injected send seams may omit it. */
  currentWorker?(workerFp: string): unknown | null;
  setTimer?: SetTerminalStreamDispatcherTimer;
  clearTimer?: ClearTerminalStreamDispatcherTimer;
}
