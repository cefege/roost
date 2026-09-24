// Public contracts for per-session terminal stream reconciliation.
// TerminalViewStreamController owns the mutable session state; TerminalViewHub
// supplies geometry, route, and browser callbacks. The dispatcher consumes the
// route shape to verify the same worker route immediately before a send.
import type { TerminalViewStatus } from "@roost/protocol/proto/sync_pb";
import type { TerminalViewGeometrySet } from "@roost/protocol/terminal-view";
import type { HopDeadline } from "../../workers/worker-send.ts";
import type { TerminalStreamDispatcher } from "../screen/terminal-stream-dispatcher.ts";
import type { TerminalScreenCaps } from "../screen/terminal-screen-budget.ts";

export interface TerminalStreamDesired {
  streamId: string;
  enabled: boolean;
  cols: number;
  rows: number;
  retry: number;
}

export interface TerminalStreamRoute {
  workerFp: string;
  channel: number;
}

export interface TerminalViewStreamControllerOptions {
  resolveRoute(sessionId: string): Promise<TerminalStreamRoute | null>;
  streamDispatcher: TerminalStreamDispatcher;
  /** Production captures the exact routable WorkerHandle, so a different token
   * is a different worker connection generation. Injected send seams may omit
   * it, and then no generation is observable at all. */
  currentWorker?(workerFp: string): unknown | null;
  createStreamDeadline?(): HopDeadline;
  sendSnapshot(workerFp: string, sessionId: string, streamId: string): boolean;
  geometries(sessionId: string): TerminalViewGeometrySet;
  broadcast(sessionId: string, status: TerminalViewStatus, message: string): void;
  closeViews(sessionId: string): void;
  presence(sessionId: string): void | Promise<void>;
  /** Baseline repair for a session this controller never minimized, i.e. one
   * whose worker owns its own terminal views. */
  repairUnownedSession?(sessionId: string, streamId: string): void;
  /** Budget-derived replica residency ceilings, forwarded to TerminalScreenHub. */
  terminalScreen?: TerminalScreenCaps;
}
