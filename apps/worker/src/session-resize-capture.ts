// Bounded resize capture + exactly-once terminal-core rebuild.
//
// A geometry-uncertain transaction must not parse one PTY byte at a size it
// cannot prove. The capture is installed atomically with the cell-emission gate
// BEFORE the transaction's first keeper operation and freezes the canonical core:
// captured bytes still advance head_seq, still land in the fixed raw ring, still
// feed the metadata/carry scans and the bounded coordinator raw lane — they just
// never touch the core.
//
// The capture owns NO byte-growing queue. The raw ring is the retained window
// (its eviction is recorded, not hidden), and the only extra state is the
// absolute boundary sequence plus the alt-screen mode at that boundary. The old
// unbounded `postResizeOutput: Buffer[]` grew without a cap for as long as a
// resize stayed unresolved, which is exactly the stall this replaces.

import type { SessionManager } from "./session-manager.ts";
import { diag, signal } from "@roost/shared/diag";
import { readRing, ringLength } from "./session-scrollback-ring.ts";
import { _createWtermCore } from "./session-constants.ts";
import { ALT_ENTER_SEQS } from "./terminal-stream-scan.ts";
import { answerQueries, drainCoreReplies } from "./terminal-query-reply.ts";
import { rewindToSequenceStart, skipOrphanSequencePrefix } from "./terminal-replay-align.ts";
import { initCellEmitState, scrollbackOrigin } from "@roost/shared/cell";
import { newTraceId } from "@roost/shared/trace";
import { monoNowMs } from "./util/mono.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { withKeeperAdmission } from "./session-control-lanes.ts";
import { resetUnhandledSequences } from "./session-unhandled-seq.ts";

/** Observable stages of one viewport transaction. The phase is what makes the
 *  failure classification decidable instead of guessed from an exception:
 *  `validating` failures are definite rejections that mutated nothing, and every
 *  phase from `keeper_written` on is ambiguous unless a typed keeper rejection
 *  proves the PTY was never resized. */
export type TerminalTxnPhase =
  | "validating"
  | "admitted"
  | "keeper_written"
  | "pty_resized"
  | "grid_rebuilt"
  | "settled";

/** Per-phase bound. Each phase is additionally clamped by the transaction's
 *  overall deadline, so the sum is a ceiling, never a reachable total.
 *  `keeper_written` carries the plan's 6 s keeper reconciliation budget (the
 *  per-command watchdog is 2.5 s, so it admits a finite number of attempts). */
export const TERMINAL_TXN_PHASE_BUDGET_MS: Readonly<Record<TerminalTxnPhase, number>> = {
  validating: 250,
  admitted: 3_000,
  keeper_written: 6_000,
  pty_resized: 4_000,
  grid_rebuilt: 500,
  settled: 0,
};

/** Whole-transaction ceiling. Under the coordinator's 8 s viewport-result
 *  timeout so the coordinator hears a truthful worker result rather than its own
 *  timeout, and above the 6 s keeper budget so a bounded ambiguity still gets to
 *  rebuild and clear its gate before reporting. */
export const VIEWPORT_TXN_BUDGET_MS = 7_000;

/** A gate older than the keeper's per-command budget is no longer explainable by
 *  one in-flight command; it is a stalled emitter and gets a corruption signal. */
export const CELL_GATE_BUDGET_MS = 2_500;

export interface ResizeCapture {
  reason: string;
  installedMonoMs: number;
  /** head_seq when the capture was installed. */
  installSeq: number;
  phase: TerminalTxnPhase;
  phaseSinceMonoMs: number;
  phaseDeadlineMonoMs: number;
  txnDeadlineMonoMs: number;
  /** Absolute head_seq of the last byte the keeper produced BEFORE it applied
   *  the resize; -1 until a keeper result frame proves it. */
  boundarySeq: number;
  /** Alt-screen mode at that boundary. The rebuild alt-primes from this instead
   *  of replaying old-geometry absolute cursor moves. */
  boundaryAltMode: boolean;
  capturedBytes: number;
  capturedChunks: number;
  /** Set when the ring evicted retained bytes below the boundary while the
   *  capture was open: replay-from-retained-ring state, stated rather than
   *  silently producing a shorter history. */
  ringEvicted: boolean;
  /** Actual core swaps performed for this capture. The rebuild is once-only, so
   *  this is 0 or 1 for the capture's whole lifetime. */
  rebuilds: number;
  /** Query replies the post-boundary tail produced and that were forwarded. */
  forwardedReplies: number;
  overBudget: boolean;
}

/** Install the capture and the cell gate in one synchronous step, and retire any
 *  open synchronized-output hold with them. Any window between the capture and
 *  the gate would either parse bytes at an unproven size or ship a cell frame
 *  built from one.
 *
 *  The hold is part of the same atomic step because the gate takes over
 *  emission: a hold left armed here measures its ceilings against a core this
 *  transaction is about to freeze and replace, and its wall timer would emit
 *  into the gate — which emitCellFrame checks BEFORE the force bypass and
 *  therefore discards. That silently spends the documented 1 s recovery ceiling
 *  inside the transaction and leaves the next chunk to open a FRESH 1 s ceiling
 *  stacked on top of the resize's own budget. From here the transaction's
 *  bounded phases are the only ceiling, and the first chunk after the gate
 *  clears opens a hold timed honestly against the replacement core. */
export function installResizeCapture(
  mgr: SessionManager,
  channelId: number,
  reason: string,
): ResizeCapture {
  const rec = mgr.sessions.get(channelId);
  const now = monoNowMs();
  const capture: ResizeCapture = {
    reason,
    installedMonoMs: now,
    installSeq: rec?.head_seq ?? 0,
    phase: "admitted",
    phaseSinceMonoMs: now,
    phaseDeadlineMonoMs: now + TERMINAL_TXN_PHASE_BUDGET_MS.admitted,
    txnDeadlineMonoMs: now + VIEWPORT_TXN_BUDGET_MS,
    boundarySeq: -1,
    boundaryAltMode: rec?.alt_mode ?? false,
    capturedBytes: 0,
    capturedChunks: 0,
    ringEvicted: false,
    rebuilds: 0,
    forwardedReplies: 0,
    overBudget: false,
  };
  mgr.resizeCaptures.set(channelId, capture);
  mgr.cellEmissionGates.add(channelId);
  mgr._releaseSyncOutputHold(channelId);
  return capture;
}

/** Advance the transaction phase and re-arm its bounded deadline. */
export function enterPhase(capture: ResizeCapture, phase: TerminalTxnPhase): void {
  const now = monoNowMs();
  capture.phase = phase;
  capture.phaseSinceMonoMs = now;
  capture.phaseDeadlineMonoMs = Math.min(
    now + TERMINAL_TXN_PHASE_BUDGET_MS[phase],
    capture.txnDeadlineMonoMs,
  );
}

export function phaseRemainingMs(capture: ResizeCapture): number {
  return capture.phaseDeadlineMonoMs - monoNowMs();
}

/** Record the exact ordered-stream boundary. Runs synchronously inside the
 *  keeper result-frame dispatch, so every byte already delivered belongs to the
 *  old geometry and every later byte to the new one. */
export function markResizeBoundary(mgr: SessionManager, channelId: number, capture: ResizeCapture): void {
  if (capture.boundarySeq >= 0) return;
  const rec = mgr.sessions.get(channelId);
  capture.boundarySeq = rec?.head_seq ?? capture.installSeq;
  capture.boundaryAltMode = rec?.alt_mode ?? capture.boundaryAltMode;
}

/** Ingest one captured chunk: retain, scan, stage coordinator metadata — never
 *  the core. Returns the chunk's end_seq, or -1 when the channel is gone. */
export function captureUpstreamChunk(
  mgr: SessionManager,
  channelId: number,
  capture: ResizeCapture,
  chunk: Buffer,
): number {
  const endSeq = mgr.appendCapturedScrollback(channelId, chunk);
  if (endSeq < 0) return -1;
  capture.capturedBytes += chunk.byteLength;
  capture.capturedChunks++;
  const rec = mgr.sessions.get(channelId);
  if (rec) {
    const retainedStart = rec.head_seq - ringLength(rec.scrollback);
    const floor = capture.boundarySeq >= 0 ? capture.boundarySeq : capture.installSeq;
    if (retainedStart > floor) capture.ringEvicted = true;
  }
  return endSeq;
}

/** Clear the capture and its gate together, then let the emitter run. */
export function clearResizeCapture(mgr: SessionManager, channelId: number): void {
  mgr.resizeCaptures.delete(channelId);
  mgr.cellEmissionGates.delete(channelId);
  mgr.cellGateSuppression.delete(channelId);
}

/** Corruption signal for a gate that outlived the keeper command budget. Called
 *  from the emitter's suppression path, so it fires on the frame that is
 *  actually being withheld. */
export function noteGateOverBudget(mgr: SessionManager, channelId: number, ageMs: number): void {
  const capture = mgr.resizeCaptures.get(channelId);
  if (capture?.overBudget) return;
  if (capture) capture.overBudget = true;
  const rec = mgr.sessions.get(channelId);
  signal("terminal.gate_over_budget", {
    sid: String(rec?.sessionId ?? ""),
    channel_id: channelId,
    age_ms: Math.round(ageMs),
    budget_ms: CELL_GATE_BUDGET_MS,
    phase: capture?.phase ?? "none",
    reason: capture?.reason ?? "pending_repair",
    captured_bytes: capture?.capturedBytes ?? 0,
    cooldownKey: String(channelId),
  });
}

/** Build a fresh core at cols×rows and swap it in. The raw ring is the single
 *  source of truth, so the grid is a pure function of (ring, cols, rows) — no
 *  path-dependence, no asymmetric-resize drift.
 *
 *  With a capture, the retained window is split at the recorded boundary: the
 *  boundary core is reconstructed (or alt-primed from the boundary mode), its
 *  historical query replies are discarded, and the post-boundary tail is applied
 *  exactly once at the proven size with only ITS replies forwarded, FIFO. The
 *  swap tail is synchronous, so a chunk arriving mid-rebuild is either already in
 *  the ring being replayed or lands on the new core — never lost, never doubled.
 *
 *  Once-only: a capture rebuilds at most one core no matter how many paths
 *  (viewport claim, reap, reattach, ring rollover) reach this function. */
export async function rebuildTerminalCore(
  mgr: SessionManager,
  channelId: number,
  cols: number,
  rows: number,
  capture: ResizeCapture | null,
): Promise<boolean> {
  if (capture && capture.rebuilds > 0) return false;
  const rec0 = mgr.shellByChannel(channelId);
  if (!rec0?.wtermCore) return false;
  // Without a capture the core was never frozen, so an already-correct size
  // needs no reflow. With one, the captured tail has not been parsed anywhere
  // yet and the rebuild is the only path that applies it.
  if (!capture && rec0.wtermCore.getCols() === cols && rec0.wtermCore.getRows() === rows) return false;
  if (capture) capture.rebuilds++;
  const fresh = await _createWtermCore(cols, rows);
  const rec = mgr.shellByChannel(channelId);
  if (!rec) return false;

  const retained = readRing(rec.scrollback);
  const retainedStart = rec.head_seq - retained.length;
  let boundaryOffset = retained.length;
  if (capture) {
    const boundary = capture.boundarySeq >= 0 ? capture.boundarySeq : rec.head_seq;
    if (boundary < retainedStart) capture.ringEvicted = true;
    boundaryOffset = Math.max(0, Math.min(retained.length, boundary - retainedStart));
  }
  // A fresh core's VT parser starts COLD, so both retained-window cuts must be
  // token aligned. A pre-existing eviction is visible as retainedStart > 0:
  // advance past its orphan prefix before reconstructing either side of this
  // resize. This remains necessary on every later core rebuild because the raw
  // ring deliberately keeps those untrimmed bytes for sequence accounting.
  const replayStart = retainedStart > 0 ? skipOrphanSequencePrefix(retained) : 0;
  if (boundaryOffset < replayStart) boundaryOffset = replayStart;
  // The resize boundary is `head_seq` at the keeper's result frame and advances
  // by WHOLE PTY CHUNKS, so it can land between `ESC [` and `32m`. Unlike the
  // eviction cut, its opener is retained: rewind onto that opener, but never
  // behind the safe replay start established above.
  const aligned = Math.max(
    replayStart,
    rewindToSequenceStart(retained, boundaryOffset),
  );
  // `head` stops at `aligned`, not at the boundary: the rewound bytes move into
  // `prefix` and are replayed exactly once, from there, on BOTH paths — leaving
  // them in `head` too would double them on the non-alt path.
  const head = retained.subarray(replayStart, aligned);
  const prefix = retained.subarray(aligned, boundaryOffset);
  const tail = retained.subarray(boundaryOffset);
  // Alt-screen: never replay the ring at a new width. It holds absolute cursor
  // moves and line clears painted for the old geometry; replaying them at
  // another width duplicates and mangles rows. Start alt-primed and let the TUI
  // repaint after SIGWINCH.
  const altAtBoundary = capture ? capture.boundaryAltMode : rec.alt_mode;
  if (!altAtBoundary && head.length > 0) fresh.writeRaw(head);
  // Historical capability replies belong to a probe that was already answered by
  // the live path; only the captured tail's probes are still unanswered. The
  // core queues replies, so drain the whole queue rather than popping one.
  drainCoreReplies(fresh);
  // Prime BEFORE the tail, not after: the tail is the TUI's post-SIGWINCH
  // repaint, and painting it on the normal buffer and only then switching to the
  // alternate buffer would show an empty grid with the repaint hidden behind it.
  if (altAtBoundary && !fresh.usingAltScreen()) fresh.writeRaw(ALT_ENTER_SEQS[0]!);
  // Parser alignment, not content. `prefix` is the lead-in of the sequence the
  // boundary split (the `ESC [` of an `ESC [ 3 2 m`), and it is written on BOTH
  // paths: the alt path skips `head` deliberately, but a cold parser still needs
  // the opening bytes or the tail's remainder prints as text. It goes through
  // writeRaw and NEVER through answerQueries — these bytes were already
  // tokenized and answered by the live probe path, and re-tokenizing them here
  // would answer one probe TWICE on the same stdin lane. It also cannot queue a
  // native reply to leak into the tail's ordered replies: by construction
  // `prefix` is one INCOMPLETE sequence, which the core parks in its parser.
  if (prefix.length > 0) fresh.writeRaw(prefix);
  let reply = "";
  if (tail.length > 0) {
    // answerQueries' own carry starts empty, and that is a claim about the REPLY
    // lane, not the parser: a probe split across the boundary was answered live
    // (its lead-in is in `prefix`, deliberately not re-tokenized), and a probe
    // split across two captured chunks is whole in this one contiguous ring
    // range. The CORE's parser is NOT cold here — the prefix write left it
    // exactly mid-sequence where the boundary cut — so the tail's first bytes
    // complete a sequence. The live carry has been advancing over these same
    // bytes all along, so the session resumes aligned.
    reply = answerQueries({ query_carry: new Uint8Array(0) }, fresh, tail).bytes;
  }
  // The tail may itself enter or leave alt-screen; this reconciles the final mode
  // with the stream's current mode when the establishing sequence is no longer in
  // the retained window (L11 "stale text wallpaper after worker restart").
  if (rec.alt_mode && !fresh.usingAltScreen()) fresh.writeRaw(ALT_ENTER_SEQS[0]!);
  // The old core's monotonic total, read from ITS counters at the swap instant
  // instead of from rec.cell_emit.lastSbTotal. lastSbTotal only advances on a
  // SUCCESSFUL emit, and every gate that withholds one leaves it frozen while
  // the core keeps appending underneath: a synchronized-output hold (up to
  // SYNC_OUTPUT_MAX_PENDING_ROWS lines / SYNC_OUTPUT_MAX_MS of throughput), a
  // pending repair, a dropped send, this transaction's own capture. Pinning off
  // the frozen value understates the old core by exactly that gap, which makes
  // the clamp below fire on margins that were never negative.
  const prevCore = rec.wtermCore;
  const prevDropped = scrollbackOrigin(prevCore, rec.cell_emit);
  const prevRetained = prevCore.getScrollbackCount();
  const prevMonoTotal = prevDropped + prevRetained;
  rec.wtermCore = fresh;
  // Fresh core, fresh (empty) debug ring: the mark taken against the old core's
  // ring would suppress this one's first entries. The replay re-feeds the same
  // bytes, so a sequence this core also fails to handle is reported once more —
  // which is correct, it is a different core's report.
  resetUnhandledSequences(rec);
  // A hold belongs to the core it was opened against: its generation counter and
  // its sbTotalAtOpen row ceiling are both that instance's numbers. The swap is
  // where those stop meaning anything, and it is reached by paths that never
  // installed a capture, so the retirement is here and not only at install.
  mgr._releaseSyncOutputHold(channelId);
  // Fresh core, fresh ring: the core's OWN discarded counter restarts at 0, so
  // Roost's monotonic origin must absorb the whole difference or every index the
  // SPA holds re-aliases and scrollbackTotal REWINDS (which parks the backfill
  // controller).
  //
  // INVARIANT (what the pin does and does not claim):
  //   sbOrigin + freshDiscarded + freshCount == prevMonoTotal
  // buys exactly one property — the monotonic total does not rewind, so no index
  // the SPA already holds is re-issued for a different line. It is NOT the claim
  // that a given index names the SAME line in both cores: replaying at a new
  // width reflows, and with a capture the fresh core also parses a tail the old
  // core never saw, so the two rings hold different line sets at different
  // widths. That is why every rebuild mints a new gridEpoch below, and why every
  // consumer of an absolute index revalidates against it.
  //
  // freshDiscarded is a term and not an assumed zero: a retained window deeper
  // than the core's line capacity makes the fresh core discard during its own
  // replay.
  const freshEmit = initCellEmitState(newTraceId());
  const freshDiscarded = scrollbackOrigin(fresh, freshEmit);
  const freshCount = fresh.getScrollbackCount();
  // The clamp is the one case with no honest origin: the replay produced MORE
  // lines than the browser's watermark covers, so every non-negative sbOrigin
  // leaves the total ABOVE prevMonoTotal. 0 is then the truthful floor — the
  // fresh core's own counters, unshifted — and the total grows rather than
  // rewinds.
  const pinned = prevMonoTotal - freshDiscarded - freshCount;
  const originClamped = pinned < 0;
  const sbOrigin = originClamped ? 0 : pinned;
  rec.cell_emit = {
    ...freshEmit,
    // seq is kept so the SPA's gap detector sees no rewind. sentFull stays false
    // (cols/rows/alt zeroed with it), so the next emit is a full frame however it
    // is triggered — a delta is meaningless across a core swap.
    seq: rec.cell_emit.seq,
    sbOrigin,
    sbDropped: sbOrigin + freshDiscarded,
    // Exactly what the authoritative frame that follows this rebuild would set,
    // so the watermark is already correct if that send is DROPPED. Left at
    // freshEmit's 0, a second rebuild before the next successful emit pinned
    // against 0 and collapsed the total to the fresh core's bare retained depth.
    lastSbTotal: sbOrigin + freshDiscarded + freshCount,
  };
  mgr.coreRebuilds.set(channelId, (mgr.coreRebuilds.get(channelId) ?? 0) + 1);
  if (reply.length > 0 && capture) {
    capture.forwardedReplies++;
    // Ordered behind any keeper write already queued for this channel, so a
    // deferred reply can never overtake the resize it belongs after.
    void withKeeperAdmission(mgr, channelId, "query_reply", () => {
      getMultiplexedPool().input(channelId, new TextEncoder().encode(reply));
    });
  }
  // The pin's own record. This is the one moment sbOrigin is DERIVED rather than
  // merely advanced, and the only moment history can vanish for a reason other
  // than eviction — so it is also the only moment worth recording. Overwrites the
  // previous record in place: the snapshot samples it O(1) and nothing grows.
  //
  // replay_lost_rows is the honest measure of "lost to the replay bound": rows the
  // OLD core still held that the fresh one does not, because the replay reads a
  // fixed BYTE ring while the core evicts by LINES. One-sided by construction — a
  // fresh core deeper than the retained window RECOVERS history and moves the
  // floor down instead, which is not a loss.
  const newDropped = sbOrigin + freshDiscarded;
  const replayLostRows = Math.max(0, newDropped - prevDropped);
  // Carried across pins: a later rebuild that loses nothing must not erase an
  // earlier one's mark, or a floor still owned by that replay would then be
  // reported as ordinary eviction.
  const prevReplayFloor = rec.sb_origin_pin?.replay_floor ?? 0;
  rec.sb_origin_pin = {
    at_mono_ms: monoNowMs(),
    cols,
    rows,
    replayed_ring: !altAtBoundary,
    ring_evicted: capture?.ringEvicted ?? false,
    prev_dropped: prevDropped,
    prev_total: prevMonoTotal,
    fresh_discarded: freshDiscarded,
    fresh_count: freshCount,
    sb_origin: sbOrigin,
    sb_dropped: newDropped,
    clamped: originClamped,
    replay_lost_rows: replayLostRows,
    replay_floor: Math.max(prevReplayFloor, replayLostRows > 0 ? newDropped : 0),
  };
  diag("resize.wterm_core", {
    sid: rec.sessionId,
    channel_id: channelId,
    session_trace_id: rec.session_trace_id,
    cols,
    rows,
    mode: altAtBoundary ? "empty_alt_primed" : "rebuild_from_ring",
    replayed_ring: !altAtBoundary,
    ring_bytes: retained.length,
    boundary_seq: capture?.boundarySeq ?? null,
    // Pre-boundary bytes, which is `head` PLUS the parser-alignment `prefix`
    // carved off its end — the split is a replay detail, the boundary is not.
    boundary_bytes: head.length + prefix.length,
    // Bytes the boundary was rewound onto a sequence start by, or 0 when it
    // already landed on one. Non-zero means this rebuild would have printed a
    // sequence remnant as literal text before the alignment landed.
    align_rewind_bytes: prefix.length,
    // Bytes dropped off the front of an evicted ring because no parser context
    // for them survives. A real (bounded) loss, so it is recorded rather than
    // folded silently into ring_evicted.
    orphan_skipped_bytes: replayStart,
    tail_bytes: tail.length,
    ring_evicted: capture?.ringEvicted ?? false,
    captured_bytes: capture?.capturedBytes ?? 0,
    forwarded_reply_bytes: reply.length,
    // The pin, in full. Without these the numbers a mis-spliced history was
    // derived from are unrecoverable after the fact.
    prev_dropped: prevDropped,
    prev_total: prevMonoTotal,
    fresh_discarded: freshDiscarded,
    fresh_count: freshCount,
    sb_origin: sbOrigin,
    sb_dropped: newDropped,
    origin_clamped: originClamped,
    replay_lost_rows: replayLostRows,
  });
  // Tier-1: the rebuild SUCCEEDED and still did not preserve history truth. Both
  // arms are the L11 "history shrank / mis-spliced after a resize" class, and both
  // are invisible from anywhere else — the fresh grid is perfectly well-formed,
  // just shallower or renumbered than the one the browser is holding.
  if (originClamped || replayLostRows > 0) {
    signal("scrollback.replay_bound", {
      sid: rec.sessionId,
      channel_id: channelId,
      session_trace_id: rec.session_trace_id,
      cols,
      rows,
      replayed_ring: !altAtBoundary,
      ring_bytes: retained.length,
      ring_evicted: capture?.ringEvicted ?? false,
      prev_dropped: prevDropped,
      prev_total: prevMonoTotal,
      fresh_discarded: freshDiscarded,
      fresh_count: freshCount,
      sb_origin: sbOrigin,
      sb_dropped: newDropped,
      origin_clamped: originClamped,
      replay_lost_rows: replayLostRows,
    });
  }
  return true;
}
