// Per-layer checkpoint extraction, matching and attribution for one terminal
// incident bundle. Called only by scripts/replay-terminal-incident.ts.
// Everything here is content-free in its OUTPUT: it reads terminal rows to
// compare them and reports identities, coordinates and field names only.
// Comparison is @roost/protocol/terminal-capture's canonical view, folding is the
// production cell pipeline, and the core replay uses the pinned WASM factory —
// nothing is reimplemented here, so a drift in production shows up as a
// divergence rather than being masked by a second implementation.

import {
  applyDelta,
  cloneCellGridFrame,
  gridToCellFrame,
  normalizeCellGridFrame,
  type CellGridFrame,
} from "../packages/protocol/src/cell/index.ts";
import {
  createWtermCore,
  resizeWtermCore,
} from "../packages/wterm/src/wterm-core-factory.ts";
import {
  canonicalViewOfFrame,
  compareCanonicalViews,
  type TerminalBrowserPaintedState,
  type TerminalCanonicalDifference,
  type TerminalCanonicalView,
  type TerminalCaptureStreamIdentity,
  type TerminalCoordinatorSection,
  type TerminalCoverageReason,
  type TerminalWorkerSection,
} from "../packages/protocol/src/terminal-capture.ts";

export interface LayerCheckpoint {
  readonly stream: TerminalCaptureStreamIdentity;
  readonly view: TerminalCanonicalView;
}

export interface CheckpointSet {
  readonly checkpoints: Map<string, LayerCheckpoint>;
  readonly reasons: TerminalCoverageReason[];
}

export interface MatchReport {
  readonly matched: number;
  readonly unmatched: number;
  readonly difference: {
    readonly stream: TerminalCaptureStreamIdentity;
    readonly difference: TerminalCanonicalDifference;
  } | null;
}

/** Identity a checkpoint is matched on. Two layers are compared ONLY at the
 *  same stream, epoch, sequence and dimensions; anything else is unmatched
 *  evidence, never a difference. */
function checkpointKey(stream: TerminalCaptureStreamIdentity): string {
  return `${stream.stream_id}|${stream.grid_epoch}|${stream.seq}|${stream.cols}x${stream.rows}`;
}

/** Fold the worker's own emitted frames per segment with the production
 *  `applyDelta`. A full establishes the baseline; a delta advances it; a delta
 *  arriving with no baseline invalidates the segment until the next full,
 *  because folding onto a guess would manufacture a screen nobody shipped. */
export function workerFoldCheckpoints(section: TerminalWorkerSection): CheckpointSet {
  const checkpoints = new Map<string, LayerCheckpoint>();
  const reasons: TerminalCoverageReason[] = [];
  const foldBySegment = new Map<string, CellGridFrame | null>();
  for (const emission of section.emissions) {
    const segmentId = emission.segment_id;
    if (emission.frame.full) {
      foldBySegment.set(segmentId, cloneCellGridFrame(emission.frame));
    } else {
      const held = foldBySegment.get(segmentId) ?? null;
      if (held === null) {
        if (!reasons.includes("baseline_invalidated")) reasons.push("baseline_invalidated");
        continue;
      }
      const folded = applyDelta(held, emission.frame);
      if (folded === null) {
        foldBySegment.set(segmentId, null);
        if (!reasons.includes("baseline_invalidated")) reasons.push("baseline_invalidated");
        continue;
      }
      foldBySegment.set(segmentId, folded);
    }
    const fold = foldBySegment.get(segmentId);
    if (!fold) continue;
    // Two reasons this clones AND normalizes, both load-bearing:
    //  * applyDelta CONSUMES and mutates the held frame, and a canonical view
    //    keeps its row array by reference, so storing the live fold would let
    //    every later delta retroactively rewrite checkpoints already taken —
    //    inventing a divergence at exactly the row the application redrew.
    //  * a delta carries sbBase 0 by contract while a canonical checkpoint
    //    carries sbBase = scrollbackTotal, so an un-normalized fold disagrees
    //    with every canonical replica on a history WINDOW neither side is
    //    comparing. The worker's own emission comparison normalizes its
    //    baseline the same way (diag/terminal-capture-emission.ts).
    const view = canonicalViewOfFrame(
      normalizeCellGridFrame(cloneCellGridFrame(fold)),
    );
    if (!view) continue;
    checkpoints.set(checkpointKey(emission.stream), { stream: emission.stream, view });
  }
  if (reasons.length === 0) reasons.push("complete");
  return { checkpoints, reasons };
}

/** Fresh-core scans the worker took at an emission's exact generation. These are
 *  the only worker-side evidence of what the CORE held, as opposed to what the
 *  worker shipped. */
export function workerCoreCheckpoints(section: TerminalWorkerSection): CheckpointSet {
  const checkpoints = new Map<string, LayerCheckpoint>();
  const reasons: TerminalCoverageReason[] = [];
  for (const sample of section.core_samples) {
    const view = canonicalViewOfFrame(sample.core_frame);
    if (!view) {
      if (!reasons.includes("frame_over_budget")) reasons.push("frame_over_budget");
      continue;
    }
    checkpoints.set(checkpointKey(sample.stream), { stream: sample.stream, view });
  }
  if (checkpoints.size === 0 && !reasons.includes("sample_budget_exceeded")) {
    reasons.push("layer_unavailable");
  }
  if (reasons.length === 0) reasons.push("complete");
  return { checkpoints, reasons };
}

export function coordinatorCheckpoints(section: TerminalCoordinatorSection): CheckpointSet {
  const checkpoints = new Map<string, LayerCheckpoint>();
  const reasons: TerminalCoverageReason[] = [];
  for (const record of section.records) {
    if (!record.accepted || record.canonical === null) continue;
    const view = canonicalViewOfFrame(record.canonical);
    if (!view) {
      if (!reasons.includes("baseline_invalidated")) reasons.push("baseline_invalidated");
      continue;
    }
    checkpoints.set(checkpointKey(record.stream), { stream: record.stream, view });
  }
  if (reasons.length === 0) reasons.push("complete");
  return { checkpoints, reasons };
}

/** Browser replica canonical viewports, one per retained painted state.
 *
 *  Keyed by the CANONICAL frame's own identity, never by the renderer's
 *  committed watermark: those are two different clocks. A pre-apply snapshot
 *  holds the DOM committed at seq N beside the replica canonical at seq M>N,
 *  and labelling that canonical frame "seq N" would compare the coordinator's
 *  seq-N frame against the browser's seq-M state and report every later
 *  keystroke as a divergence. */
export function browserCheckpoints(
  states: readonly (TerminalBrowserPaintedState | null)[],
): CheckpointSet {
  const checkpoints = new Map<string, LayerCheckpoint>();
  const reasons: TerminalCoverageReason[] = [];
  for (const state of states) {
    if (!state || state.canonical === null) continue;
    const view = canonicalViewOfFrame(state.canonical);
    if (!view) continue;
    const stream: TerminalCaptureStreamIdentity = {
      stream_id: state.canonical.streamId,
      grid_epoch: state.canonical.gridEpoch,
      seq: String(state.canonical.seq),
      base_seq: null,
      cols: state.canonical.cols,
      rows: state.canonical.rows,
    };
    checkpoints.set(checkpointKey(stream), { stream, view });
  }
  if (checkpoints.size === 0) reasons.push("layer_unavailable");
  if (reasons.length === 0) reasons.push("complete");
  return { checkpoints, reasons };
}

/** Compare two layers ONLY where their identities match. An identity present on
 *  one side alone is unmatched coverage, never a divergence. */
export function matchLayers(left: CheckpointSet, right: CheckpointSet): MatchReport {
  let matched = 0;
  let unmatched = 0;
  let difference: MatchReport["difference"] = null;
  for (const [key, leftPoint] of left.checkpoints) {
    const rightPoint = right.checkpoints.get(key);
    if (!rightPoint) {
      unmatched++;
      continue;
    }
    matched++;
    if (difference !== null) continue;
    const found = compareCanonicalViews(leftPoint.view, rightPoint.view);
    if (found) difference = { stream: leftPoint.stream, difference: found };
  }
  for (const key of right.checkpoints.keys()) {
    if (!left.checkpoints.has(key)) unmatched++;
  }
  return { matched, unmatched, difference };
}

export interface RawReplayResult {
  readonly status: "complete" | "partial" | "unavailable";
  readonly reason: TerminalCoverageReason;
  readonly bytes: number;
  readonly resizes: number;
  readonly difference: TerminalCanonicalDifference | null;
  readonly comparedAgainst: "worker_fold" | "none";
}

/** Replay the retained raw bytes through a FRESH pinned core, applying each
 *  keeper-acknowledged resize at its exact byte boundary, and compare the
 *  result with the worker's own emitted fold.
 *
 *  The pinned core has no exact import API, so the ONLY provable replay starts
 *  at absolute offset 0 — this exact core's initialization. A recording armed
 *  mid-session opens its segment at the head offset it was armed at, and a
 *  replay from there reconstructs a parser state it never observed: it reports
 *  `missing_initial_prefix` and attributes nothing rather than blaming a core
 *  for output it was never shown. A segment that DID open at 0 but whose
 *  oldest raw records were evicted by the recorder's bounded pools reports
 *  `raw_prefix_evicted`, which is a different fact about a different failure.
 *  Captured bytes are written to a core; they are never executed. */
export async function replayRawToCore(
  section: TerminalWorkerSection,
  foldCheckpoints: CheckpointSet,
): Promise<RawReplayResult> {
  const segment = section.segments[0];
  if (!segment || section.raw.length === 0) {
    return unavailableReplay("layer_unavailable");
  }
  if (segment.open_offset !== "0") {
    return unavailableReplay("missing_initial_prefix");
  }
  const chunks = section.raw
    .filter((record) => record.segment_id === segment.segment_id)
    .slice()
    .sort((left, right) => (BigInt(left.start_offset) < BigInt(right.start_offset) ? -1 : 1));
  if (chunks.length === 0) return unavailableReplay("layer_unavailable");
  if (chunks[0]!.start_offset !== "0") {
    return unavailableReplay("raw_prefix_evicted");
  }
  for (let idx = 1; idx < chunks.length; idx++) {
    if (chunks[idx]!.start_offset !== chunks[idx - 1]!.end_offset) {
      return unavailableReplay("raw_prefix_evicted");
    }
  }
  const resizes = section.resizes
    .filter((record) => record.segment_id === segment.segment_id)
    .slice()
    .sort((left, right) => (BigInt(left.install_offset) < BigInt(right.install_offset) ? -1 : 1));
  if (resizes.some((record) => record.boundary_offset === null)) {
    return unavailableReplay("missing_resize_boundary");
  }

  const core = await createWtermCore(segment.geometry.cols, segment.geometry.rows);
  let pendingResize = 0;
  let written = 0;
  for (const chunk of chunks) {
    let bytes = Buffer.from(chunk.base64, "base64");
    let offset = BigInt(chunk.start_offset);
    while (bytes.byteLength > 0) {
      const next = resizes[pendingResize];
      const boundary = next ? BigInt(next.boundary_offset!) : null;
      if (boundary !== null && boundary <= offset) {
        resizeWtermCore(core, next!.to);
        pendingResize++;
        continue;
      }
      const cut = boundary === null
        ? bytes.byteLength
        : Number(boundary - offset);
      const slice = bytes.subarray(0, Math.min(cut, bytes.byteLength));
      core.writeRaw(new Uint8Array(slice));
      written += slice.byteLength;
      offset += BigInt(slice.byteLength);
      bytes = bytes.subarray(slice.byteLength);
    }
  }
  while (pendingResize < resizes.length) {
    resizeWtermCore(core, resizes[pendingResize]!.to);
    pendingResize++;
  }

  // A replay that reached here started at this core's initialization, so the
  // monotonic origin is exactly what THIS core has evicted — the same read
  // emitter.ts::scrollbackOrigin performs, with no held state to add.
  const replayed = canonicalViewOfFrame(gridToCellFrame(
    core,
    0,
    segment.grid_epoch,
    segment.stream_id,
    0,
    core.getScrollbackDiscardedCount?.() ?? 0,
  ));
  const finalFold = lastCheckpoint(foldCheckpoints);
  if (!replayed || !finalFold) {
    return {
      status: "partial",
      reason: "layer_unavailable",
      bytes: written,
      resizes: resizes.length,
      difference: null,
      comparedAgainst: "none",
    };
  }
  return {
    status: "complete",
    reason: "complete",
    bytes: written,
    resizes: resizes.length,
    difference: compareCanonicalViews(replayed, finalFold.view),
    comparedAgainst: "worker_fold",
  };
}

function lastCheckpoint(set: CheckpointSet): LayerCheckpoint | null {
  let last: LayerCheckpoint | null = null;
  for (const checkpoint of set.checkpoints.values()) last = checkpoint;
  return last;
}

function unavailableReplay(reason: TerminalCoverageReason): RawReplayResult {
  return {
    status: "unavailable",
    reason,
    bytes: 0,
    resizes: 0,
    difference: null,
    comparedAgainst: "none",
  };
}
