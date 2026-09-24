// The authenticated terminal-capture bridge: the only path from DiagSnapshot's
// terminal_capture request to a worker capture command. Owns the order of
// operations — validate, resolve the durable session and its worker, check
// lease ownership, admit the capture, freeze coordinator evidence, dispatch.
// Lease/idempotency state lives in terminal-capture-lease.ts, cell records in
// terminal-capture-recorder.ts, the worker call in
// terminal-capture-worker-call.ts. Called by handlers-system.ts::diagSnapshot.

import { diag, signal } from "@roost/observability/diag";
import type { TerminalCaptureRequest } from "@roost/protocol/proto/coordinator_pb";
import {
  checkTerminalCaptureEnvelope,
  TERMINAL_CAPTURE_LIMITS,
  validateTerminalCaptureRequest,
  type TerminalCaptureCommand,
  type TerminalCaptureErrorCode,
  type TerminalCaptureResult,
} from "@roost/protocol/terminal-capture";
import type { AccountDeviceCaller } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";
import {
  armRecording,
  armedRecordingCount,
  armedRecordingForSession,
  authorizeRecording,
  captureFailure,
  captureOwnerKey,
  captureResult,
  createOneShotRecording,
  releaseClosedSessionRecordings,
  releaseRecording,
  sessionCaptureGate,
  _sweepTerminalCaptureRecordings,
  type CaptureRecording,
} from "./terminal-capture-lease.ts";
import { freezeCoordinatorEvidence } from "./terminal-capture-recorder.ts";
import {
  requestTerminalCapture,
  type TerminalCaptureWorkerOutcome,
} from "./terminal-capture-worker-call.ts";

export interface TerminalCaptureBridge {
  handle(
    request: TerminalCaptureRequest,
    principal: AccountDeviceCaller,
  ): Promise<TerminalCaptureResult>;
}

export function createTerminalCaptureBridge(deps: ConnectDeps): TerminalCaptureBridge {
  return {
    async handle(request, principal) {
      const nowMs = Date.now();
      _sweepTerminalCaptureRecordings(nowMs);
      const validation = validateTerminalCaptureRequest({
        action: request.action,
        sessionId: request.sessionId,
        captureId: request.captureId,
        recordingId: request.recordingId,
        reason: request.reason,
        browserEvidenceJson: request.browserEvidenceJson,
      });
      if (!validation.ok) throw captureFailure(validation.code, validation.field);
      const command = validation.command;
      if (command.browser_evidence_json.length !== 0) {
        const envelope = checkTerminalCaptureEnvelope(command.browser_evidence_json, {
          layer: "browser",
          command,
        });
        if (!envelope.ok) throw captureFailure(envelope.code, envelope.field);
      }
      // Every authorization boundary comes from durable rows, before any cache
      // lookup, lease allocation, recorder arming or worker command.
      const workerFp = await resolveOpenSessionWorker(deps, command.session_id);
      const ownerKey = captureOwnerKey(principal);
      const owned = authorizeRecording(command, ownerKey);
      const active = armedRecordingForSession(command.session_id);
      if (active !== null && active.ownerKey !== ownerKey) {
        // Another operator's live recording is never disturbed, whichever
        // recording ID this caller claims.
        throw captureFailure(
          command.action === "stop" ? "permission_denied" : "lease_conflict",
          "recording_id",
        );
      }
      const ownedActive = active?.recordingId === command.recording_id ? active : null;
      if (command.action === "stop") {
        return await stopRecording(command, workerFp, nowMs, ownedActive);
      }
      if (active !== null && ownedActive === null) {
        // Another page of this owner holds the session's lease. A second
        // recording — including an unarmed one-shot — never overrides it.
        throw captureFailure("lease_conflict", "recording_id");
      }
      if (command.action === "start") {
        return await startRecording(deps, command, ownerKey, workerFp, nowMs, ownedActive);
      }
      if (owned !== null && owned.state === "expired") {
        throw captureFailure("lease_expired", "recording_id");
      }
      return await captureIncident(command, ownerKey, workerFp, nowMs, owned, ownedActive);
    },
  };
}

async function startRecording(
  deps: ConnectDeps,
  command: TerminalCaptureCommand,
  ownerKey: string,
  workerFp: string,
  nowMs: number,
  active: CaptureRecording | null,
): Promise<TerminalCaptureResult> {
  const renewed = active !== null;
  if (!renewed && armedRecordingCount() >= TERMINAL_CAPTURE_LIMITS.maxRecordingsPerProcess) {
    await releaseClosedSessionRecordings(deps, nowMs);
    if (armedRecordingCount() >= TERMINAL_CAPTURE_LIMITS.maxRecordingsPerProcess) {
      throw captureFailure("resource_exhausted", "recording_id");
    }
  }
  const recording = armRecording(command, ownerKey, nowMs);
  const outcome = await requestTerminalCapture(workerFp, command, "");
  const rejected = workerRejection(outcome, "recording");
  if (!outcome.ok || rejected !== null) {
    // Roll a fresh arm back so no layer keeps recording without the operator's
    // acknowledgement; an already-acknowledged lease survives a renewal error.
    if (!renewed) releaseRecording(recording);
    signal("terminal.capture_failed", {
      cooldownKey: command.capture_id,
      sid: command.session_id,
      recording_id: command.recording_id,
      capture_id: command.capture_id,
      action: command.action,
      error: rejected,
    });
    return captureResult(command, workerFp, "error", {
      expires_at_ms: renewed ? recording.expiresAtMs : null,
      path: null,
      byte_length: null,
      error: rejected,
      recent_worker_capture: null,
    });
  }
  signal("terminal.capture_started", {
    cooldownKey: command.recording_id,
    sid: command.session_id,
    recording_id: command.recording_id,
    worker_fp: workerFp,
    expires_at_ms: recording.expiresAtMs,
    renewed,
    layers: "coordinator+worker",
  });
  return captureResult(command, workerFp, "recording", {
    expires_at_ms: recording.expiresAtMs,
    path: null,
    byte_length: null,
    error: null,
    recent_worker_capture: outcome.ack.recentWorkerCapture,
  });
}

async function stopRecording(
  command: TerminalCaptureCommand,
  workerFp: string,
  nowMs: number,
  active: CaptureRecording | null,
): Promise<TerminalCaptureResult> {
  if (active !== null) {
    const freed = releaseRecording(active);
    signal("terminal.capture_stopped", {
      cooldownKey: command.recording_id,
      sid: command.session_id,
      recording_id: command.recording_id,
      coordinator_records: freed.records,
      coordinator_bytes: freed.bytes,
      lease_ms: Math.max(0, nowMs - active.startedAtMs),
    });
  }
  // Forwarded even when this coordinator holds no lease: a repeat by the owner
  // is harmless, and it is how a worker recorder is freed after a coord restart.
  const outcome = await requestTerminalCapture(workerFp, command, "");
  const rejected = workerRejection(outcome, "stopped");
  return captureResult(command, workerFp, rejected === null ? "stopped" : "error", {
    expires_at_ms: null,
    path: null,
    byte_length: null,
    error: rejected,
    recent_worker_capture: outcome.ok ? outcome.ack.recentWorkerCapture : null,
  });
}

async function captureIncident(
  command: TerminalCaptureCommand,
  ownerKey: string,
  workerFp: string,
  nowMs: number,
  owned: CaptureRecording | null,
  active: CaptureRecording | null,
): Promise<TerminalCaptureResult> {
  const cached = owned?.completed.get(command.capture_id);
  if (cached !== undefined) {
    // An idempotent retry answers with its original result. Past the worker's
    // retention window the file is gone and is never recreated from later
    // terminal state.
    return nowMs - cached.atMs < TERMINAL_CAPTURE_LIMITS.retentionMs
      ? cached.result
      : captureResult(command, workerFp, "error", {
        expires_at_ms: null,
        path: null,
        byte_length: null,
        error: "capture_expired",
        recent_worker_capture: null,
      });
  }
  const gate = sessionCaptureGate(command.session_id);
  if (gate.inFlightCaptureId !== null) throw captureFailure("capture_in_flight", "capture_id");
  if (nowMs - gate.lastCaptureAtMs < TERMINAL_CAPTURE_LIMITS.manualCooldownMs) {
    throw captureFailure("rate_limited", "capture_id");
  }
  const owner = owned ?? createOneShotRecording(command, ownerKey, nowMs);
  if (owner.completed.size >= TERMINAL_CAPTURE_LIMITS.completedCaptureIds) {
    throw captureFailure("resource_exhausted", "capture_id");
  }
  gate.inFlightCaptureId = command.capture_id;
  gate.lastCaptureAtMs = nowMs;
  try {
    // Frozen before any await: the evidence must describe the incident, not
    // whatever the terminal did while the worker was answering.
    const evidence = freezeCoordinatorEvidence(
      command.session_id,
      command.capture_id,
      command.recording_id,
    );
    const outcome = await requestTerminalCapture(workerFp, command, evidence.json);
    const rejected = workerRejection(outcome, "captured");
    if (!outcome.ok || rejected !== null) {
      signal("terminal.capture_failed", {
        cooldownKey: command.capture_id,
        sid: command.session_id,
        recording_id: command.recording_id,
        capture_id: command.capture_id,
        action: command.action,
        reason: command.reason,
        error: rejected,
        coordinator_records: evidence.records,
        coordinator_available: evidence.available,
      });
      return captureResult(command, workerFp, "error", {
        expires_at_ms: null,
        path: null,
        byte_length: null,
        error: rejected,
        recent_worker_capture: outcome.ok ? outcome.ack.recentWorkerCapture : null,
      });
    }
    const ack = outcome.ack;
    const result = captureResult(command, workerFp, ack.status, {
      expires_at_ms: ack.expiresAtMs ?? active?.expiresAtMs ?? null,
      path: ack.path,
      byte_length: ack.byteLength,
      error: null,
      recent_worker_capture: ack.recentWorkerCapture,
    });
    owner.completed.set(command.capture_id, { result, atMs: nowMs });
    diag("diag.capture", {
      sid: command.session_id,
      recording_id: command.recording_id,
      capture_id: command.capture_id,
      reason: command.reason,
      status: ack.status,
      byte_length: ack.byteLength,
      coordinator_records: evidence.records,
      coordinator_dropped: evidence.dropped,
      coordinator_bytes: evidence.bytes,
      armed: active !== null,
    });
    return result;
  } finally {
    gate.inFlightCaptureId = null;
  }
}

async function resolveOpenSessionWorker(
  deps: ConnectDeps,
  sessionId: string,
): Promise<string> {
  const row = await deps.db.selectFrom("sessions as session")
    .innerJoin("workers as worker", "worker.fp", "session.worker_fp")
    .select("session.worker_fp as worker_fp")
    .where("session.id", "=", sessionId)
    .where("session.status", "=", "open")
    .where("worker.deleted_at_ms", "is", null)
    .executeTakeFirst();
  if (row === undefined) throw captureFailure("session_unknown", "session_id");
  return row.worker_fp;
}

/** The fixed error code for a worker answer that did not acknowledge this
 *  action, or null when it did. A partial capture IS an acknowledgement: the
 *  bundle exists, so the browser must not re-send its frozen evidence. */
function workerRejection(
  outcome: TerminalCaptureWorkerOutcome,
  acknowledged: "recording" | "captured" | "stopped",
): TerminalCaptureErrorCode | null {
  if (!outcome.ok) return outcome.code;
  if (outcome.ack.error !== null) return outcome.ack.error;
  if (outcome.ack.status === acknowledged) return null;
  return acknowledged === "captured" && outcome.ack.status === "partial"
    ? null
    : "worker_failed";
}
