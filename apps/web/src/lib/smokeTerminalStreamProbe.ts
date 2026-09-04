// A terminal failure can originate in the browser, coordinator, or routed worker.
// This probe joins all three snapshots into one stable smoke-facing diagnostic shape.
// The smoke backdoor invokes it on demand so normal terminal traffic carries no extra work.
// Missing and malformed layers stay explicit instead of being mistaken for healthy state.

import { coordClient } from "../connect.ts";
import type { SmokeApi, TerminalStreamProbe } from "./smokeTypes.ts";
import {
  terminalBrowserStreamSnapshot,
  type TerminalBrowserStreamSnapshot,
} from "./terminalDiagSnapshot.ts";

type SmokeTerminalStreamProbeMethods = Pick<SmokeApi, "terminalStreamProbe">;

export function createSmokeTerminalStreamProbeMethods(): SmokeTerminalStreamProbeMethods {
  return {
    async terminalStreamProbe(sessionId) {
      const browser = terminalBrowserStreamSnapshot(sessionId);
      const response = await coordClient.diagSnapshot({
        spaStateJson: JSON.stringify(browser),
      });
      let decoded: unknown;
      try {
        decoded = JSON.parse(response.snapshotJson);
      } catch (error) {
        throw new Error(`coordinator diagnostic snapshot was invalid JSON: ${String(error)}`);
      }
      return normalizeTerminalStreamProbe(sessionId, browser, decoded);
    },
  };
}

function diagnosticRecord(value: unknown): Record<string, unknown> | null {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return null;
  return value as Record<string, unknown>;
}

function diagnosticBuild(
  value: unknown,
): { git_sha: string | null; artifact_version: string | null } | null {
  const record = diagnosticRecord(value);
  if (!record) return null;
  return {
    git_sha: typeof record.git_sha === "string" ? record.git_sha : null,
    artifact_version: typeof record.artifact_version === "string"
      ? record.artifact_version
      : null,
  };
}

function normalizeTerminalStreamProbe(
  sessionId: string,
  browser: TerminalBrowserStreamSnapshot,
  rawSnapshot: unknown,
): TerminalStreamProbe {
  const root = diagnosticRecord(rawSnapshot);
  if (!root) throw new Error("coordinator diagnostic snapshot was not an object");
  const coordRecord = diagnosticRecord(root.coord);
  const coordSessions = diagnosticRecord(coordRecord?.sessions);
  const coordSession = diagnosticRecord(coordSessions?.[sessionId]);
  const terminalView = diagnosticRecord(coordSession?.terminal_view);
  const terminalEffective = diagnosticRecord(terminalView?.effective);
  const terminalControl = terminalView
    ? {
        active_view_count: terminalView.activeViews,
        parked_view_count: terminalView.parkedViews,
        stream_id: terminalView.streamId,
        unavailable: terminalView.unavailable,
        effective_cols: terminalEffective?.cols ?? null,
        effective_rows: terminalEffective?.rows ?? null,
      }
    : null;
  const route = diagnosticRecord(coordSession?.route);
  const workerFp = typeof route?.worker_fp === "string" ? route.worker_fp : null;
  const workers = diagnosticRecord(root.workers);
  const workerEnvelope = workerFp ? diagnosticRecord(workers?.[workerFp]) : null;
  const workerStatus = workerEnvelope?.status;
  const responseMs = typeof workerEnvelope?.response_ms === "number"
    && Number.isFinite(workerEnvelope.response_ms)
    ? workerEnvelope.response_ms
    : null;
  const workerSnapshot = workerStatus === "ok"
    ? diagnosticRecord(workerEnvelope?.snapshot)
    : null;
  const workerSessions = diagnosticRecord(workerSnapshot?.sessions);
  const workerSession = diagnosticRecord(workerSessions?.[sessionId]);
  const workerError = diagnosticRecord(workerEnvelope?.error);
  const capturedAtMs = typeof root.captured_at_ms === "number"
    && Number.isFinite(root.captured_at_ms)
    ? root.captured_at_ms
    : browser.captured_at_ms;

  return {
    captured_at_ms: capturedAtMs,
    session_id: sessionId,
    browser,
    coord: coordRecord
      ? {
          build: diagnosticBuild(coordRecord.build) ?? {
            git_sha: null,
            artifact_version: null,
          },
          session: coordSession,
          terminal_control: terminalControl,
        }
      : null,
    worker: {
      worker_fp: workerFp,
      status: workerStatus === "ok" || workerStatus === "error"
        ? workerStatus
        : "missing",
      response_ms: responseMs,
      build: diagnosticBuild(workerSnapshot?.build),
      session: workerSession,
      error: workerStatus === "error"
        ? {
            code: typeof workerError?.code === "string" ? workerError.code : null,
            message: typeof workerError?.message === "string" ? workerError.message : null,
          }
        : null,
    },
  };
}
