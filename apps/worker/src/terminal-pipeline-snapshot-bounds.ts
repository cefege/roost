// Shared bounds and protobuf shaping for worker terminal-pipeline evidence.
// The live sampler owns source reads; this module owns deterministic admission,
// numeric normalization, fixed age buckets, and exact response-size fencing.
// It contains no terminal, keeper, or generic diagnostic content.

import { create, toBinary } from "@bufbuild/protobuf";
import {
  TerminalPipelineReason,
  TerminalPipelineStage,
  TerminalPipelineStageSnapshotSchema,
  type TerminalPipelineSessionSnapshot,
  type TerminalPipelineStageSnapshot,
  type TerminalPipelineTarget,
} from "@roost/shared/proto/wire_pb";
import {
  WTerminalPipelineSnapshotSchema,
  type DTerminalPipelineSnapshotRequest,
  type WTerminalPipelineSnapshot,
} from "@roost/shared/proto/worker_transport_pb";
import type { CoordLinkPipelineState } from "./transport/coord-link-types.ts";

export const TERMINAL_PIPELINE_MAX_TARGETS = 64;
export const TERMINAL_PIPELINE_MAX_RESPONSE_BYTES = 64 * 1024;
export const TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS = 16;
// Tag 19 and its nested length use at most five bytes; keep conservative
// headroom.
const TERMINAL_PIPELINE_MAX_ENVELOPE_BYTES = 8;
// Target IDs are echoed into every record, so cap them before response
// encoding.
const TERMINAL_PIPELINE_MAX_IDENTIFIER_BYTES = 512;
const TERMINAL_PIPELINE_MAX_REQUEST_ID_BYTES = 256;
const HISTOGRAM_UPPER_BOUND_MS = [
  1, 2, 4, 8, 16, 32, 64, 128,
  256, 512, 1_000, 2_000, 5_000, 10_000, 30_000, Number.POSITIVE_INFINITY,
] as const;

export interface TerminalPipelineOrderedTarget {
  target: TerminalPipelineTarget;
  ordinal: number;
}

export interface PipelineStageInput {
  stage: TerminalPipelineStage;
  reason: TerminalPipelineReason;
  generation?: number;
  streamId?: string;
  sequence?: number;
  queueFrames?: number;
  queueBytes?: number;
  nativeBufferedBytes?: number;
  oldestAgeMs?: number;
  count?: number;
  histogramBuckets?: readonly bigint[];
}

export function admitTerminalPipelineTargets(
  request: DTerminalPipelineSnapshotRequest,
): TerminalPipelineOrderedTarget[] {
  const targets = Array.isArray(request.targets) ? request.targets : [];
  return targets
    .slice(0, TERMINAL_PIPELINE_MAX_TARGETS)
    .map((target, ordinal) => ({ target, ordinal }))
    .sort(compareTerminalPipelineTargets);
}

export function terminalPipelineDroppedTargets(
  request: DTerminalPipelineSnapshotRequest,
  admittedTargets: readonly TerminalPipelineOrderedTarget[],
): number {
  const targetCount = Array.isArray(request.targets) ? request.targets.length : 0;
  return terminalPipelineBoundedCount(targetCount - admittedTargets.length);
}

export function terminalPipelineTargetIdentifiersAreBounded(
  target: TerminalPipelineTarget,
): boolean {
  return terminalPipelineIdentifierIsBounded(target.sessionId)
    && terminalPipelineIdentifierIsBounded(target.viewId);
}

export function terminalPipelineBoundedIdentifier(value: string): string {
  return terminalPipelineIdentifierIsBounded(value) ? value : "";
}

export function terminalPipelineStage(input: PipelineStageInput): TerminalPipelineStageSnapshot {
  return create(TerminalPipelineStageSnapshotSchema, {
    stage: input.stage,
    reason: input.reason,
    generation: terminalPipelineUint64(input.generation),
    streamId: input.streamId ?? "",
    sequence: terminalPipelineUint64(input.sequence),
    queueFrames: terminalPipelineUint64(input.queueFrames),
    queueBytes: terminalPipelineUint64(input.queueBytes),
    nativeBufferedBytes: terminalPipelineUint64(input.nativeBufferedBytes),
    oldestAgeMs: terminalPipelineUint64(input.oldestAgeMs),
    count: terminalPipelineUint64(input.count),
    histogramBuckets: input.histogramBuckets?.slice(0, TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS) ?? [],
  });
}

export function terminalPipelineHistogramForAges(ages: readonly number[]): bigint[] {
  if (ages.length === 0) return [];
  const buckets = emptyTerminalPipelineHistogram();
  for (const ageMs of ages) buckets[terminalPipelineHistogramIndex(ageMs)]! += 1n;
  return buckets;
}

export function emptyTerminalPipelineHistogram(): bigint[] {
  return Array<bigint>(TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS).fill(0n);
}

export function terminalPipelineHistogramIndex(ageMs: number): number {
  for (let idx = 0; idx < HISTOGRAM_UPPER_BOUND_MS.length; idx += 1) {
    if (ageMs <= HISTOGRAM_UPPER_BOUND_MS[idx]!) return idx;
  }
  return TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS - 1;
}

export function terminalPipelineAge(nowMonoMs: number, startedMonoMs: number): number {
  return terminalPipelineNonnegativeInteger(nowMonoMs - startedMonoMs);
}

export function terminalPipelineOldestAge(ages: readonly number[]): number {
  let oldest = 0;
  for (const ageMs of ages) oldest = Math.max(oldest, ageMs);
  return oldest;
}

export function normalizeTerminalPipelineTransportState(
  state: CoordLinkPipelineState,
): CoordLinkPipelineState {
  return {
    queueFrames: terminalPipelineNonnegativeInteger(state.queueFrames),
    queueBytes: terminalPipelineNonnegativeInteger(state.queueBytes),
    nativeBufferedBytes: terminalPipelineNonnegativeInteger(state.nativeBufferedBytes),
    attached: state.attached === true,
  };
}

export function terminalPipelineResponse(
  requestId: string,
  sessions: readonly TerminalPipelineSessionSnapshot[],
  droppedTargets: number,
  droppedRecords: number,
): WTerminalPipelineSnapshot {
  return create(WTerminalPipelineSnapshotSchema, {
    requestId,
    sessions: [...sessions],
    droppedTargets,
    droppedRecords,
  });
}

export function terminalPipelineResponseBytes(snapshot: WTerminalPipelineSnapshot): number {
  return toBinary(WTerminalPipelineSnapshotSchema, snapshot).byteLength;
}

export function terminalPipelineResponseFits(
  snapshot: WTerminalPipelineSnapshot,
): boolean {
  return terminalPipelineResponseBytes(snapshot)
    <= TERMINAL_PIPELINE_MAX_RESPONSE_BYTES - TERMINAL_PIPELINE_MAX_ENVELOPE_BYTES;
}

export function terminalPipelineBoundedRequestId(requestId: string): string {
  return typeof requestId === "string" && Buffer.byteLength(requestId) <= TERMINAL_PIPELINE_MAX_REQUEST_ID_BYTES
    ? requestId
    : "";
}

export function terminalPipelineBoundedCount(value: number): number {
  return Math.min(0xffff_ffff, terminalPipelineNonnegativeInteger(value));
}

export function terminalPipelineNonnegativeInteger(value: number): number {
  return Number.isFinite(value) && value > 0
    ? Math.min(Number.MAX_SAFE_INTEGER, Math.floor(value))
    : 0;
}

function compareTerminalPipelineTargets(
  left: TerminalPipelineOrderedTarget,
  right: TerminalPipelineOrderedTarget,
): number {
  const sessionOrder = compareTerminalPipelineIds(left.target.sessionId, right.target.sessionId);
  if (sessionOrder !== 0) return sessionOrder;
  const viewOrder = compareTerminalPipelineIds(left.target.viewId, right.target.viewId);
  return viewOrder !== 0 ? viewOrder : left.ordinal - right.ordinal;
}

function compareTerminalPipelineIds(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}

function terminalPipelineIdentifierIsBounded(value: string): boolean {
  return typeof value === "string"
    && Buffer.byteLength(value) <= TERMINAL_PIPELINE_MAX_IDENTIFIER_BYTES;
}

function terminalPipelineUint64(value: number | undefined): bigint {
  return BigInt(terminalPipelineNonnegativeInteger(value ?? 0));
}
