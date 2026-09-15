// Per-session slice of the coord diag snapshot: the cached-or-durable worker
// route, the terminal-view aggregate, the screen watermark, and the per-view
// geometry inputs the SCD minimized over. handlers-system.ts's diagSnapshot
// assembles the session map from this; it reads the terminal-view hub
// accessors and the byte-hub route cache, and owns no state of its own.

import type { TerminalGeometry } from "@roost/shared/viewport";
import type { TerminalViewInput } from "@roost/shared/terminal-view";
import {
  currentTerminalScreenHub,
  terminalViewInputs,
  terminalViewSnapshot,
} from "./terminal-view-hub.ts";
import { getCachedSessionWorker } from "../byte-hub.ts";

export interface DiagSessionRow {
  id: string;
  worker_fp: string;
  channel: number;
}

/** Durable admission the caller already resolved: which workers this caller may
 *  see at all, and which of those can currently be dispatched to. Volatile
 *  registry state is never allowed to widen it. */
export interface DiagSessionScope {
  allowedWorkerFps: ReadonlySet<string>;
  dispatchableWorkerFps: ReadonlySet<string>;
}

export interface CoordSessionRouteDiagnostic {
  worker_fp: string;
  channel_id: number;
  connected: boolean;
  source: "live_cache" | "database";
}

export interface CoordSessionTerminalViewDiagnostic {
  activeViews: number;
  parkedViews: number;
  streamId: string;
  effective: TerminalGeometry | null;
  unavailable: boolean;
}

export interface CoordSessionScreenDiagnostic {
  stream_id: string;
  grid_epoch: string;
  seq: string;
  cols: number;
  rows: number;
  valid: boolean;
}

export interface CoordSessionDiagnostic {
  route: CoordSessionRouteDiagnostic | null;
  terminal_view: CoordSessionTerminalViewDiagnostic | null;
  terminal_screen: CoordSessionScreenDiagnostic | null;
  /** One entry per watching view, NOT per device: two panes on one device are
   *  two inputs, and `constrains` names the ones the effective size is the
   *  minimum of. Without this an operator sees a narrow session and cannot
   *  tell which record is pinning it. */
  viewers: readonly TerminalViewInput[];
}

export function coordSessionDiagnostic(
  row: DiagSessionRow,
  scope: DiagSessionScope,
): CoordSessionDiagnostic {
  const cachedRoute = getCachedSessionWorker(row.id);
  const screen = currentTerminalScreenHub()?.snapshot(row.id);
  return {
    route: cachedRoute && scope.allowedWorkerFps.has(cachedRoute.worker_fp)
      ? {
        worker_fp: cachedRoute.worker_fp,
        channel_id: cachedRoute.channel,
        connected: scope.dispatchableWorkerFps.has(cachedRoute.worker_fp),
        source: "live_cache",
      }
      : scope.allowedWorkerFps.has(row.worker_fp)
        ? {
          worker_fp: row.worker_fp,
          channel_id: row.channel,
          connected: scope.dispatchableWorkerFps.has(row.worker_fp),
          source: "database",
        }
        : null,
    terminal_view: terminalViewSnapshot(row.id),
    terminal_screen: screen
      ? {
        stream_id: screen.streamId,
        grid_epoch: screen.gridEpoch,
        seq: screen.seq.toString(),
        cols: screen.cols,
        rows: screen.rows,
        valid: screen.valid,
      }
      : null,
    viewers: terminalViewInputs(row.id),
  };
}
