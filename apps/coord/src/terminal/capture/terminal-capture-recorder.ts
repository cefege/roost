// Bounded coordinator-side cell records for opt-in terminal incident capture.
// Armed and released only by terminal-capture.ts (the authenticated bridge);
// fed by two narrow hooks at TerminalScreenHub's accepted full/delta fold
// boundary; frozen into the bundle's `coordinator` section before the bridge
// dispatches CAPTURE to a worker.
// Bundle shapes and every bound come from @roost/protocol/terminal-capture.

import { randomUUID } from "node:crypto";
import type { CellGridFrame } from "@roost/protocol/cell";
import { ROOST_ARTIFACT_VERSION } from "@roost/host/build-identity";
import {
  TERMINAL_CAPTURE_LIMITS,
  TERMINAL_INCIDENT_SCHEMA,
  utf8ByteLength,
  type TerminalCaptureCoordinatorPayload,
  type TerminalCaptureDropCounters,
  type TerminalCaptureOmission,
  type TerminalCoordinatorRecord,
  type TerminalCoordinatorSection,
} from "@roost/protocol/terminal-capture";
import { COORD_GIT_SHA } from "../../git-sha.ts";

/** Just enough of the admitted wire frame to name what the hub accepted. The
 *  proto frame (bigint sequences) and the canonical frame (numbers) both
 *  satisfy it, so a hook site passes existing locals instead of building an
 *  argument object on a path that is unarmed almost always. */
export interface AdmittedFrameIdentity {
  readonly full: boolean;
  readonly seq: number | bigint;
  readonly baseSeq: number | bigint;
}

export interface CoordinatorEvidence {
  /** A TerminalCaptureCoordinatorPayload — the capture envelope with the
   *  section nested under `coordinator` — or "" when unavailable. */
  readonly json: string;
  readonly records: number;
  readonly dropped: number;
  readonly bytes: number;
  readonly available: boolean;
}

interface RecordedFrame {
  readonly record: TerminalCoordinatorRecord;
  readonly bytes: number;
}

interface SequenceRange {
  start: string;
  end: string;
}

interface ArmedSession {
  readonly recordingId: string;
  readonly armedAtMs: number;
  readonly frames: RecordedFrame[];
  bytes: number;
  droppedRecords: number;
  droppedBytes: number;
  droppedRange: SequenceRange | null;
  overBudgetRecords: number;
  overBudgetRows: number;
}

// Distinguishes a coordinator restart in a bundle whose records otherwise look
// continuous. Minted at module load, never derived from a request.
const COORDINATOR_PROCESS_ID = randomUUID();

const RECORD_JSON_OVERHEAD = 320;
const FRAME_JSON_OVERHEAD = 256;
const ROW_JSON_OVERHEAD = 24;
const SPAN_JSON_OVERHEAD = 56;
// Room for the section header, its process identity and its omission list, so
// the record selection below cannot need a second pass after an omission that
// the first pass itself produced.
const SECTION_HEADER_RESERVE = 2_048;

const armedSessions = new Map<string, ArmedSession>();

/** Hot-path guard for the hub hooks and the bridge. */
export function coordinatorRecorderArmed(sessionId: string): boolean {
  return armedSessions.has(sessionId);
}

/** Idempotent for the same recording: a renewal must never clear evidence the
 *  operator armed the lease to collect. A different recording ID belongs to a
 *  different page and starts empty. */
export function armCoordinatorRecorder(
  sessionId: string,
  recordingId: string,
  atMs: number,
): void {
  if (armedSessions.get(sessionId)?.recordingId === recordingId) return;
  armedSessions.set(sessionId, {
    recordingId,
    armedAtMs: atMs,
    frames: [],
    bytes: 0,
    droppedRecords: 0,
    droppedBytes: 0,
    droppedRange: null,
    overBudgetRecords: 0,
    overBudgetRows: 0,
  });
}

/** Frees every retained record. Saved bundles are the worker's to retain. */
export function disarmCoordinatorRecorder(
  sessionId: string,
): { records: number; bytes: number } {
  const session = armedSessions.get(sessionId);
  if (!session) return { records: 0, bytes: 0 };
  armedSessions.delete(sessionId);
  return { records: session.frames.length, bytes: session.bytes };
}

/** TerminalScreenHub hook, called once per ACCEPTED full or folded delta with
 *  the canonical the hub just installed. `canonical` is retained by reference:
 *  the hub folds into a clone, so an installed canonical is never mutated in
 *  place and copying it here would buy nothing. */
export function recordCoordinatorFrame(
  sessionId: string,
  canonical: CellGridFrame,
  admitted: AdmittedFrameIdentity,
  watchers: ReadonlyMap<string, ReadonlySet<string>>,
): void {
  const session = armedSessions.get(sessionId);
  if (!session) return;
  const previous = session.frames[session.frames.length - 1]?.record ?? null;
  const frameBytes = estimateFrameJsonBytes(canonical);
  const overBudget = frameBytes > TERMINAL_CAPTURE_LIMITS.coordinatorEvidenceBytes;
  const gap = sequenceGap(previous, canonical, admitted);
  const record: TerminalCoordinatorRecord = {
    at_ms: Date.now(),
    stream: {
      stream_id: canonical.streamId,
      grid_epoch: canonical.gridEpoch,
      seq: String(canonical.seq),
      base_seq: admitted.full ? null : String(admitted.baseSeq),
      cols: canonical.cols,
      rows: canonical.rows,
    },
    admitted_full: admitted.full,
    accepted: true,
    canonical: overBudget ? null : canonical,
    snapshot_state: "installed",
    send_state: (watchers.get(sessionId)?.size ?? 0) === 0 ? "not_sent" : "queued",
    gap,
    repair: gap === null ? "none" : "requested_full",
  };
  if (overBudget) {
    session.overBudgetRecords++;
    session.overBudgetRows += canonical.viewportRows.length;
  }
  const bytes = RECORD_JSON_OVERHEAD + (overBudget ? 0 : frameBytes);
  session.frames.push({ record, bytes });
  session.bytes += bytes;
  evictRetainedFrames(session);
}

/** Freeze the coordinator's evidence synchronously, before the bridge awaits
 *  anything. Rows that do not fit the wire budget are reported unavailable:
 *  the coordinator has no browser-local copy to promise. */
export function freezeCoordinatorEvidence(
  sessionId: string,
  captureId: string,
  recordingId: string,
): CoordinatorEvidence {
  const session = armedSessions.get(sessionId);
  if (!session || session.recordingId !== recordingId) {
    return { json: "", records: 0, dropped: 0, bytes: 0, available: false };
  }
  const budget = TERMINAL_CAPTURE_LIMITS.coordinatorEvidenceBytes;
  const trimmed = selectRecordsWithinBudget(session.frames, budget - SECTION_HEADER_RESERVE);
  const dropped: TerminalCaptureDropCounters = {
    records: session.droppedRecords + trimmed.dropped,
    bytes: session.droppedBytes + trimmed.droppedBytes,
    rows: session.overBudgetRows,
    raw_bytes: 0,
    samples: 0,
  };
  const omissions: TerminalCaptureOmission[] = [];
  if (session.droppedRecords !== 0) {
    omissions.push({
      kind: "records",
      name: "coordinator.records",
      reason: "segment_evicted",
      dropped_count: session.droppedRecords,
      dropped_bytes: session.droppedBytes,
      range: session.droppedRange,
    });
  }
  if (trimmed.dropped !== 0) {
    omissions.push({
      kind: "records",
      name: "coordinator.records",
      reason: "evidence_trimmed",
      dropped_count: trimmed.dropped,
      dropped_bytes: trimmed.droppedBytes,
      range: trimmed.range,
    });
  }
  if (session.overBudgetRecords !== 0) {
    omissions.push({
      kind: "rows",
      name: "coordinator.records[].canonical",
      reason: "frame_over_budget",
      dropped_count: session.overBudgetRecords,
      dropped_bytes: 0,
      range: null,
    });
  }
  const last = session.frames[session.frames.length - 1]?.record ?? null;
  const section: TerminalCoordinatorSection = {
    layer: "coordinator",
    captured_at_ms: Date.now(),
    process: {
      layer: "coordinator",
      process_id: COORDINATOR_PROCESS_ID,
      git_sha: COORD_GIT_SHA,
      artifact_version: ROOST_ARTIFACT_VERSION,
      wasm_identity: null,
      worker_fp: null,
      viewer_id: null,
      user_agent: null,
    },
    stream: last === null ? null : last.stream,
    geometry: last === null ? null : { cols: last.stream.cols, rows: last.stream.rows },
    dropped,
    omissions,
    records: trimmed.records,
    snapshot: last === null ? null : last.stream,
    // The retained evidence ends on a complete canonical checkpoint, which is
    // what a replay needs. It is not a claim about the live cache after that
    // boundary.
    valid: last !== null && last.canonical !== null,
  };
  // The section is NESTED under its layer name, never flattened onto the
  // envelope: a flattened payload passes an envelope check and then fails as a
  // section, silently dropping this whole layer from the bundle.
  const payload: TerminalCaptureCoordinatorPayload = {
    schema: TERMINAL_INCIDENT_SCHEMA,
    layer: "coordinator",
    capture_id: captureId,
    recording_id: recordingId,
    session_id: sessionId,
    coordinator: section,
  };
  const json = JSON.stringify(payload);
  const bytes = utf8ByteLength(json);
  if (bytes > budget) {
    return {
      json: "",
      records: 0,
      dropped: session.frames.length,
      bytes: 0,
      available: false,
    };
  }
  return { json, records: trimmed.records.length, dropped: dropped.records, bytes, available: true };
}

/** Test/diagnostic seam: retained record count and byte cost for one session. */
export function _coordinatorRecorderStats(
  sessionId: string,
): { recordingId: string; records: number; bytes: number; dropped: number } | null {
  const session = armedSessions.get(sessionId);
  return session === undefined
    ? null
    : {
      recordingId: session.recordingId,
      records: session.frames.length,
      bytes: session.bytes,
      dropped: session.droppedRecords,
    };
}

/** Test/diagnostic seam: the retained records themselves, oldest first. */
export function _coordinatorRecorderRecords(
  sessionId: string,
): readonly TerminalCoordinatorRecord[] {
  return (armedSessions.get(sessionId)?.frames ?? []).map((frame) => frame.record);
}

export function _resetCoordinatorRecorder(): void {
  armedSessions.clear();
}

/** A full that does not continue the retained fold means the coordinator lost
 *  its baseline and asked the worker for a fresh one. Deltas cannot gap: the
 *  hub folds a delta only when its base sequence equals its cached sequence. */
function sequenceGap(
  previous: TerminalCoordinatorRecord | null,
  canonical: CellGridFrame,
  admitted: AdmittedFrameIdentity,
): { from: string; to: string } | null {
  if (
    previous === null
    || previous.stream.stream_id !== canonical.streamId
    || previous.stream.grid_epoch !== canonical.gridEpoch
  ) return null;
  const previousSeq = BigInt(previous.stream.seq);
  const base = admitted.full ? previousSeq : BigInt(admitted.baseSeq);
  if (base === previousSeq && BigInt(canonical.seq) === previousSeq + 1n) return null;
  return { from: previous.stream.seq, to: String(canonical.seq) };
}

/** Eviction removes whole records oldest-first and then keeps dropping while
 *  the head carries no canonical, so the oldest retained record is always a
 *  complete checkpoint rather than an apparently replayable orphan delta. */
function evictRetainedFrames(session: ArmedSession): void {
  while (
    session.frames.length > TERMINAL_CAPTURE_LIMITS.layerEntries
    || session.bytes > TERMINAL_CAPTURE_LIMITS.layerBytes
  ) {
    if (!dropOldestFrame(session)) return;
  }
  while (session.frames.length > 1 && session.frames[0]!.record.canonical === null) {
    dropOldestFrame(session);
  }
}

function dropOldestFrame(session: ArmedSession): boolean {
  const oldest = session.frames.shift();
  if (oldest === undefined) return false;
  session.bytes -= oldest.bytes;
  session.droppedRecords++;
  session.droppedBytes += oldest.bytes;
  const seq = oldest.record.stream.seq;
  if (session.droppedRange === null) session.droppedRange = { start: seq, end: seq };
  else session.droppedRange.end = seq;
  return true;
}

/** Newest records first so a capture keeps the evidence around the incident,
 *  then restored to chronological order. A leading record with no canonical
 *  cannot anchor a replay, so it is dropped with the records before it. */
function selectRecordsWithinBudget(
  frames: readonly RecordedFrame[],
  budget: number,
): {
  records: TerminalCoordinatorRecord[];
  dropped: number;
  droppedBytes: number;
  range: SequenceRange | null;
} {
  const kept: RecordedFrame[] = [];
  let used = 2;
  for (let idx = frames.length - 1; idx >= 0; idx--) {
    const frame = frames[idx]!;
    const cost = utf8ByteLength(JSON.stringify(frame.record)) + 1;
    if (used + cost > budget) break;
    used += cost;
    kept.push(frame);
  }
  kept.reverse();
  while (kept.length !== 0 && kept[0]!.record.canonical === null) kept.shift();
  const droppedFrames = frames.slice(0, frames.length - kept.length);
  let droppedBytes = 0;
  for (const frame of droppedFrames) droppedBytes += frame.bytes;
  return {
    records: kept.map((frame) => frame.record),
    dropped: droppedFrames.length,
    droppedBytes,
    range: droppedFrames.length === 0
      ? null
      : {
        start: droppedFrames[0]!.record.stream.seq,
        end: droppedFrames[droppedFrames.length - 1]!.record.stream.seq,
      },
  };
}

/** Exact JSON cost of one canonical viewport without serializing it: the
 *  retention bound must hold on the admission path, where the frame was just
 *  folded and a second pass over its rows is the same order of work. */
function estimateFrameJsonBytes(frame: CellGridFrame): number {
  let bytes = FRAME_JSON_OVERHEAD;
  for (const row of frame.viewportRows) {
    bytes += ROW_JSON_OVERHEAD;
    for (const span of row.spans) {
      bytes += SPAN_JSON_OVERHEAD + utf8ByteLength(span.text);
      if (span.linkUri !== undefined) bytes += utf8ByteLength(span.linkUri) + 12;
      if (span.linkKey !== undefined) bytes += utf8ByteLength(span.linkKey) + 12;
    }
  }
  return bytes;
}
