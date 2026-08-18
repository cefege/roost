// Deterministic differential trace oracle for the PINNED terminal core
// (@wterm/core 0.3.4 + apps/shared/wasm/wterm-roost.wasm). Plan section 7:
// replay a recorded chat/TUI byte trace through the real
// Roost cell pipeline at the RECORDED PTY chunk boundaries and prove, at every
// step, that folding the emitted frames reproduces an all-row snapshot of the
// core's grid. That separates parser/model faults from the browser/claim stalls
// the earlier plan sections repair.
//
// Five lanes, each with its own ground truth:
//
//   fold       Roost-owned. nextCellFrame() emits full/delta frames, applyDelta()
//              folds them, and the fold must equal gridToCellFrame() read fresh
//              off the same core. Only frame COMPOSITION is under test, so both
//              sides deliberately share rowToSpans() as the cell encoder.
//   scrollback Roost-owned. The emitter reads the ring's eviction origin from
//              0.3.4's getScrollbackDiscardedCount() (emitter.ts::
//              scrollbackOrigin). Ground truth comes from the trace itself:
//              every append-only line carries a globally unique `#<ordinal>`, so
//              the oldest/newest retained line names the true origin and total,
//              independently of anything the core reports.
//   document   Core-owned. Read top to bottom, history++viewport ordinals must
//              strictly increase: append-only output may never reorder, duplicate
//              or interleave the document. Armed only between `doc on`/`doc off`,
//              because cursor-addressed redraws rewrite rows in place by design.
//   reply      Split ownership. A second core instance is fed the identical byte
//              stream in small pieces cut exactly at query-token ends, draining
//              getResponse() after every piece. That recovers the replies the
//              stream is OWED, in stream order, without trusting the per-chunk
//              drain under test.
//   boundary   Core-owned. The same reference instance also proves chunk-boundary
//              independence: bytes re-cut away from the recorded PTY boundaries
//              must still produce an identical grid.
//
// Everything here is clock-free: identical input bytes and identical chunk
// boundaries produce identical output on every run and every platform.

import type { CellData, TerminalCore } from "@wterm/core";
import {
  applyDelta,
  gridToCellFrame,
  initCellEmitState,
  nextCellFrame,
  readScrollbackRangeCells,
  rowToSpans,
  SB_SNAPSHOT_HISTORY_ROWS,
  spanIsAtomic,
  type CellEmitState,
  type CellGridFrame,
  type CellRow,
  type CellSpan,
} from "../src/cell/index.ts";
// Nothing below may derive the eviction origin the way the emitter does — no
// capacity table, no tail probe, no core counter — only the trace's ordinals.
//
// The worker owns both the reply TEXT it synthesizes and the ordered lane that
// interleaves it with the core's own replies. The oracle drives that exact code
// rather than a copy, so a drift in either is visible here; the tokenizer that
// decides what a chunk OWES stays independent below (test-only cross-app
// import, as in apps/worker/tests/coord-target.test.ts).
import {
  answerQueries,
  PRIMARY_DA_REPLY,
  XTVERSION_REPLY,
  type QueryCarry,
} from "../../worker/src/terminal-query-reply.ts";

// ---------------------------------------------------------------------------
// Trace program
// ---------------------------------------------------------------------------

export interface TraceWriteStep {
  kind: "write";
  bytes: Uint8Array;
  traceLine: number;
}
export interface TraceResizeStep {
  kind: "resize";
  cols: number;
  rows: number;
  traceLine: number;
}
/** A fresh viewer attaches: the worker forces one authoritative full frame. */
export interface TraceAttachStep {
  kind: "attach";
  traceLine: number;
}
/** Deep checkpoint: compare the complete retained ring, not only its tail. */
export interface TraceSweepStep {
  kind: "sweep";
  label: string;
  traceLine: number;
}
/** Arm/disarm the append-only document invariant around cursor-addressed draws. */
export interface TraceDocStep {
  kind: "doc";
  on: boolean;
  traceLine: number;
}

export type TraceStep =
  | TraceWriteStep
  | TraceResizeStep
  | TraceAttachStep
  | TraceSweepStep
  | TraceDocStep;

export interface TraceProgram {
  cols: number;
  rows: number;
  steps: TraceStep[];
  /** Total `{i}` expansions: how many uniquely marked lines the trace emits. */
  ordinals: number;
}

/** Marker every append-only trace line carries: `#<ordinal>` at a low column so
 *  a horizontal shrink cannot clip it. Parsed back out of painted rows to give
 *  the scrollback and document lanes a ground truth the emitter cannot fake. */
const ORDINAL_RE = /#(\d+)/;

function unescapeTraceBytes(src: string, traceLine: number): Uint8Array {
  const out: number[] = [];
  const encoder = new TextEncoder();
  let literal = "";
  const flush = (): void => {
    if (literal === "") return;
    for (const byte of encoder.encode(literal)) out.push(byte);
    literal = "";
  };
  for (let i = 0; i < src.length; i++) {
    const ch = src[i]!;
    if (ch !== "\\") {
      literal += ch;
      continue;
    }
    flush();
    const esc = src[++i];
    switch (esc) {
      case "e": out.push(0x1b); break;
      case "r": out.push(0x0d); break;
      case "n": out.push(0x0a); break;
      case "t": out.push(0x09); break;
      case "a": out.push(0x07); break;
      case "s": out.push(0x20); break;
      case "0": out.push(0x00); break;
      case "\\": out.push(0x5c); break;
      case "x": {
        const hex = src.slice(i + 1, i + 3);
        if (!/^[0-9a-fA-F]{2}$/.test(hex)) {
          throw new Error(`trace line ${traceLine}: bad \\x escape "${hex}"`);
        }
        out.push(Number.parseInt(hex, 16));
        i += 2;
        break;
      }
      default:
        throw new Error(`trace line ${traceLine}: unknown escape "\\${esc ?? ""}"`);
    }
  }
  flush();
  return Uint8Array.from(out);
}

/** Parse the committed `.trace` program. Directives:
 *
 *    init <cols> <rows>              grid the replay starts at (once, first)
 *    w <payload>                     ONE PTY chunk, boundary preserved exactly
 *    wrep <chunks> <perChunk> <pay>  `chunks` chunks of `perChunk` payloads each
 *    resize <cols> <rows>            terminal resize between chunks
 *    attach                          fresh viewer -> forced full frame
 *    sweep <label>                   deep whole-ring checkpoint
 *    doc on|off                      arm/disarm the append-only invariant
 *
 *  `{i}` anywhere in a payload expands to the next global ordinal, so every
 *  marked line in the whole trace is unique and strictly increasing. Payload
 *  escapes: \e \r \n \t \a \s \0 \\ \xNN; any other character is written as its
 *  UTF-8 bytes. */
export function parseTrace(text: string): TraceProgram {
  const steps: TraceStep[] = [];
  let cols = 0;
  let rows = 0;
  let ordinal = 0;
  const lines = text.split("\n");

  const expand = (payload: string, traceLine: number): Uint8Array => {
    let expanded = payload;
    while (expanded.includes("{i}")) expanded = expanded.replace("{i}", String(ordinal++));
    return unescapeTraceBytes(expanded, traceLine);
  };

  for (let n = 0; n < lines.length; n++) {
    const traceLine = n + 1;
    const raw = lines[n]!;
    if (raw === "" || raw.startsWith("#")) continue;
    const sep = raw.indexOf(" ");
    const verb = sep === -1 ? raw : raw.slice(0, sep);
    const rest = sep === -1 ? "" : raw.slice(sep + 1);
    switch (verb) {
      case "init": {
        const [c, r] = rest.split(" ");
        cols = Number(c);
        rows = Number(r);
        if (!Number.isInteger(cols) || !Number.isInteger(rows) || cols <= 0 || rows <= 0) {
          throw new Error(`trace line ${traceLine}: bad init "${rest}"`);
        }
        break;
      }
      case "w":
        steps.push({ kind: "write", bytes: expand(rest, traceLine), traceLine });
        break;
      case "wrep": {
        const firstGap = rest.indexOf(" ");
        const secondGap = rest.indexOf(" ", firstGap + 1);
        const chunks = Number(rest.slice(0, firstGap));
        const perChunk = Number(rest.slice(firstGap + 1, secondGap));
        const payload = rest.slice(secondGap + 1);
        if (!Number.isInteger(chunks) || !Number.isInteger(perChunk) || chunks <= 0 || perChunk <= 0) {
          throw new Error(`trace line ${traceLine}: bad wrep counts "${rest}"`);
        }
        for (let c = 0; c < chunks; c++) {
          const parts: Uint8Array[] = new Array(perChunk);
          let size = 0;
          for (let p = 0; p < perChunk; p++) {
            parts[p] = expand(payload, traceLine);
            size += parts[p]!.length;
          }
          const chunk = new Uint8Array(size);
          let at = 0;
          for (const part of parts) {
            chunk.set(part, at);
            at += part.length;
          }
          steps.push({ kind: "write", bytes: chunk, traceLine });
        }
        break;
      }
      case "resize": {
        const [c, r] = rest.split(" ");
        steps.push({ kind: "resize", cols: Number(c), rows: Number(r), traceLine });
        break;
      }
      case "attach":
        steps.push({ kind: "attach", traceLine });
        break;
      case "sweep":
        steps.push({ kind: "sweep", label: rest, traceLine });
        break;
      case "doc":
        steps.push({ kind: "doc", on: rest === "on", traceLine });
        break;
      default:
        throw new Error(`trace line ${traceLine}: unknown directive "${verb}"`);
    }
  }
  if (cols === 0 || rows === 0) throw new Error("trace is missing its `init <cols> <rows>` header");
  return { cols, rows, steps, ordinals: ordinal };
}

// ---------------------------------------------------------------------------
// Query tokenizer — the ordering contract the reply lane holds the stack to
// ---------------------------------------------------------------------------

const ESC = 0x1b;
const LBRACKET = 0x5b;

export type QueryKind = "cpr" | "da" | "xtversion";
interface QueryToken {
  kind: QueryKind;
  /** Absolute offsets into the whole trace byte stream. */
  start: number;
  end: number;
}

/** Longest query token is ESC [ > 0 q, so carrying four bytes across a PTY chunk
 *  boundary recognises a split token exactly once. */
const QUERY_CARRY = 4;

/** Every capability probe in `buf`, whose bytes start at absolute `base`, that
 *  COMPLETES after `minEnd` — a token already reported from the previous chunk's
 *  carry ends at or before it. */
function scanQueries(buf: Uint8Array, base: number, minEnd: number): QueryToken[] {
  const found: QueryToken[] = [];
  for (let i = 0; i + 2 < buf.length; i++) {
    if (buf[i] !== ESC || buf[i + 1] !== LBRACKET) continue;
    const c2 = buf[i + 2];
    let kind: QueryKind | null = null;
    let len = 0;
    if (c2 === 0x36 /* 6 */ && buf[i + 3] === 0x6e /* n */) { kind = "cpr"; len = 4; }
    else if (c2 === 0x63 /* c */) { kind = "da"; len = 3; }
    else if (c2 === 0x30 /* 0 */ && buf[i + 3] === 0x63 /* c */) { kind = "da"; len = 4; }
    else if (c2 === 0x3e /* > */ && buf[i + 3] === 0x30 && buf[i + 4] === 0x71 /* q */) { kind = "xtversion"; len = 5; }
    if (kind === null) continue;
    const end = base + i + len;
    if (end > minEnd) found.push({ kind, start: base + i, end });
    i += len - 1;
  }
  return found;
}

// ---------------------------------------------------------------------------
// Hashing and row text
// ---------------------------------------------------------------------------

const FNV_OFFSET = 2_166_136_261;
const FNV_PRIME = 16_777_619;

function mix(hash: number, value: number): number {
  return Math.imul(hash ^ value, FNV_PRIME);
}

/** Style-aware row hash: text codepoints plus every style field, so a
 *  colour-only divergence is exactly as loud as a text one. */
export function rowHash(spans: readonly CellSpan[]): string {
  let h = FNV_OFFSET;
  for (const span of spans) {
    for (const ch of span.text) h = mix(h, ch.codePointAt(0)!);
    h = mix(h, span.fg);
    h = mix(h, span.bg);
    h = mix(h, span.flags);
    h = mix(h, span.fgRgb ?? -1);
    h = mix(h, span.bgRgb ?? -1);
    h = mix(h, 0x1f);
  }
  return (h >>> 0).toString(16).padStart(8, "0");
}

export function gridHash(rows: readonly CellRow[]): string {
  let h = FNV_OFFSET;
  for (const row of rows) {
    h = mix(h, row.index);
    for (const ch of rowHash(row.spans)) h = mix(h, ch.charCodeAt(0));
  }
  return (h >>> 0).toString(16).padStart(8, "0");
}

/** Content-only digest of the newest history rows. Index-free on purpose: the
 *  reference core numbers its ring from zero while the primary has evicted. */
function historyTailHash(rows: readonly CellRow[], count: number): string {
  let h = FNV_OFFSET;
  for (const row of rows.slice(Math.max(0, rows.length - count))) {
    for (const ch of rowHash(row.spans)) h = mix(h, ch.charCodeAt(0));
  }
  return (h >>> 0).toString(16).padStart(8, "0");
}

function rowText(spans: readonly CellSpan[]): string {
  let text = "";
  for (const span of spans) text += span.text;
  return text;
}

/** Continuation column of a wide glyph. Deliberately not a codepoint the wide
 *  probe below could mistake for a lead. */
const CONTINUATION_TOKEN = "~cont";

/** One printable token per GRID COLUMN, so a row divergence can name a column.
 *  A span is not one codepoint per column: a wide glyph is one atomic span over
 *  two columns (its continuation column repeats as CONTINUATION_TOKEN) and an
 *  astral codepoint is two code units in one. */
function rowCells(spans: readonly CellSpan[]): string[] {
  const cells: string[] = [];
  for (const span of spans) {
    const style = `${span.fg}/${span.bg}/${span.flags}/${span.fgRgb ?? "-"}/${span.bgRgb ?? "-"}`;
    if (spanIsAtomic(span)) {
      cells.push(`${span.text}/${style}`);
      for (let extra = 1; extra < span.columns; extra++) cells.push(`${CONTINUATION_TOKEN}/${style}`);
      continue;
    }
    for (const ch of span.text) cells.push(`${ch}/${style}`);
  }
  return cells;
}

function firstDifferentCell(expected: readonly CellSpan[], actual: readonly CellSpan[]): number | null {
  const a = rowCells(expected);
  const b = rowCells(actual);
  const n = Math.max(a.length, b.length);
  for (let i = 0; i < n; i++) if (a[i] !== b[i]) return i;
  return null;
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

export type TraceLane = "fold" | "scrollback" | "document" | "reply" | "boundary";

export interface TraceDivergence {
  lane: TraceLane;
  /** Stable identifier for the failure mode, safe to grep and to gate on. */
  id: string;
  /** Index into TraceProgram.steps. */
  stepIndex: number;
  /** Directive that produced the step, so a resize/attach record is never read
   *  as if the byte offsets below were the bytes that broke it. */
  stepKind: TraceStep["kind"];
  /** Index among WRITE steps only, i.e. the PTY chunk ordinal. */
  chunkIndex: number;
  /** Absolute offset of the LAST chunk's first byte in the whole trace stream. */
  byteOffset: number;
  /** Length of that chunk, i.e. the exact recorded PTY boundary. */
  chunkBytes: number;
  /** 1-based line in the fixture that produced the step. */
  traceLine: number;
  row: number | null;
  col: number | null;
  expected: string;
  actual: string;
  detail: string;
}

export interface TraceCoverage {
  writeChunks: number;
  bytes: number;
  resizes: number;
  dimensions: string[];
  epochs: string[];
  epochAdvances: number;
  framesFull: number;
  framesDelta: number;
  deltaRowsTotal: number;
  deltaRowsMax: number;
  distinctGridHashes: number;
  nativeQueries: number;
  synthQueries: number;
  splitQueries: number;
  syncBoundaries: number;
  altTransitions: number;
  wideCells: number;
  graphemeCells: number;
  astralCells: number;
  scrollbackMax: number;
  scrollbackDiscarded: number;
  /** Steps at which the ring had ACTUALLY discarded lines, not merely filled. */
  ringEvictingSteps: number;
  /** Eviction origin at each named `sweep`, so the trail is pinnable by point
   *  rather than only at the end of the run. */
  discardedAtSweep: Record<string, number>;
  attachFullFrames: number;
  sweeps: number;
  cursorHiddenSteps: number;
  cursorKeysAppSteps: number;
  bracketedPasteSteps: number;
}

export interface RefreshObservation {
  stepIndex: number;
  scrollbackTotal: number;
  /** History rows the authoritative full frame itself carried. */
  fullFrameHistoryRows: number;
  /** History rows still addressable through the explicit backfill read. */
  backfillRows: number;
  recovers: "future-only" | "accumulated";
}

export interface CoreObservations {
  scrollbackCapacity: number;
  /** Does a width-2 codepoint own a continuation cell, or only its lead cell? */
  wideCellModel: "lead-only" | "lead-plus-continuation" | "unobserved";
  /** Can one chunk holding several probes yield several replies? */
  queryReplyModel: "single-slot-last-wins" | "ordered-drain" | "unobserved";
  refreshes: RefreshObservation[];
}

export interface TraceReport {
  steps: number;
  coverage: TraceCoverage;
  observations: CoreObservations;
  divergences: TraceDivergence[];
  /** Divergences actually seen per lane, including any past the per-lane cap. */
  divergenceCounts: Record<TraceLane, number>;
  /** Set when a lane whose state cannot survive its own failure stopped the run. */
  abortedAtStep: number | null;
  finalGridHash: string;
}

export function formatDivergence(d: TraceDivergence): string {
  return [
    `${d.lane} divergence [${d.id}]`,
    `  step        ${d.stepIndex} ${d.stepKind}  (fixture line ${d.traceLine})`,
    `  pty chunk   #${d.chunkIndex}  bytes [${d.byteOffset}, ${d.byteOffset + d.chunkBytes}) len ${d.chunkBytes}`,
    `  cell        row ${d.row ?? "-"} col ${d.col ?? "-"}`,
    `  expected    ${d.expected}`,
    `  actual      ${d.actual}`,
    `  detail      ${d.detail}`,
  ].join("\n");
}

/** The gate record: the first divergence whose lane OR id is `select`, rendered
 *  in full, or `"<select>: clean"`. Test assertions compare against the clean
 *  string so a failure prints the whole record instead of a boolean. */
export function reportOn(report: TraceReport, select: TraceLane | string): string {
  const hits = report.divergences.filter((d) => d.lane === select || d.id === select);
  if (hits.length === 0) return `${select}: clean`;
  const rest = hits.length - 1;
  return `${formatDivergence(hits[0]!)}${rest === 0 ? "" : `\n  (+${rest} more matching ${select})`}`;
}

// ---------------------------------------------------------------------------
// Replay
// ---------------------------------------------------------------------------

export interface ReplayOptions {
  /** Production factory, so the oracle runs against the core the worker loads. */
  createCore: (cols: number, rows: number) => Promise<TerminalCore>;
}

/** Reference-core write granularity. Prime and small, so escape sequences, UTF-8
 *  sequences and query tokens are all re-cut away from the recorded PTY
 *  boundaries; pieces still end exactly at every query token. */
const REF_PIECE = 7;
/** Retained lines re-read from the ring on every step. A `sweep` pays for the
 *  whole ring instead. */
const SB_TAIL_CHECK = 8;
/** History rows joined to the viewport for the per-step document check. */
const DOC_TAIL_ROWS = 8;
/** History rows compared between the primary and the re-cut reference core. */
const BOUNDARY_TAIL_ROWS = 64;
/** Per-lane divergence cap: enough to characterise, bounded enough to read. */
const MAX_PER_LANE = 6;

const SYNC_ON = "\x1b[?2026h";
const SYNC_OFF = "\x1b[?2026l";

function isCombining(cp: number): boolean {
  return (cp >= 0x0300 && cp <= 0x036f) || (cp >= 0x1ab0 && cp <= 0x1aff) || (cp >= 0x20d0 && cp <= 0x20f0);
}

/** Deliberately minimal: the width-2 ranges this fixture actually paints. A
 *  general table would be unpinned surface the oracle never drives. */
function isWide(cp: number): boolean {
  return (cp >= 0x1100 && cp <= 0x115f)
    || (cp >= 0x2e80 && cp <= 0x303e)
    || (cp >= 0x3041 && cp <= 0x33ff)
    || (cp >= 0x4e00 && cp <= 0x9fff)
    || (cp >= 0xac00 && cp <= 0xd7a3)
    || (cp >= 0xff00 && cp <= 0xff60)
    || (cp >= 0x1f300 && cp <= 0x1f9ff);
}

function scrollbackLineText(core: TerminalCore, offset: number): string {
  const len = core.getScrollbackLineLen(offset);
  const cells: CellData[] = new Array(len);
  for (let col = 0; col < len; col++) cells[col] = core.getScrollbackCell(offset, col);
  return rowText(rowToSpans(cells));
}

function ordinalOf(text: string): number | null {
  const m = ORDINAL_RE.exec(text);
  return m === null ? null : Number(m[1]);
}

function countOccurrences(text: string, needle: string): number {
  let seen = 0;
  let at = text.indexOf(needle);
  while (at !== -1) {
    seen++;
    at = text.indexOf(needle, at + needle.length);
  }
  return seen;
}

/** How many of the owed reply `segments` the pty actually received, consuming
 *  `actual` left to right in any order. Reply segments have distinct prefixes
 *  (CSI..c, CSI..R, DCS..ST), so greedy matching is exact. -1 when `actual`
 *  carries bytes no owed segment explains. */
function countOwedSegments(actual: string, segments: readonly string[]): number {
  const remaining = [...segments];
  let at = 0;
  let matched = 0;
  while (at < actual.length) {
    const hit = remaining.findIndex((s) => s.length > 0 && actual.startsWith(s, at));
    if (hit === -1) return -1;
    at += remaining[hit]!.length;
    remaining.splice(hit, 1);
    matched++;
  }
  return matched;
}

export async function replayTrace(program: TraceProgram, opts: ReplayOptions): Promise<TraceReport> {
  const core = await opts.createCore(program.cols, program.rows);
  const ref = await opts.createCore(program.cols, program.rows);
  const decoder = new TextDecoder();

  let emit: CellEmitState = initCellEmitState("trace");
  // The one browser-side fold under test. Boxed because every writer is a
  // closure below, and a bare `let` would let control-flow analysis conclude the
  // frame is still the initial null after the replay loop.
  const client: { held: CellGridFrame | null } = { held: null };

  const divergences: TraceDivergence[] = [];
  const laneCount = new Map<TraceLane, number>();
  const lastSignature = new Map<TraceLane, string>();
  const laneKept = new Map<TraceLane, number>();
  const refreshes: RefreshObservation[] = [];
  const dimensions = new Set<string>();
  const epochs = new Set<string>();
  const gridHashes = new Set<string>();

  const coverage: TraceCoverage = {
    writeChunks: 0, bytes: 0, resizes: 0, dimensions: [], epochs: [], epochAdvances: 0,
    framesFull: 0, framesDelta: 0, deltaRowsTotal: 0, deltaRowsMax: 0, distinctGridHashes: 0,
    nativeQueries: 0, synthQueries: 0, splitQueries: 0, syncBoundaries: 0, altTransitions: 0,
    wideCells: 0, graphemeCells: 0, astralCells: 0,
    scrollbackMax: 0, scrollbackDiscarded: 0, ringEvictingSteps: 0, discardedAtSweep: {},
    attachFullFrames: 0, sweeps: 0,
    cursorHiddenSteps: 0, cursorKeysAppSteps: 0, bracketedPasteSteps: 0,
  };
  let wideCellModel: CoreObservations["wideCellModel"] = "unobserved";
  let queryReplyModel: CoreObservations["queryReplyModel"] = "unobserved";

  // Per-step context, so every divergence can name its exact chunk boundary.
  let stepIndex = 0;
  let chunkIndex = -1;
  let byteOffset = 0;
  let chunkBytes = 0;
  let traceLine = 0;
  let stepKind: TraceStep["kind"] = "write";
  let aborted: number | null = null;
  let docArmed = false;
  let lastAlt = false;

  // Reference lane.
  let carry = new Uint8Array(0);
  let streamOffset = 0;
  // Replies the current chunk owes, one segment per probe, in stream order;
  // `expectedNatives` is the core-produced subset, which is what decides whether
  // several probes in one chunk can yield several replies at all.
  const expectedSegments: string[] = [];
  const expectedNatives: string[] = [];
  // The worker's own per-session tokenizer carry, so the replay drives the live
  // path's cross-chunk state exactly as one session would.
  const replyCarry: QueryCarry = { query_carry: new Uint8Array(0) };

  // Scrollback ground truth. The core's discarded counter is exact, so while it
  // reads zero nothing has been evicted and the newest retained line sits at
  // monotonic index retained-1 — an exact, free anchor. `floorOrdinal` is the
  // oldest ordinal from which the one-line-per-ordinal relation has held
  // continuously: chat output pushes unmarked and viewport-only lines, so the
  // relation only becomes usable once the trace is emitting a marked line per
  // scrolled row.
  let anchorOrdinal = -1;
  let anchorMono = -1;
  let floorOrdinal = -1;

  const record = (
    lane: TraceLane,
    id: string,
    detail: string,
    expected: string,
    actual: string,
    row: number | null = null,
    col: number | null = null,
  ): void => {
    laneCount.set(lane, (laneCount.get(lane) ?? 0) + 1);
    // A broken invariant stays broken until the offending rows leave the window.
    // Keep the distinct records and merely count the echoes, so the first record
    // — the one that names the fault — is never buried by its own aftershocks.
    const signature = `${id}\u0000${expected}\u0000${actual}`;
    const echo = lastSignature.get(lane) === signature;
    lastSignature.set(lane, signature);
    const kept = laneKept.get(lane) ?? 0;
    if (echo || kept >= MAX_PER_LANE) return;
    laneKept.set(lane, kept + 1);
    divergences.push({
      lane, id, stepIndex, stepKind, chunkIndex, byteOffset, chunkBytes, traceLine,
      row, col, expected, actual, detail,
    });
  };

  const writeReference = (slice: Uint8Array): void => {
    for (let at = 0; at < slice.length; at += REF_PIECE) {
      ref.writeRaw(slice.subarray(at, Math.min(at + REF_PIECE, slice.length)));
      const reply = ref.getResponse();
      if (reply === null) continue;
      expectedSegments.push(reply);
      expectedNatives.push(reply);
    }
  };

  /** Feed the reference core this chunk re-cut at query boundaries, collecting
   *  the replies the stream is owed as ordered segments. */
  const referenceChunk = (bytes: Uint8Array): void => {
    const combined = new Uint8Array(carry.length + bytes.length);
    combined.set(carry, 0);
    combined.set(bytes, carry.length);
    const base = streamOffset - carry.length;
    let cursor = 0;
    for (const token of scanQueries(combined, base, streamOffset)) {
      if (token.start < streamOffset) coverage.splitQueries++;
      const end = Math.max(token.end - streamOffset, 0);
      writeReference(bytes.subarray(cursor, end));
      cursor = end;
      if (token.kind === "cpr") {
        coverage.nativeQueries++;
      } else {
        coverage.synthQueries++;
        expectedSegments.push(token.kind === "da" ? PRIMARY_DA_REPLY : XTVERSION_REPLY);
      }
    }
    writeReference(bytes.subarray(cursor));
    carry = combined.slice(Math.max(0, combined.length - QUERY_CARRY));
    streamOffset += bytes.length;
  };

  const countContentCoverage = (rows: readonly CellRow[]): void => {
    for (const row of rows) {
      for (const span of row.spans) {
        for (const ch of span.text) {
          const cp = ch.codePointAt(0)!;
          if (cp > 0xffff) coverage.astralCells++;
          if (isCombining(cp)) coverage.graphemeCells++;
          if (isWide(cp)) coverage.wideCells++;
        }
      }
    }
  };

  /** Whether a width-2 codepoint owns a continuation cell decides every column
   *  computation downstream, so pin it the first time the trace paints one. */
  const observeWideModel = (rows: readonly CellRow[], cols: number): void => {
    if (wideCellModel !== "unobserved") return;
    for (const row of rows) {
      const cells = rowCells(row.spans);
      const limit = Math.min(cells.length, cols) - 1;
      for (let col = 0; col < limit; col++) {
        if (!isWide(cells[col]!.codePointAt(0)!)) continue;
        wideCellModel = core.getCell(row.index, col + 1).char === 0 ? "lead-plus-continuation" : "lead-only";
        return;
      }
    }
  };

  /** history(tail) ++ viewport must read as one strictly increasing document. */
  const checkDocument = (snapshot: CellGridFrame, historyRows: readonly CellRow[]): void => {
    if (!docArmed || snapshot.altScreen) return;
    let previous = -1;
    let previousRow = -1;
    for (const [label, rows] of [["history", historyRows], ["viewport", snapshot.viewportRows]] as const) {
      for (const row of rows) {
        const ordinal = ordinalOf(rowText(row.spans));
        if (ordinal === null) continue;
        if (ordinal <= previous) {
          record(
            "document",
            "document-order-broken",
            `${label} row ${row.index} carries #${ordinal} after #${previous} (row ${previousRow}): `
            + "history++viewport is no longer the append-only document the stream wrote",
            `#${ordinal} > #${previous}`,
            `#${ordinal} <= #${previous}`,
            row.index,
          );
          return;
        }
        previous = ordinal;
        previousRow = row.index;
      }
    }
  };

  const compareFold = (fold: CellGridFrame, snapshot: CellGridFrame): boolean => {
    const scalars: Array<readonly [string, string | number | boolean, string | number | boolean]> = [
      ["cols", snapshot.cols, fold.cols],
      ["rows", snapshot.rows, fold.rows],
      ["cursorRow", snapshot.cursorRow, fold.cursorRow],
      ["cursorCol", snapshot.cursorCol, fold.cursorCol],
      ["cursorVisible", snapshot.cursorVisible, fold.cursorVisible],
      ["altScreen", snapshot.altScreen, fold.altScreen],
      ["cursorKeysApp", snapshot.cursorKeysApp, fold.cursorKeysApp],
      ["bracketedPaste", snapshot.bracketedPaste, fold.bracketedPaste],
      ["scrollbackTotal", snapshot.scrollbackTotal, fold.scrollbackTotal],
      ["viewportRows", snapshot.viewportRows.length, fold.viewportRows.length],
    ];
    for (const [name, expected, actual] of scalars) {
      if (expected === actual) continue;
      record("fold", "fold-scalar", `folded ${name} drifted from the live grid`, `${name}=${expected}`, `${name}=${actual}`);
      return false;
    }
    for (let row = 0; row < snapshot.viewportRows.length; row++) {
      const want = snapshot.viewportRows[row]!;
      const got = fold.viewportRows[row]!;
      if (got.index !== row) {
        record("fold", "fold-row-index", "folded viewport row lost its grid coordinate", `index=${row}`, `index=${got.index}`, row);
        return false;
      }
      const wantHash = rowHash(want.spans);
      const gotHash = rowHash(got.spans);
      if (wantHash === gotHash) continue;
      record(
        "fold",
        "fold-row-mismatch",
        `folded row ${row} differs from the all-row snapshot: snapshot `
        + `${JSON.stringify(rowText(want.spans).slice(0, 72))} vs fold ${JSON.stringify(rowText(got.spans).slice(0, 72))}`,
        `row ${row} hash ${wantHash}`,
        `row ${row} hash ${gotHash}`,
        row,
        firstDifferentCell(want.spans, got.spans),
      );
      return false;
    }
    return true;
  };

  /** The fold only ever claims [sbBase, scrollbackTotal): older lines were either
   *  evicted by the ring or dropped by an authoritative viewport-only full frame. */
  const compareScrollback = (fold: CellGridFrame, deep: boolean): boolean => {
    const retained = core.getScrollbackCount();
    const monoTotal = emit.sbDropped + retained;
    for (let i = 0; i < fold.scrollbackRows.length; i++) {
      const row = fold.scrollbackRows[i]!;
      if (row.index === fold.sbBase + i) continue;
      record(
        "scrollback", "history-gap",
        `held history is not contiguous from sbBase=${fold.sbBase}: slot ${i} carries index ${row.index}`,
        `index=${fold.sbBase + i}`, `index=${row.index}`, row.index,
      );
      return false;
    }
    const lo = Math.max(fold.sbBase, emit.sbDropped, deep ? 0 : monoTotal - SB_TAIL_CHECK);
    if (monoTotal <= lo) return true;
    for (const want of readScrollbackRangeCells(core, lo, monoTotal, emit.sbDropped)) {
      const got = fold.scrollbackRows[want.index - fold.sbBase];
      if (got === undefined) {
        record(
          "scrollback", "history-missing",
          `retained line ${want.index} is absent from the folded history `
          + `[${fold.sbBase}, ${fold.sbBase + fold.scrollbackRows.length})`,
          JSON.stringify(rowText(want.spans).slice(0, 72)), "<missing>", want.index,
        );
        return false;
      }
      const wantHash = rowHash(want.spans);
      const gotHash = rowHash(got.spans);
      if (wantHash === gotHash) continue;
      record(
        "scrollback", "history-mismatch",
        `folded history line ${want.index} differs from the ring: ring `
        + `${JSON.stringify(rowText(want.spans).slice(0, 72))} vs fold ${JSON.stringify(rowText(got.spans).slice(0, 72))}`,
        `line ${want.index} hash ${wantHash}`, `line ${want.index} hash ${gotHash}`,
        want.index, firstDifferentCell(want.spans, got.spans),
      );
      return false;
    }
    return true;
  };

  /** Ordinal-derived truth for the ring origin, independent of the counter the
   *  emitter reads. The two must agree at every step. */
  const checkRingOrigin = (fold: CellGridFrame): boolean => {
    const retained = core.getScrollbackCount();
    if (retained === 0) return true;
    const newest = ordinalOf(scrollbackLineText(core, 0));
    if (newest === null) return true;
    if (emit.sbDropped === 0) {
      const mono = retained - 1;
      if (anchorOrdinal === -1 || mono - anchorMono !== newest - anchorOrdinal) floorOrdinal = newest;
      anchorOrdinal = newest;
      anchorMono = mono;
      return true;
    }
    if (anchorOrdinal === -1) return true;
    const oldest = ordinalOf(scrollbackLineText(core, retained - 1));
    if (oldest === null || oldest < floorOrdinal) return true;
    if (newest - oldest + 1 !== retained) {
      record(
        "scrollback", "ring-not-contiguous",
        `the ring retains ${retained} lines but spans ordinals #${oldest}..#${newest}`,
        `${newest - oldest + 1} lines`, `${retained} lines`,
      );
      return false;
    }
    const trueDropped = anchorMono + (oldest - anchorOrdinal);
    const trueTotal = anchorMono + (newest - anchorOrdinal) + 1;
    if (emit.sbDropped !== trueDropped) {
      record(
        "scrollback", "eviction-origin-drift",
        "the eviction origin the emitter read from the core drifted from the ordinals the ring actually holds "
        + `(oldest retained line is #${oldest})`,
        `sbDropped=${trueDropped}`, `sbDropped=${emit.sbDropped}`,
      );
      return false;
    }
    if (fold.scrollbackTotal !== trueTotal) {
      record(
        "scrollback", "monotonic-total-drift",
        "the folded monotonic total drifted from the ordinals the ring actually holds "
        + `(newest retained line is #${newest})`,
        `scrollbackTotal=${trueTotal}`, `scrollbackTotal=${fold.scrollbackTotal}`,
      );
      return false;
    }
    return true;
  };

  /** One emit + fold + all-row comparison. Returns the folded frame, or null when
   *  a lane whose state cannot survive its own failure has diverged. */
  const emitAndFold = (force: boolean): CellGridFrame | null => {
    const previousRevision = emit.gridEpochRevision;
    const next = nextCellFrame(core, emit, force, SB_SNAPSHOT_HISTORY_ROWS);
    const frame = next.frame;
    emit = next.state;
    if (emit.gridEpochRevision !== previousRevision) coverage.epochAdvances++;
    epochs.add(frame.gridEpoch);
    if (frame.full) {
      coverage.framesFull++;
      if (force) coverage.attachFullFrames++;
    } else {
      coverage.framesDelta++;
      coverage.deltaRowsTotal += frame.viewportRows.length;
      coverage.deltaRowsMax = Math.max(coverage.deltaRowsMax, frame.viewportRows.length);
    }
    const folded = client.held === null ? frame : applyDelta(client.held, frame);
    if (folded === null) {
      record(
        "fold", "delta-rejected",
        "applyDelta refused the emitted delta, so a browser holding this frame could only recover by repair",
        "delta applies to the held frame", "applyDelta() === null",
      );
      return null;
    }
    client.held = folded;
    core.clearDirty();

    const snapshot = gridToCellFrame(core, frame.seq, frame.gridEpoch, 0, emit.sbDropped);
    dimensions.add(`${snapshot.cols}x${snapshot.rows}`);
    gridHashes.add(gridHash(snapshot.viewportRows));
    if (!snapshot.cursorVisible) coverage.cursorHiddenSteps++;
    if (snapshot.cursorKeysApp) coverage.cursorKeysAppSteps++;
    if (snapshot.bracketedPaste) coverage.bracketedPasteSteps++;
    if (snapshot.altScreen !== lastAlt) {
      coverage.altTransitions++;
      lastAlt = snapshot.altScreen;
    }
    const retained = core.getScrollbackCount();
    coverage.scrollbackMax = Math.max(coverage.scrollbackMax, retained);
    coverage.scrollbackDiscarded = emit.sbDropped;
    if (emit.sbDropped > 0) coverage.ringEvictingSteps++;
    countContentCoverage(snapshot.viewportRows);
    observeWideModel(snapshot.viewportRows, snapshot.cols);

    if (!compareFold(folded, snapshot)) return null;
    if (!compareScrollback(folded, false)) return null;
    if (!checkRingOrigin(folded)) return null;
    checkDocument(snapshot, readScrollbackRangeCells(
      core,
      Math.max(emit.sbDropped, emit.sbDropped + retained - DOC_TAIL_ROWS),
      emit.sbDropped + retained,
      emit.sbDropped,
    ));
    return folded;
  };

  for (const step of program.steps) {
    traceLine = step.traceLine;
    stepKind = step.kind;
    switch (step.kind) {
      case "doc":
        docArmed = step.on;
        break;
      case "sweep": {
        coverage.sweeps++;
        // The origin at each named checkpoint. A single end-of-run number cannot
        // pin WHERE the ring rolled; this trail can, and it is what holds the
        // authoritative counter to the value the retired inference produced.
        coverage.discardedAtSweep[step.label] = emit.sbDropped;
        if (client.held !== null) {
          if (!compareScrollback(client.held, true)) { aborted = stepIndex; break; }
          const full = gridToCellFrame(core, -1, "sweep", undefined, emit.sbDropped);
          checkDocument(full, full.scrollbackRows);
          const recut = gridToCellFrame(ref, -1, "sweep", BOUNDARY_TAIL_ROWS, 0);
          const want = `${gridHash(full.viewportRows)}/${historyTailHash(full.scrollbackRows, BOUNDARY_TAIL_ROWS)}`;
          const got = `${gridHash(recut.viewportRows)}/${historyTailHash(recut.scrollbackRows, BOUNDARY_TAIL_ROWS)}`;
          if (want !== got) {
            record(
              "boundary", "chunk-boundary-dependent",
              `sweep "${step.label}": the same bytes re-cut into ${REF_PIECE}-byte writes produced a different grid, `
              + "so the parser's cross-chunk carry is not boundary independent",
              want, got,
            );
          }
        }
        break;
      }
      case "resize":
        coverage.resizes++;
        core.resize(step.cols, step.rows);
        ref.resize(step.cols, step.rows);
        if (emitAndFold(false) === null) aborted = stepIndex;
        break;
      case "attach": {
        const fold = emitAndFold(true);
        if (fold === null) { aborted = stepIndex; break; }
        const monoTotal = emit.sbDropped + core.getScrollbackCount();
        refreshes.push({
          stepIndex,
          scrollbackTotal: monoTotal,
          fullFrameHistoryRows: fold.scrollbackRows.length,
          backfillRows: readScrollbackRangeCells(core, emit.sbDropped, monoTotal, emit.sbDropped).length,
          recovers: fold.scrollbackRows.length > 0 ? "accumulated" : "future-only",
        });
        break;
      }
      case "write": {
        chunkIndex++;
        byteOffset = streamOffset;
        chunkBytes = step.bytes.length;
        coverage.writeChunks++;
        coverage.bytes += chunkBytes;
        const text = decoder.decode(step.bytes);
        coverage.syncBoundaries += countOccurrences(text, SYNC_ON) + countOccurrences(text, SYNC_OFF);

        // The worker's exact per-chunk order: one ordered pass that feeds the
        // core in segments cut at each synthesized probe and drains the core's
        // queued replies between them, then emit
        // (session-scrollback.appendScrollback -> answerQueries ->
        // session-emit.emitCellFrame).
        expectedSegments.length = 0;
        expectedNatives.length = 0;
        const answered = answerQueries(replyCarry, core, step.bytes);
        const actualReply = answered.bytes;
        referenceChunk(step.bytes);
        const owed = expectedSegments.join("");
        if (expectedNatives.length > 1 && queryReplyModel === "unobserved") {
          const drained = answered.native;
          if (drained === expectedNatives.join("")) queryReplyModel = "ordered-drain";
          else if (drained === expectedNatives[expectedNatives.length - 1]) queryReplyModel = "single-slot-last-wins";
        }
        if (owed !== actualReply) {
          const matched = countOwedSegments(actualReply, expectedSegments);
          const reordered = matched === expectedSegments.length;
          let detail: string;
          if (reordered) {
            detail = "every reply this chunk owes reached the pty, but not in the order the probes appeared in the stream";
          } else if (matched === -1) {
            detail = "the pty received reply bytes no probe in this chunk asked for";
          } else {
            detail = `the pty never received ${expectedSegments.length - matched} of the `
              + `${expectedSegments.length} replies this chunk owes the application`;
          }
          record(
            "reply", reordered ? "reply-out-of-order" : "reply-dropped", detail,
            JSON.stringify(owed), JSON.stringify(actualReply),
          );
        }
        if (emitAndFold(false) === null) aborted = stepIndex;
        break;
      }
    }
    stepIndex++;
    if (aborted !== null) break;
  }

  coverage.dimensions = [...dimensions];
  coverage.epochs = [...epochs];
  coverage.distinctGridHashes = gridHashes.size;

  return {
    steps: program.steps.length,
    coverage,
    observations: {
      scrollbackCapacity: coverage.scrollbackMax,
      wideCellModel,
      queryReplyModel,
      refreshes,
    },
    divergences,
    divergenceCounts: {
      fold: laneCount.get("fold") ?? 0,
      scrollback: laneCount.get("scrollback") ?? 0,
      document: laneCount.get("document") ?? 0,
      reply: laneCount.get("reply") ?? 0,
      boundary: laneCount.get("boundary") ?? 0,
    },
    abortedAtStep: aborted,
    finalGridHash: client.held === null ? "" : gridHash(client.held.viewportRows),
  };
}
