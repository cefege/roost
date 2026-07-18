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
// RoostTerm.ts + input-channel.ts (callers via the global `diag` export).

import { setDiagSink, setSignalSink, isDiagEnabled } from "@roost/shared/diag";
import { coordClient } from "../connect.ts";

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
  (window as Window & { __roostEchoRtt?: () => void }).__roostEchoRtt = _printEchoRtt;
  window.addEventListener("pagehide", sendBeaconFlush, { capture: true });
  window.addEventListener("beforeunload", sendBeaconFlush, { capture: true });
}

/** Install the SPA-side firehose sink. Gated by localStorage.roostDiag.
 *  Shares the buffer + beacon listeners installed by installSignalShip. */
export function installSpaDiag(): void {
  if (!isDiagEnabled()) return;
  setDiagSink(spaSink);
}
