// Bounded, content-free worker evidence for terminal pipeline samples.
// CoordLink routes one typed request here after it has decoded protobuf bytes.
// This owner only reads live session, terminal, keeper, and transport state.
// It never serializes terminal content, generic diagnostics, or free-form errors.

import { create } from "@bufbuild/protobuf";
import {
  TerminalPipelineReason,
  TerminalPipelineSessionSnapshotSchema,
  TerminalPipelineStage,
  type TerminalPipelineSessionSnapshot,
  type TerminalPipelineTarget,
} from "@roost/shared/proto/wire_pb";
import type {
  DTerminalPipelineSnapshotRequest,
  WTerminalPipelineSnapshot,
} from "@roost/shared/proto/worker_transport_pb";
import {
  TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS,
  TERMINAL_PIPELINE_MAX_RESPONSE_BYTES,
  admitTerminalPipelineTargets,
  normalizeTerminalPipelineTransportState,
  terminalPipelineAge,
  terminalPipelineBoundedCount,
  terminalPipelineBoundedIdentifier,
  terminalPipelineBoundedRequestId,
  terminalPipelineDroppedTargets,
  terminalPipelineHistogramForAges,
  terminalPipelineHistogramIndex,
  terminalPipelineNonnegativeInteger,
  terminalPipelineOldestAge,
  terminalPipelineResponse,
  terminalPipelineResponseFits,
  terminalPipelineStage,
  terminalPipelineTargetIdentifiersAreBounded,
  type TerminalPipelineOrderedTarget,
} from "./terminal-pipeline-snapshot-bounds.ts";
import {
  getMultiplexedPool,
  type MultiplexedKeeperPool,
} from "./keeper/multiplexed-client.ts";
import type { SessionManager } from "./session-manager.ts";
import type { SessionRecord } from "./session-record.ts";
import type { TerminalStreamState } from "./session-terminal-state.ts";
import type { CoordLinkPipelineState } from "./transport/coord-link-types.ts";
import { monoNowMs } from "./util/mono.ts";

export {
  TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS,
  TERMINAL_PIPELINE_MAX_RESPONSE_BYTES,
  TERMINAL_PIPELINE_MAX_TARGETS,
} from "./terminal-pipeline-snapshot-bounds.ts";

interface KeeperPipelineFacts {
  inputFrames: number;
  inputBytes: number;
  resizeFrames: number;
  oldestAgeMs: number;
  histogramBuckets: bigint[];
}

/** Samples a request synchronously so every target sees one coherent owner read. */
export function terminalPipelineSnapshot(
  manager: SessionManager,
  request: DTerminalPipelineSnapshotRequest,
  transport: CoordLinkPipelineState,
): WTerminalPipelineSnapshot {
  const requestId = terminalPipelineBoundedRequestId(request.requestId);
  const orderedTargets = admitTerminalPipelineTargets(request);
  const droppedTargets = terminalPipelineDroppedTargets(request, orderedTargets);
  const nowMonoMs = monoNowMs();
  const sessionsById = indexSessions(manager);
  const keeper = getMultiplexedPool();
  const keeperFacts = sampleKeeperFacts(
    keeper,
    sessionsById,
    orderedTargets,
    nowMonoMs,
  );
  const transportState = normalizeTerminalPipelineTransportState(transport);
  const sessions: TerminalPipelineSessionSnapshot[] = [];
  let droppedRecords = 0;

  for (const { target } of orderedTargets) {
    if (!terminalPipelineTargetIdentifiersAreBounded(target)) {
      droppedRecords = terminalPipelineBoundedCount(droppedRecords + 1);
      continue;
    }
    const session = snapshotSession(
      manager,
      sessionsById.get(target.sessionId),
      target,
      keeper,
      keeperFacts,
      transportState,
      nowMonoMs,
    );
    sessions.push(session);
    const candidate = terminalPipelineResponse(
      requestId,
      sessions,
      droppedTargets,
      droppedRecords,
    );
    if (terminalPipelineResponseFits(candidate)) continue;
    sessions.pop();
    droppedRecords = terminalPipelineBoundedCount(droppedRecords + 1);
  }

  let boundedResponse = terminalPipelineResponse(
    requestId,
    sessions,
    droppedTargets,
    droppedRecords,
  );
  while (
    terminalPipelineResponseFits(boundedResponse) === false
    && sessions.length > 0
  ) {
    sessions.pop();
    droppedRecords = terminalPipelineBoundedCount(droppedRecords + 1);
    boundedResponse = terminalPipelineResponse(
      requestId,
      sessions,
      droppedTargets,
      droppedRecords,
    );
  }
  return boundedResponse;
}


function indexSessions(manager: SessionManager): Map<string, SessionRecord> {
  const sessionsById = new Map<string, SessionRecord>();
  for (const session of manager.sessions.values()) {
    sessionsById.set(String(session.sessionId), session);
  }
  return sessionsById;
}

function sampleKeeperFacts(
  keeper: MultiplexedKeeperPool,
  sessionsById: ReadonlyMap<string, SessionRecord>,
  targets: readonly TerminalPipelineOrderedTarget[],
  nowMonoMs: number,
): ReadonlyMap<number, KeeperPipelineFacts> {
  const factsByChannel = new Map<number, KeeperPipelineFacts>();
  for (const { target } of targets) {
    const session = sessionsById.get(target.sessionId);
    if (session) factsByChannel.set(session.channelId, emptyKeeperFacts());
  }

  for (const pending of keeper.pendingInputs.values()) {
    const facts = factsByChannel.get(pending.channelId);
    if (!facts) continue;
    facts.inputFrames += 1;
    observeAge(facts, terminalPipelineAge(nowMonoMs, pending.startedMonoMs));
  }
  for (const pending of keeper.pendingResizes.values()) {
    const facts = factsByChannel.get(pending.channelId);
    if (!facts) continue;
    facts.resizeFrames += 1;
    observeAge(facts, terminalPipelineAge(nowMonoMs, pending.startedMonoMs));
  }
  for (const [channelId, usage] of keeper._pendingInputUsage) {
    const facts = factsByChannel.get(channelId);
    if (!facts) continue;
    facts.inputFrames = Math.max(
      facts.inputFrames,
      terminalPipelineNonnegativeInteger(usage.commands),
    );
    facts.inputBytes = terminalPipelineNonnegativeInteger(usage.bytes);
  }
  return factsByChannel;
}

function emptyKeeperFacts(): KeeperPipelineFacts {
  return {
    inputFrames: 0,
    inputBytes: 0,
    resizeFrames: 0,
    oldestAgeMs: 0,
    histogramBuckets: [],
  };
}

function observeAge(facts: KeeperPipelineFacts, ageMs: number): void {
  facts.oldestAgeMs = Math.max(facts.oldestAgeMs, ageMs);
  if (facts.histogramBuckets.length === 0) {
    facts.histogramBuckets = Array<bigint>(TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS).fill(0n);
  }
  facts.histogramBuckets[terminalPipelineHistogramIndex(ageMs)]! += 1n;
}

function snapshotSession(
  manager: SessionManager,
  session: SessionRecord | undefined,
  target: TerminalPipelineTarget,
  keeper: MultiplexedKeeperPool,
  keeperFactsByChannel: ReadonlyMap<number, KeeperPipelineFacts>,
  transport: CoordLinkPipelineState,
  nowMonoMs: number,
): TerminalPipelineSessionSnapshot {
  if (!session) {
    return create(TerminalPipelineSessionSnapshotSchema, {
      sessionId: target.sessionId,
      viewId: target.viewId,
      stages: [terminalPipelineStage({
        stage: TerminalPipelineStage.WORKER_STREAM,
        reason: TerminalPipelineReason.SESSION_NOT_FOUND,
      })],
    });
  }

  const channelId = session.channelId;
  const stream = manager.terminalStreams.get(channelId);
  const generation = stream?.version ?? 0;
  const streamId = terminalPipelineBoundedIdentifier(stream?.streamId ?? "");
  const sequence = session.cell_emit.seq;
  const rawMetadata = manager.rawMetadataQueues.get(channelId);
  const cellDirty = manager.cellDirty.has(channelId);
  const pendingRepair = manager.pendingCellRepairs.has(channelId);
  const cellGate = manager.cellEmissionGates.has(channelId);
  const syncOutput = manager.syncOutputHolds.get(channelId);
  const suppression = manager.cellGateSuppression.get(channelId);
  const controlLane = manager.terminalControlChains.get(channelId);
  const admissionLane = manager.keeperAdmissionLane.get(channelId);
  const keeperFacts = keeperFactsByChannel.get(channelId) ?? emptyKeeperFacts();
  const remainingSnapshotParts = stream?.snapshotCursor
    ? Math.max(0, stream.snapshotCursor.parts.length - stream.snapshotCursor.nextPart)
    : 0;
  const controlDepth = terminalPipelineNonnegativeInteger(controlLane?.depth ?? 0);
  const admissionDepth = terminalPipelineNonnegativeInteger(admissionLane?.depth ?? 0);
  const controlRunning = controlLane?.running ? 1 : 0;
  const admissionHeld = admissionLane?.holder ? 1 : 0;
  const controlAges = [
    controlLane?.running
      ? terminalPipelineAge(nowMonoMs, controlLane.runningSinceMonoMs)
      : null,
    admissionLane?.holder
      ? terminalPipelineAge(nowMonoMs, admissionLane.heldSinceMonoMs)
      : null,
  ].filter((age): age is number => age !== null);

  return create(TerminalPipelineSessionSnapshotSchema, {
    sessionId: target.sessionId,
    viewId: target.viewId,
    stages: [
      terminalPipelineStage({
        stage: TerminalPipelineStage.WORKER_PTY,
        reason: (rawMetadata?.frames.length ?? 0) > 0
          ? TerminalPipelineReason.RAW_METADATA_PENDING
          : TerminalPipelineReason.NONE,
        generation,
        streamId,
        sequence,
        queueFrames: rawMetadata?.frames.length ?? 0,
        queueBytes: rawMetadata?.bytes ?? 0,
        count: rawMetadata?.frames.length ?? 0,
      }),
      terminalPipelineStage({
        stage: TerminalPipelineStage.WORKER_CORE,
        reason: stream?.coreValid === false
          ? TerminalPipelineReason.CORE_INVALID
          : TerminalPipelineReason.NONE,
        generation,
        streamId,
        sequence,
        count: session.wtermCore ? 1 : 0,
      }),
      terminalPipelineStage({
        stage: TerminalPipelineStage.WORKER_SCHEDULER,
        reason: schedulerReason(syncOutput !== undefined, cellGate, pendingRepair, cellDirty),
        generation,
        streamId,
        sequence,
        queueFrames: Number(cellDirty) + Number(pendingRepair),
        count: Number(cellDirty) + Number(pendingRepair),
        oldestAgeMs: suppression
          ? terminalPipelineAge(nowMonoMs, suppression.sinceMonoMs)
          : 0,
        histogramBuckets: suppression
          ? terminalPipelineHistogramForAges([
            terminalPipelineAge(nowMonoMs, suppression.sinceMonoMs),
          ])
          : [],
      }),
      terminalPipelineStage({
        stage: TerminalPipelineStage.WORKER_STREAM,
        reason: streamReason(stream, pendingRepair),
        generation,
        streamId,
        sequence,
        queueFrames: remainingSnapshotParts,
        count: stream?.snapshotCursor?.parts.length ?? 0,
      }),
      terminalPipelineStage({
        stage: TerminalPipelineStage.WORKER_STREAM_CONTROL,
        reason: controlDepth + admissionDepth + controlRunning + admissionHeld > 0
          ? TerminalPipelineReason.CONTROL_QUEUED
          : TerminalPipelineReason.NONE,
        generation,
        streamId,
        sequence,
        queueFrames: controlDepth + admissionDepth,
        oldestAgeMs: terminalPipelineOldestAge(controlAges),
        count: controlDepth + admissionDepth + controlRunning + admissionHeld,
        histogramBuckets: terminalPipelineHistogramForAges(controlAges),
      }),
      terminalPipelineStage({
        stage: TerminalPipelineStage.WORKER_KEEPER,
        reason: keeperReason(
          Boolean(keeper.socket && !keeper.socket.destroyed),
          keeperFacts.inputFrames,
          keeperFacts.resizeFrames,
        ),
        generation,
        streamId,
        sequence,
        queueFrames: keeperFacts.inputFrames + keeperFacts.resizeFrames,
        queueBytes: keeperFacts.inputBytes,
        oldestAgeMs: keeperFacts.oldestAgeMs,
        count: keeperFacts.inputFrames + keeperFacts.resizeFrames,
        histogramBuckets: keeperFacts.histogramBuckets,
      }),
      terminalPipelineStage({
        stage: TerminalPipelineStage.WORKER_COORD_LINK,
        reason: transport.attached
          ? transport.nativeBufferedBytes > 0
            ? TerminalPipelineReason.NATIVE_BUFFERED
            : TerminalPipelineReason.NONE
          : TerminalPipelineReason.COORD_LINK_UNAVAILABLE,
        generation,
        streamId,
        sequence,
        queueFrames: transport.queueFrames,
        queueBytes: transport.queueBytes,
        nativeBufferedBytes: transport.nativeBufferedBytes,
        count: transport.queueFrames,
      }),
    ],
  });
}

function schedulerReason(
  syncOutput: boolean,
  cellGate: boolean,
  pendingRepair: boolean,
  cellDirty: boolean,
): TerminalPipelineReason {
  if (syncOutput) return TerminalPipelineReason.SYNC_OUTPUT;
  if (cellGate) return TerminalPipelineReason.CELL_GATE;
  if (pendingRepair) return TerminalPipelineReason.PENDING_REPAIR;
  return cellDirty ? TerminalPipelineReason.CELL_DIRTY : TerminalPipelineReason.NONE;
}

function streamReason(
  stream: TerminalStreamState | undefined,
  pendingRepair: boolean,
): TerminalPipelineReason {
  if (!stream) return TerminalPipelineReason.STREAM_NOT_FOUND;
  if (!stream.enabled) return TerminalPipelineReason.STREAM_DISABLED;
  if (!stream.coreValid) return TerminalPipelineReason.CORE_INVALID;
  if (stream.snapshotCursor) return TerminalPipelineReason.SNAPSHOT_PENDING;
  if (!stream.baselineReady) return TerminalPipelineReason.BASELINE_PENDING;
  return pendingRepair ? TerminalPipelineReason.PENDING_REPAIR : TerminalPipelineReason.NONE;
}

function keeperReason(
  connected: boolean,
  inputFrames: number,
  resizeFrames: number,
): TerminalPipelineReason {
  if (!connected) return TerminalPipelineReason.KEEPER_DISCONNECTED;
  if (resizeFrames > 0) return TerminalPipelineReason.KEEPER_RESIZE_PENDING;
  return inputFrames > 0
    ? TerminalPipelineReason.KEEPER_INPUT_PENDING
    : TerminalPipelineReason.NONE;
}
