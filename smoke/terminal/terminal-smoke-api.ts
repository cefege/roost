import type { TerminalStreamProbe } from "../../apps/web/src/lib/smoke.ts";
import type {
  PaintedCursorProof,
  PaintedMarkerProof,
} from "../../apps/web/src/lib/smokeHarness.ts";

export interface RecoveryMarkerScan {
  total: number; unique: number; min: number; max: number;
  duplicated: number[]; missing: number; outOfOrder: number; firstInversion: number;
}

export interface TerminalInputCapture {
  batches: Array<{ sessionId: string; data: number[] }>;
  droppedBatches: number;
}

interface SmokeSessionProjection {
  id: string;
  worker_fp: string;
  cwd?: string;
  spawn_cwd?: string;
}

export interface RecoverySmokeApi {
  spawnShell(workerFp: string, folder: string, sessionId?: string): Promise<{ session_id: string; channel_id: number }>;
  state(): {
    sessions: Record<string, SmokeSessionProjection>;
    workers: Record<string, unknown>;
  };
  createWorkspace(workerFp: string, folder: string, sessionId: string): Promise<{ id: string; channel: number }>;
  navigate(href: string): void;
  input(sessionId: string, text: string): Promise<void>;
  paneFocused(sessionId: string): { hasSlot: boolean; hasTextarea: boolean; focused: boolean };
  terminalInputCapture(): TerminalInputCapture;
  resetTerminalInputCapture(): void;
  dropNextCellFrame(sessionId: string): void;
  droppedCellFrameCount(sessionId: string): number;
  /** Fire the mount-repair callback a mounted pane registered with
   *  registerCellHandler, without needing its mount buffer to overflow. False
   *  when the smoke pin is off or no pane has registered for this session. */
  requestCellMountRepair(sessionId: string): boolean;
  cellFrameCount(sessionId: string): number;
  cellFullFrameCount(sessionId: string): number;
  cellGridEpoch(sessionId: string): string;
  lastFullFrameSbRows(sessionId: string): number;
  scrollbackBackfillRequestCount(sessionId: string): number;
  syncWsGeneration(): number;
  pauseSyncTransport(): void;
  resumeSyncTransport(): void;
  forceSyncMaxBackoff(): void;
  syncRedialStatus(): {
    failures: number;
    nextDelayMs: number;
    hiddenParked: boolean;
    liveness: "none" | "dialing" | "open";
  };
  forceHidden(on: boolean): void;
  forceVisible(on: boolean): void;
  viewportText(sessionId: string): string;
  markerScan(sessionId: string, prefix: string): RecoveryMarkerScan;
  renderProbe(sessionId: string): { atBottom: boolean };
  terminalStreamProbe(sessionId: string): Promise<TerminalStreamProbe>;
  waitForPaintedMarker(sessionId: string, marker: string, timeoutMs?: number): Promise<PaintedMarkerProof>;
  waitForPaintedCursor(
    sessionId: string,
    expected?: { row?: number; column?: number },
    timeoutMs?: number,
  ): Promise<PaintedCursorProof>;
  rejectNextViewportClaim(sessionId: string): void;
  rejectedViewportClaimCount(sessionId: string): number;
}

export interface TerminalIdentityProbeWindow {
  __smoke: RecoverySmokeApi;
  __terminalIdentityProbe: { slot: Element; grid: Element; textarea: Element };
}

export interface RecoveryProbeResult {
  canary: string | null;
  scan: RecoveryMarkerScan;
  atBottom: boolean;
}

export type PaintAttempt<T> =
  | { proof: T; error: null }
  | { proof: null; error: string };

export interface ImmediateTerminalPaintSample {
  eventType: string;
  trusted: boolean;
  selectionCollapsed: boolean;
  cursorRow: number | null;
  cursorColumn: number | null;
  cursorRect: { left: number; top: number; right: number; bottom: number } | null;
  markerRowRect: { left: number; top: number; right: number; bottom: number } | null;
  composerHeight: number | null;
  cursorRowIdentity: boolean | null;
}
