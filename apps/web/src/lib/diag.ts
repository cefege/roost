// SPA-side diag batcher. Sits in front of the shared `diag()` facade
// from @roost/shared/diag. Buffers events in an in-memory ring, flushes
// every 100ms or 64 entries (whichever first) via coordClient.diagDebugLogBatch.
// On pagehide / beforeunload, uses navigator.sendBeacon to flush the
// tail synchronously so the last events before close still ship.
//
// Gated by localStorage.roostDiag === "1" (module-load gate inside
// @roost/shared/diag). When disabled this whole file is a no-op since
// the upstream diag() never invokes our sink.
//
// Owners: apps/web/src/main.tsx (installer), CellTerminal.tsx + sync.ts +
// ws/sync-outbound.ts (callers via the global `diag` export).

import { setDiagSink, setSignalSink, isDiagEnabled, signal } from "@roost/shared/diag";
import { coordClient } from "../connect.ts";

// Fixed-size, always-on SPA phase recorder. Unlike the diagnostic firehose,
// phase marks must exist on an ordinary cold navigation: enabling diagnostics
// after the fact would make the bootstrap waterfall unmeasurable. The eager
// ring is deliberately small and never grows with reconnects or session churn.
const PHASE_MARK_CAPACITY = 256;

export type SpaPhaseName =
  | "module_start"
  | "identity_complete"
  | "self_register_gate"
  | "self_register_start"
  | "self_register_complete"
  | "sync_subscribed"
  | "snapshot_complete"
  | "snapshot_applied"
  | "sessions_list_publish"
  | "terminal_mount"
  | "viewport_enqueue"
  | "viewport_accept"
  | "first_cell_receive"
  | "first_cell_apply"
  | "marker_presented"
  | "cursor_presented";

export type SpaPhaseValue = string | number | boolean | bigint | null | undefined;

export type SpaPhaseMark = {
  index: number;
  name: SpaPhaseName;
  monotonicMs: number;
  epochMs: number;
  sinceNavigationMs: number;
  onceKey?: string;
  detail: Record<string, string | number | boolean | null>;
};

export type SpaPhaseTimeline = {
  capacity: number;
  dropped: number;
  timeOriginEpochMs: number;
  navigationStartEpochMs: number;
  driverBeforeNavigationEpochMs: number | null;
  marks: SpaPhaseMark[];
};

const _phaseTimeOrigin = typeof performance === "undefined" ? Date.now() : performance.timeOrigin;
const _phaseNavigationStart = (() => {
  if (typeof performance === "undefined") return _phaseTimeOrigin;
  const entry = performance.getEntriesByType("navigation")[0] as PerformanceNavigationTiming | undefined;
  return performance.timeOrigin + (entry?.startTime ?? 0);
})();
const _phaseMarks = new Array<SpaPhaseMark | undefined>(PHASE_MARK_CAPACITY);
let _phaseMarkWrites = 0;

function phaseDetail(
  detail: Readonly<Record<string, SpaPhaseValue>> | undefined,
): Record<string, string | number | boolean | null> {
  const safe: Record<string, string | number | boolean | null> = {};
  if (!detail) return safe;
  for (const [key, value] of Object.entries(detail)) {
    if (value === undefined) continue;
    safe[key] = typeof value === "bigint" ? value.toString() : value;
  }
  return safe;
}

function findPhaseOnce(name: SpaPhaseName, onceKey: string): SpaPhaseMark | undefined {
  const available = Math.min(_phaseMarkWrites, PHASE_MARK_CAPACITY);
  const first = Math.max(0, _phaseMarkWrites - available);
  for (let index = first; index < _phaseMarkWrites; index++) {
    const mark = _phaseMarks[index % PHASE_MARK_CAPACITY];
    if (mark?.name === name && mark.onceKey === onceKey) return mark;
  }
  return undefined;
}

/** Record one bounded, synchronous bootstrap/terminal phase mark. */
export function markPhase(
  name: SpaPhaseName,
  detail?: Readonly<Record<string, SpaPhaseValue>>,
): SpaPhaseMark {
  const monotonicMs = typeof performance === "undefined" ? 0 : performance.now();
  const mark: SpaPhaseMark = {
    index: _phaseMarkWrites,
    name,
    monotonicMs,
    epochMs: _phaseTimeOrigin + monotonicMs,
    sinceNavigationMs: _phaseTimeOrigin + monotonicMs - _phaseNavigationStart,
    detail: phaseDetail(detail),
  };
  _phaseMarks[_phaseMarkWrites % PHASE_MARK_CAPACITY] = mark;
  _phaseMarkWrites++;
  return mark;
}

/** Record the first matching phase only within this bounded document ring. */
export function markPhaseOnce(
  name: SpaPhaseName,
  onceKey: string,
  detail?: Readonly<Record<string, SpaPhaseValue>>,
): SpaPhaseMark {
  const existing = findPhaseOnce(name, onceKey);
  if (existing) return existing;
  const mark = markPhase(name, detail);
  mark.onceKey = onceKey;
  return mark;
}

/** Snapshot the phase ring oldest→newest without exposing mutable storage. */
export function phaseTimeline(): SpaPhaseTimeline {
  const available = Math.min(_phaseMarkWrites, PHASE_MARK_CAPACITY);
  const first = Math.max(0, _phaseMarkWrites - available);
  const marks = new Array<SpaPhaseMark>(available);
  for (let offset = 0; offset < available; offset++) {
    marks[offset] = _phaseMarks[(first + offset) % PHASE_MARK_CAPACITY]!;
  }
  const driverEpoch = typeof window === "undefined"
    ? undefined
    : (window as Window & { __roostDriverBeforeNavigationEpochMs?: unknown })
      .__roostDriverBeforeNavigationEpochMs;
  return {
    capacity: PHASE_MARK_CAPACITY,
    dropped: Math.max(0, _phaseMarkWrites - PHASE_MARK_CAPACITY),
    timeOriginEpochMs: _phaseTimeOrigin,
    navigationStartEpochMs: _phaseNavigationStart,
    driverBeforeNavigationEpochMs:
      typeof driverEpoch === "number" && Number.isFinite(driverEpoch) ? driverEpoch : null,
    marks: marks.map((mark) => ({ ...mark, detail: { ...mark.detail } })),
  };
}

const FLUSH_MAX_ENTRIES = 64;
const FLUSH_INTERVAL_MS = 100;
// session_trace_id cache. Filled by sync stream when it sees a
// `opened` event; helpers below pull from this. Module-global so any
// emit callsite can attach without props plumbing.
const _sessionTrace = new Map<string, string>();
/** Returns the session_trace_id for `sid`, lazily generating a fresh
 *  one if not seen before. SPA-local — for cross-app correlation use
 *  sid + ts window (worker stamps its own session_trace_id; the two
 *  ids don't share a generator). */
export function getSessionTraceId(sid: string): string {
  let id = _sessionTrace.get(sid);
  if (!id) {
    // 8 hex chars — same shape as trace.ts::newTraceId; generated
    // from crypto for collision-resistance across browser windows.
    const a = crypto.getRandomValues(new Uint8Array(4));
    id = Array.from(a).map((b) => b.toString(16).padStart(2, "0")).join("");
    _sessionTrace.set(sid, id);
  }
  return id;
}

/** Reap a closed session's trace-id entry. No-op when absent. */
export function pruneSessionTrace(sid: string): void { _sessionTrace.delete(sid); _lagFloor.delete(sid); }

/** Live trace-id map size, for the leak-watch accumulator sample. */
export function sessionTraceSize(): number { return _sessionTrace.size; }

// Field names follow the proto-es camelCase convention because the
// generated Connect client serializer drops fields whose names don't
// match — sending snake_case results in 0n/empty on the coord side.
interface Entry {
  evt: string;
  tsMs: bigint;
  monoNs: bigint;
  traceId: string;
  sessionTraceId: string;
  sid: string;
  viewerKey: string;
  kvJson: string;
  // Tier-1 always-on signal — coord routes these to log.warn(target=signal)
  // → *.err.log (read by `roost doctor`). false = gated diag firehose entry.
  signal: boolean;
}

let _buf: Entry[] = [];
let _timer: ReturnType<typeof setTimeout> | null = null;

// Echo RTT distribution tracker (Step 1.5). Collects echo.rtt_sample and
// echo.frame_rtt events into sliding windows; window.__roostEchoRtt() prints
// p50/p95/p99/max — the metric that maps to "typing feels laggy" (variance,
// not mean). Only collects when diag is enabled (events are diag, not signal).
const ECHO_RTT_CAP = 500;
const _echoRtt: Record<string, number[]> = { "echo.rtt_sample": [], "echo.frame_rtt": [] };
function _pushEchoRtt(evt: string, rttMs: number): void {
  const buf = _echoRtt[evt];
  if (!buf) return;
  buf.push(rttMs);
  if (buf.length > ECHO_RTT_CAP) buf.shift();
}
function _printEchoRtt(): void {
  for (const [evt, samples] of Object.entries(_echoRtt)) {
    if (samples.length === 0) { console.log(`${evt}: no samples`); continue; }
    const s = [...samples].sort((a, b) => a - b);
    const at = (p: number) => s[Math.min(s.length - 1, Math.floor(p * s.length))];
    console.log(`${evt} (n=${s.length}): p50=${at(0.5).toFixed(1)}ms p95=${at(0.95).toFixed(1)}ms p99=${at(0.99).toFixed(1)}ms max=${s[s.length - 1].toFixed(1)}ms`);
  }
}

// Lag-lens (Phase 1). Per-hop echo round-trip breakdown. recordCellLag reads
// the 4 down-leg frame stamps off the raw PbCellGridFrame; `post` (client POST
// duration) + `renderer_apply` (fold plus any synchronous DOM reconciliation)
// arrive via pushRecord. `dom_reconcile_opportunity` is frame arrival through
// two rAFs after the apply; it is a presentation opportunity, never raster
// paint proof. window.__roostLag() prints p50/p95/max per segment + the
// dominant hop. All wall-clock ms; valid
const LAG_CAP = 500;
// Felt-lag alarm floor for the always-on cell.paint_lag signal. The worker's own
// coalesce budget is 16 ms and the documented first-paint budget is 40 ms
// (CLAUDE.md), so 750 ms is unambiguously user-felt and far above any legitimate
// coalesce window plus tailnet RTT.
const PAINT_LAG_SIGNAL_MS = 750;
// Per-session clock-offset estimate: the smallest raw (browser recvWall −
// worker ptyOutMs) ever seen for that session. ptyOutMs is the WORKER's clock,
// so the raw difference carries an unknown constant offset and must never be
// compared to an absolute threshold — only the EXCESS above this floor is lag.
// NTP-scale drift is ms/hour and is swamped by the threshold; a session whose
// very first frames are already lagged under-reports, which is the correct
// direction to fail.
const _lagFloor = new Map<string, number>();
const _lag: Record<string, number[]> = {
  queue_wait: [], post: [], worker_prep: [], w2c_wire: [], coord_internal: [],
  c2client_wire: [], renderer_apply: [], dom_reconcile_opportunity: [],
};
function _pushLag(seg: string, ms: number): void {
  const buf = _lag[seg];
  if (!buf) return;
  buf.push(ms);
  if (buf.length > LAG_CAP) buf.shift();
}
type LagStamps = { sessionId: string; full: boolean; ptyOutMs: bigint; workerEmitMs: bigint; coordRecvMs: bigint; coordFanoutMs: bigint };
/** Collect the down-leg segment durations from a raw cell frame's stamps.
 *  recvWall = Date.now() captured at browser dispatch. The diag ring skips any
 *  segment whose needed stamp is 0/unset (Number(bigint) is lossless for
 *  ms < 2^53); the ALWAYS-ON paint-lag check reports such a segment as -1. */
export function recordCellLag(pb: LagStamps, recvWall: number): void {
  const ptyOut = Number(pb.ptyOutMs);
  const workerEmit = Number(pb.workerEmitMs);
  const coordRecv = Number(pb.coordRecvMs);
  const coordFanout = Number(pb.coordFanoutMs);
  const havePrep = ptyOut > 0 && workerEmit > 0;
  const haveW2c = workerEmit > 0 && coordRecv > 0;
  const haveInternal = coordRecv > 0 && coordFanout > 0;
  const haveDownLeg = coordFanout > 0;
  // w2c_wire / c2client_wire cross a clock boundary, so a legitimate value can
  // be negative; presence of the stamps — not its sign — decides reporting.
  const workerPrep = havePrep ? workerEmit - ptyOut : -1;
  const w2cWire = haveW2c ? coordRecv - workerEmit : -1;
  const coordInternal = haveInternal ? coordFanout - coordRecv : -1;
  const c2clientWire = haveDownLeg ? recvWall - coordFanout : -1;
  if (isDiagEnabled()) {
    if (havePrep) _pushLag("worker_prep", workerPrep);
    if (haveW2c) _pushLag("w2c_wire", w2cWire);
    if (haveInternal) _pushLag("coord_internal", coordInternal);
    if (haveDownLeg) _pushLag("c2client_wire", c2clientWire);
  }
  // Deltas only. A FULL frame is a snapshot of retained state, so its ptyOutMs
  // is the age of the newest retained byte, not a latency: revealing a pane
  // whose session last printed minutes ago legitimately carries a ptyOut that
  // old. Measured live before this guard: three of seven alarms were reveals of
  // idle sessions (worker_prep 6.4 s / 37.9 s / 114.3 s with c2client_wire
  // NEGATIVE, i.e. the frame arrived promptly). The symptom this signal owns —
  // painted output falling behind live output — rides the delta path.
  if (pb.full || ptyOut <= 0) return;
  const sid = pb.sessionId;
  const raw = recvWall - ptyOut;
  const floor = Math.min(_lagFloor.get(sid) ?? raw, raw);
  _lagFloor.set(sid, floor);
  const excess = raw - floor;
  if (excess > PAINT_LAG_SIGNAL_MS) {
    signal("cell.paint_lag", {
      sid, total_ms: excess, worker_prep: workerPrep, w2c_wire: w2cWire,
      coord_internal: coordInternal, c2client_wire: c2clientWire, cooldownKey: sid,
    });
  }
}
function _pct(arr: number[], p: number): number {
  const s = [...arr].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor(p * s.length))] ?? 0;
}
function _printLag(): void {
  const segs = ["queue_wait", "post", "worker_prep", "w2c_wire", "coord_internal", "c2client_wire", "renderer_apply", "dom_reconcile_opportunity"];
  const p50s: Record<string, number> = {};
  for (const seg of segs) {
    const arr = _lag[seg] ?? [];
    if (arr.length === 0) { console.log(`${seg}: no samples`); p50s[seg] = 0; continue; }
    p50s[seg] = _pct(arr, 0.5);
    console.log(`${seg} (n=${arr.length}): p50=${_pct(arr, 0.5).toFixed(1)}ms p95=${_pct(arr, 0.95).toFixed(1)}ms max=${Math.max(...arr).toFixed(1)}ms`);
  }
  const totalArr = _echoRtt["echo.frame_rtt"] ?? [];
  const totalP50 = totalArr.length ? _pct(totalArr, 0.5) : 0;
  if (totalArr.length) console.log(`total (n=${totalArr.length}): p50=${totalP50.toFixed(1)}ms p95=${_pct(totalArr, 0.95).toFixed(1)}ms max=${Math.max(...totalArr).toFixed(1)}ms`);
  else console.log("total: no samples");
  const sumP50 = segs.reduce((a, s) => a + (p50s[s] ?? 0), 0);
  const residual = totalP50 - sumP50;
  console.log(`residual (total - measured segments): p50=${residual.toFixed(1)}ms`);
  const candidates: Record<string, number> = { ...p50s, residual };
  let dom = "residual", domV = -Infinity;
  for (const [k, v] of Object.entries(candidates)) if (v > domV) { domV = v; dom = k; }
  console.log(`dominant hop: ${dom} (p50=${domV.toFixed(1)}ms)`);
}

function flushSoon(): void {
  if (_timer !== null) return;
  _timer = setTimeout(() => {
    _timer = null;
    void flushNow();
  }, FLUSH_INTERVAL_MS);
}

async function flushNow(): Promise<void> {
  if (_buf.length === 0) return;
  const batch = _buf;
  _buf = [];
  try {
    await coordClient.diagDebugLogBatch({ entries: batch });
  } catch {
    // Coord unreachable — drop silently. Memory-bounded over the
    // network outage by virtue of the buffer cap below. Console-only
    // here: calling diag()/signal() would recurse into this same sink.
    console.warn("[diag] flush dropped", batch.length);
  }
}

function sendBeaconFlush(): void {
  if (_buf.length === 0) return;
  // Best-effort sync send on pagehide. coordClient is the proto-ish
  // codec; we can't easily use it from sendBeacon. Use a raw POST with
  // proto-es camelCase field names so the Connect JSON encoding parses
  // the same way the unary path does.
  try {
    const body = JSON.stringify({
      entries: _buf.map((e) => ({
        evt: e.evt,
        tsMs: e.tsMs.toString(),
        monoNs: e.monoNs.toString(),
        traceId: e.traceId,
        sessionTraceId: e.sessionTraceId,
        sid: e.sid,
        viewerKey: e.viewerKey,
        kvJson: e.kvJson,
        signal: e.signal,
      })),
    });
    const blob = new Blob([body], { type: "application/json" });
    navigator.sendBeacon("/roost.v1.CoordinatorService/DiagDebugLogBatch", blob);
  } catch { console.warn("[diag] flush dropped", _buf.length); }
  _buf = [];
}

// Hook the shared facade's sinks. We extract sid / viewer_key / trace_id
// / session_trace_id from the record up front so the proto fields stay
// typed; everything else goes into kv_json. `isSignal` marks Tier-1
// always-on entries so coord routes them to *.err.log.
function pushRecord(record: Record<string, unknown>, isSignal: boolean): void {
  const evt = String(record.evt ?? "");
  const tsMs = BigInt(Math.round(Number(record.ts_ms ?? Date.now())));
  const monoNs = BigInt(Math.round(Number(record.mono_ns ?? 0)));
  const traceId = String(record.trace_id ?? "");
  const sid = String(record.sid ?? "");
  const viewerKey = String(record.viewer_key ?? "");
  const sessionTraceId = String(
    record.session_trace_id ?? (sid ? (_sessionTrace.get(sid) ?? "") : ""),
  );

  const kv: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(record)) {
    if (k === "evt" || k === "ts_ms" || k === "mono_ns" || k === "trace_id"
      || k === "session_trace_id" || k === "sid" || k === "viewer_key") continue;
    kv[k] = v;
  }

  _buf.push({
    evt, tsMs, monoNs, traceId, sessionTraceId, sid, viewerKey,
    kvJson: Object.keys(kv).length ? JSON.stringify(kv) : "",
    signal: isSignal,
  });
  if (evt === "echo.rtt_sample" || evt === "echo.frame_rtt") {
    const rttMs = Number(record.rtt_ms ?? 0);
    if (rttMs > 0) _pushEchoRtt(evt, rttMs);
  } else if (evt === "echo.post_dur") {
    const d = Number(record.dur_ms ?? 0);
    if (d > 0) _pushLag("post", d);
  } else if (evt === "input.queue_wait") {
    const d = Number(record.dur_ms ?? 0);
    if (d > 0) _pushLag("queue_wait", d);
  } else if (evt === "cell.apply_dur") {
    const d = Number(record.dur_ms ?? 0);
    if (d > 0) _pushLag("renderer_apply", d);
  } else if (evt === "cell.dom_reconcile_opportunity") {
    const d = Number(record.dur_ms ?? 0);
    if (d > 0) _pushLag("dom_reconcile_opportunity", d);
  }

  if (_buf.length >= FLUSH_MAX_ENTRIES) {
    if (_timer !== null) { clearTimeout(_timer); _timer = null; }
    void flushNow();
    return;
  }
  flushSoon();
}
function spaSink(record: Record<string, unknown>): void { pushRecord(record, false); }
function spaSignalSink(record: Record<string, unknown>): void { pushRecord(record, true); }

/** Install the always-on signal ship path + pagehide flush. Call once from
 *  main.tsx UNCONDITIONALLY — Tier-1 signals must ship even when the diag
 *  firehose (ROOST_DIAG / localStorage.roostDiag) is off. */
export function installSignalShip(): void {
  setSignalSink(spaSignalSink);
  (window as Window & { __roostEchoRtt?: () => void; __roostLag?: () => void }).__roostEchoRtt = _printEchoRtt;
  (window as Window & { __roostLag?: () => void }).__roostLag = _printLag;
  window.addEventListener("pagehide", sendBeaconFlush, { capture: true });
  window.addEventListener("beforeunload", sendBeaconFlush, { capture: true });
}

/** Install the SPA-side firehose sink. Gated by localStorage.roostDiag.
 *  Shares the buffer + beacon listeners installed by installSignalShip. */
export function installSpaDiag(): void {
  if (!isDiagEnabled()) return;
  setDiagSink(spaSink);
}
