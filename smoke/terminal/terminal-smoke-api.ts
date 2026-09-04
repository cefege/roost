// Shared browser-smoke contracts for terminal recovery and presentation probes.
// Playwright specs consume the production SmokeApi type through this type-only leaf.
// Probe result aliases keep test assertions aligned with the browser producer.

import type {
  SmokeApi,
  SmokeMarkerScan,
  SmokePaintedScrollbackProbe,
  SmokeTerminalInputCapture,
  TerminalStreamProbe,
} from "../../apps/web/src/lib/smokeTypes.ts";
import type {
  PaintedCursorProof,
  PaintedMarkerProof,
} from "../../apps/web/src/lib/smokeHarness.ts";

export type RecoveryMarkerScan = SmokeMarkerScan;

export type TerminalInputCapture = SmokeTerminalInputCapture;

export type PaintedScrollbackProbe = SmokePaintedScrollbackProbe;

export type RecoverySmokeApi = SmokeApi;

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
