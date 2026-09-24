// Process-wide terminal-capture lease registry: who owns a recording, when its
// server-time lease expires, the per-session capture admission gate, and the
// bounded completed-capture cache that makes one CAPTURE idempotent. Also owns
// the result and fixed-error vocabulary the bridge answers with.
// Used only by terminal-capture.ts; arms and disarms the coordinator records in
// terminal-capture-recorder.ts so no lease transition can leave one behind.

import { Code, ConnectError } from "@connectrpc/connect";
import { signal } from "@roost/observability/diag";
import {
  TERMINAL_CAPTURE_LIMITS,
  type TerminalCaptureCommand,
  type TerminalCaptureErrorCode,
  type TerminalCaptureFileRef,
  type TerminalCaptureResult,
  type TerminalCaptureStatus,
} from "@roost/protocol/terminal-capture";
import type { AccountDeviceCaller } from "../../auth/auth-interceptor.ts";
import type { ConnectDeps } from "../../rpc/router.ts";
import {
  armCoordinatorRecorder,
  disarmCoordinatorRecorder,
} from "./terminal-capture-recorder.ts";

interface CompletedCapture {
  readonly result: TerminalCaptureResult;
  readonly atMs: number;
}

export interface CaptureRecording {
  readonly recordingId: string;
  readonly sessionId: string;
  /** Derived from the authenticated principal, never from a request body. */
  readonly ownerKey: string;
  state: "armed" | "expired" | "one_shot";
  readonly startedAtMs: number;
  expiresAtMs: number;
  retainUntilMs: number;
  readonly completed: Map<string, CompletedCapture>;
  timer: NodeJS.Timeout | undefined;
}

/** One outstanding capture and one manual cooldown per SESSION, so two pages of
 *  one operator cannot double-dispatch against the same terminal. */
export interface SessionCaptureGate {
  inFlightCaptureId: string | null;
  lastCaptureAtMs: number;
}

export interface ReleasedRecords {
  readonly records: number;
  readonly bytes: number;
}

const recordings = new Map<string, CaptureRecording>();
const armedRecordingIdBySession = new Map<string, string>();
const gates = new Map<string, SessionCaptureGate>();

const CONNECT_CODES: Record<TerminalCaptureErrorCode, Code> = {
  invalid_argument: Code.InvalidArgument,
  evidence_too_large: Code.InvalidArgument,
  evidence_malformed: Code.InvalidArgument,
  permission_denied: Code.PermissionDenied,
  session_unknown: Code.NotFound,
  lease_conflict: Code.AlreadyExists,
  lease_expired: Code.FailedPrecondition,
  lease_absent: Code.FailedPrecondition,
  capture_expired: Code.FailedPrecondition,
  capture_in_flight: Code.Aborted,
  rate_limited: Code.ResourceExhausted,
  resource_exhausted: Code.ResourceExhausted,
  worker_offline: Code.Unavailable,
  worker_timeout: Code.DeadlineExceeded,
  worker_failed: Code.Internal,
  storage_failed: Code.Internal,
  internal: Code.Internal,
};

export function captureOwnerKey(principal: AccountDeviceCaller): string {
  return principal.kind === "account-device"
    ? `account-device:${principal.accountId}:${principal.fingerprint}`
    : `legacy-self-hosted:${principal.fingerprint}`;
}

/** The session's acknowledged lease, or null when nothing is armed for it. */
export function armedRecordingForSession(sessionId: string): CaptureRecording | null {
  const recordingId = armedRecordingIdBySession.get(sessionId);
  if (recordingId === undefined) return null;
  const recording = recordings.get(recordingId);
  return recording?.state === "armed" ? recording : null;
}

export function recordingById(recordingId: string): CaptureRecording | null {
  return recordings.get(recordingId) ?? null;
}

export function armedRecordingCount(): number {
  return armedRecordingIdBySession.size;
}

/** A recording ID is bound to ONE session and ONE authenticated owner for its
 *  whole retained life: reusing it elsewhere would disarm another session's
 *  records and spend a second per-process slot on one recording. */
export function authorizeRecording(
  command: TerminalCaptureCommand,
  ownerKey: string,
): CaptureRecording | null {
  const recording = recordings.get(command.recording_id);
  if (recording === undefined) return null;
  if (recording.ownerKey !== ownerKey) throw captureFailure("permission_denied", "recording_id");
  if (recording.sessionId !== command.session_id) {
    throw captureFailure("lease_conflict", "session_id");
  }
  return recording;
}

/** Creates or renews one lease and arms the coordinator records with it. A
 *  renewal keeps the evidence the operator armed the lease to collect. */
export function armRecording(
  command: TerminalCaptureCommand,
  ownerKey: string,
  nowMs: number,
): CaptureRecording {
  const existing = recordings.get(command.recording_id);
  const recording: CaptureRecording = existing ?? {
    recordingId: command.recording_id,
    sessionId: command.session_id,
    ownerKey,
    state: "armed",
    startedAtMs: nowMs,
    expiresAtMs: nowMs,
    retainUntilMs: nowMs,
    completed: new Map(),
    timer: undefined,
  };
  recording.state = "armed";
  recording.expiresAtMs = nowMs + TERMINAL_CAPTURE_LIMITS.leaseMs;
  recording.retainUntilMs = recording.expiresAtMs;
  recordings.set(recording.recordingId, recording);
  armedRecordingIdBySession.set(recording.sessionId, recording.recordingId);
  clearTimeout(recording.timer);
  const timer = setTimeout(
    () => _sweepTerminalCaptureRecordings(Date.now()),
    Math.max(1, recording.expiresAtMs - nowMs) + 1_000,
  );
  timer.unref?.();
  recording.timer = timer;
  armCoordinatorRecorder(recording.sessionId, recording.recordingId, nowMs);
  return recording;
}

/** An unarmed manual capture: no lease, no renewal, and its idempotent result
 *  is retained with the other completed-capture records. */
export function createOneShotRecording(
  command: TerminalCaptureCommand,
  ownerKey: string,
  nowMs: number,
): CaptureRecording {
  evictRetainedRecordings(1);
  const recording: CaptureRecording = {
    recordingId: command.recording_id,
    sessionId: command.session_id,
    ownerKey,
    state: "one_shot",
    startedAtMs: nowMs,
    expiresAtMs: nowMs,
    retainUntilMs: nowMs + TERMINAL_CAPTURE_LIMITS.retentionMs,
    completed: new Map(),
    timer: undefined,
  };
  recordings.set(recording.recordingId, recording);
  return recording;
}

export function sessionCaptureGate(sessionId: string): SessionCaptureGate {
  const existing = gates.get(sessionId);
  if (existing !== undefined) return existing;
  const gate: SessionCaptureGate = { inFlightCaptureId: null, lastCaptureAtMs: 0 };
  gates.set(sessionId, gate);
  return gate;
}

/** Drops the lease and frees the coordinator records. Saved incident files
 *  belong to the worker and are never touched here. */
export function releaseRecording(recording: CaptureRecording): ReleasedRecords {
  clearTimeout(recording.timer);
  recording.timer = undefined;
  if (armedRecordingIdBySession.get(recording.sessionId) === recording.recordingId) {
    armedRecordingIdBySession.delete(recording.sessionId);
  }
  recordings.delete(recording.recordingId);
  // A stop that races a capture still leaves that capture outstanding, so its
  // one-at-a-time fence survives the release and the sweep reaps it later.
  if (gates.get(recording.sessionId)?.inFlightCaptureId === null) {
    gates.delete(recording.sessionId);
  }
  return disarmCoordinatorRecorder(recording.sessionId);
}

/** An expired lease keeps its recording ID so a late CAPTURE is told the lease
 *  expired instead of quietly becoming an unarmed one-shot. */
export function expireRecording(
  recording: CaptureRecording,
  nowMs: number,
  reason: "lease_expired" | "session_closed",
): void {
  const freed = releaseRecording(recording);
  recording.state = "expired";
  recording.retainUntilMs = nowMs + TERMINAL_CAPTURE_LIMITS.retentionMs;
  recordings.set(recording.recordingId, recording);
  signal("terminal.capture_expired", {
    cooldownKey: recording.recordingId,
    sid: recording.sessionId,
    recording_id: recording.recordingId,
    reason,
    coordinator_records: freed.records,
    coordinator_bytes: freed.bytes,
  });
}

/** A lease whose session is gone is not an active recording. Reclaiming it is
 *  what keeps a closed terminal from parking a slot for the whole lease window;
 *  an operator's live recording is never evicted to make room. */
export async function releaseClosedSessionRecordings(
  deps: ConnectDeps,
  nowMs: number,
): Promise<void> {
  const sessionIds = [...armedRecordingIdBySession.keys()];
  if (sessionIds.length === 0) return;
  const openRows = await deps.db.selectFrom("sessions")
    .select("id")
    .where("id", "in", sessionIds)
    .where("status", "=", "open")
    .execute();
  const openSessionIds = new Set(openRows.map((row) => row.id));
  for (const sessionId of sessionIds) {
    if (openSessionIds.has(sessionId)) continue;
    const recording = armedRecordingForSession(sessionId);
    if (recording !== null) expireRecording(recording, nowMs, "session_closed");
  }
}

export function captureResult(
  command: TerminalCaptureCommand,
  workerFp: string,
  status: TerminalCaptureStatus,
  fields: {
    readonly expires_at_ms: number | null;
    readonly path: string | null;
    readonly byte_length: number | null;
    readonly error: TerminalCaptureErrorCode | null;
    readonly recent_worker_capture: TerminalCaptureFileRef | null;
  },
): TerminalCaptureResult {
  return {
    capture_id: command.capture_id,
    recording_id: command.recording_id,
    session_id: command.session_id,
    action: command.action,
    status,
    expires_at_ms: fields.expires_at_ms,
    worker_fp: workerFp,
    path: fields.path,
    byte_length: fields.byte_length,
    error: fields.error,
    recent_worker_capture: fields.recent_worker_capture,
  };
}

/** `<fixed error code>: <field path>` — the browser maps the code back to a
 *  TerminalCaptureErrorCode. Never a validator or parser message: those can
 *  quote the terminal text being validated. */
export function captureFailure(
  code: TerminalCaptureErrorCode,
  field: string,
): ConnectError {
  return new ConnectError(`${code}: ${field}`, CONNECT_CODES[code]);
}

/** Server-time expiry sweep. Carries the test/diagnostic marker because the
 *  focused tests drive it at an arbitrary instant instead of waiting out a
 *  thirty-minute lease. */
export function _sweepTerminalCaptureRecordings(nowMs: number): void {
  for (const recording of [...recordings.values()]) {
    if (recording.state === "armed" && nowMs >= recording.expiresAtMs) {
      expireRecording(recording, nowMs, "lease_expired");
      continue;
    }
    if (recording.state !== "armed" && nowMs >= recording.retainUntilMs) {
      recordings.delete(recording.recordingId);
    }
  }
  for (const [sessionId, gate] of gates) {
    if (
      gate.inFlightCaptureId === null
      && !armedRecordingIdBySession.has(sessionId)
      && nowMs - gate.lastCaptureAtMs >= TERMINAL_CAPTURE_LIMITS.manualCooldownMs
    ) gates.delete(sessionId);
  }
  evictRetainedRecordings(0);
}

export function _resetTerminalCaptureLeases(): void {
  for (const recording of recordings.values()) {
    clearTimeout(recording.timer);
    disarmCoordinatorRecorder(recording.sessionId);
  }
  recordings.clear();
  armedRecordingIdBySession.clear();
  gates.clear();
}

/** Retained completed-capture records are bounded process-wide, oldest first.
 *  An armed lease is never evicted to make room for one. */
function evictRetainedRecordings(headroom: number): void {
  const retained = [...recordings.values()]
    .filter((recording) => recording.state !== "armed")
    .sort((left, right) => left.startedAtMs - right.startedAtMs);
  const excess = retained.length + headroom - TERMINAL_CAPTURE_LIMITS.completedCaptureIds;
  for (let idx = 0; idx < excess; idx++) recordings.delete(retained[idx]!.recordingId);
}
