// Public contracts for per-session terminal stream reconciliation.
// TerminalViewStreamController owns the mutable session state; TerminalViewHub
// supplies geometry, route, and browser callbacks. The dispatcher consumes the
// route shape to verify the same worker route immediately before a send.
import type { TerminalViewStatus } from "@roost/shared/proto/sync_pb";
import type { TerminalGeometry } from "@roost/shared/viewport";
import type { HopDeadline } from "./worker-send.ts";
import type { TerminalStreamDispatcher } from "./terminal-stream-dispatcher.ts";

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

export type TerminalUnavailablePolicy = "heartbeat" | "route" | "never";

export interface TerminalStreamState {
  effective: TerminalGeometry | null;
  streamId: string;
  unavailable: boolean;
  unavailableReason: string;
  unavailablePolicy: TerminalUnavailablePolicy;
}

export interface TerminalViewStreamControllerOptions {
  resolveRoute(sessionId: string): Promise<TerminalStreamRoute | null>;
  streamDispatcher: TerminalStreamDispatcher;
  createStreamDeadline?(): HopDeadline;
  sendSnapshot(workerFp: string, sessionId: string, streamId: string): boolean;
  geometries(sessionId: string): readonly TerminalGeometry[];
  broadcast(sessionId: string, status: TerminalViewStatus, message: string): void;
  closeViews(sessionId: string): void;
  presence(sessionId: string): void | Promise<void>;
}
