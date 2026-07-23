// Persistent PTY input channel. 2026-06-15: switched from client-streaming
// InputStream RPC to unary sessionsInput per-frame. Chrome's Fetch API
// can't stream request bodies over HTTP/1.1 (throws "The fetch API does
// not support streaming request bodies"), and Bun.serve has no HTTP/2
// server in any release per bun.com/docs/api/http (only h1 + h3). The
// coord's alpnProtocols hint is left in place for when Bun ships h2;
// until then this is the only path that works in-browser.
//
// flush() drains the queue as COALESCED per-session batches: the leading run
// of same-session frames is concatenated (in order) into ONE sessionsInput
// POST. HTTP/1.1 can't multiplex one keep-alive socket, so the old per-frame
// await capped input at ~1 keystroke/RTT and let a single wedged POST (coord
// mid-crash → 15s pending-rpc) freeze the whole queue. Batching collapses a
// burst to one round-trip; SEND_TIMEOUT_MS bounds a wedged coord to ~2.5s
// (drop + reconnect), never 15s. A deadline is the AMBIGUOUS case — the coord
// may have already written the bytes to the PTY and only the ack was slow —
// and PTY input has NO dedup, so a retry would DUPLICATE keystrokes ("hi"→
// "hihi", a stray \r submitting a command). Rule: retry ONLY a hard transport
// error (bytes never left); on a deadline, drop + surface reconnecting.
// Duplicate input is worse than dropped input.

import { coordClient } from "../connect.ts";
import { diag, signal } from "@roost/shared/diag";
import { getSessionTraceId } from "../lib/diag.ts";
import { Code, ConnectError } from "@connectrpc/connect";

async function _sha8(bytes: Uint8Array): Promise<string> {
  try {
    const buf = await crypto.subtle.digest("SHA-256", bytes.slice().buffer);
    return Array.from(new Uint8Array(buf).slice(0, 4))
      .map((b) => b.toString(16).padStart(2, "0")).join("");
  } catch { return ""; }
}

const PENDING_CAP = 200;

// A keystroke POST that hasn't acked within this budget is abandoned (Connect
// aborts the fetch → DeadlineExceeded) so a wedged coord (mid-crash → 15s
// pending-rpc) can't freeze typing. Well above a healthy tailnet RTT (sub-
// 100ms), well below the coord's 15s pending-rpc deadline.
const SEND_TIMEOUT_MS = 2500;
// Echo RTT tracker: records performance.now() on each sendInput so the cell
// frame handler can measure input→echo round-trip (echo.frame_rtt diag).
// One timestamp per session; consumed (read+cleared) on the next cell frame
// so each input→echo cycle yields exactly one measurement.
const _lastSendTs = new Map<string, number>();
export function consumeLastInputSendTs(sid: string): number | undefined {
  const ts = _lastSendTs.get(sid);
  if (ts !== undefined) _lastSendTs.delete(sid);
  return ts;
}

// Injectable so tests drive the send outcome without a live coord. Default
// issues the unary sessionsInput with the deadline above.
type InputSend = (sessionId: string, data: Uint8Array, timeoutMs: number) => Promise<unknown>;

const defaultSend: InputSend = (sessionId, data, timeoutMs) =>
  coordClient.sessionsInput({ sessionId, data }, { timeoutMs });

interface InputFrame { session_id: string; data: Uint8Array; t: number; }

export class InputChannel {
  private pending: InputFrame[] = [];
  private closed = false;
  private flushing = false;
  // Per-session cumulative dropped-frame count (page-scoped). Surfaced on
  // the always-on signal channel so dropped keystrokes on iPad/cellular —
  // otherwise invisible — show up in `roost doctor`. cooldownKey=sid → at
  // most one signal per session per 10s (the burst), carrying the running total.
  private dropTotals = new Map<string, number>();

  constructor(private readonly send: InputSend = defaultSend) {}

  private noteDrop(sid: string, reason: string): void {
    const total = (this.dropTotals.get(sid) ?? 0) + 1;
    this.dropTotals.set(sid, total);
    diag("bytes.up_dropped", { sid, session_trace_id: getSessionTraceId(sid), reason });
    signal("input.drop_burst", { sid, reason, dropped_total: total, cooldownKey: sid });
  }

  start(): void { /* no-op — unary mode, no persistent stream to open */ }

  sendInput(sessionId: string, bytes: Uint8Array): void {
    if (this.closed) return;
    if (this.pending.length >= PENDING_CAP) {
      // Drop-newest on overflow: drop-oldest (the prior `pending.shift()`
      // policy) corrupted pastes by removing prefix chars while letting
      // the suffix through — the shell parsed garbage. Drop-newest at
      // least keeps a contiguous prefix coherent. Surfaced via the signal
      // channel (queue_full) so the user-visible input loss is reviewable.
      this.noteDrop(sessionId, "queue_full");
      return;
    }
    _lastSendTs.set(sessionId, performance.now());
    this.pending.push({ session_id: sessionId, data: bytes, t: performance.now() });
    // diag — every up-byte chunk gets logged + sha8'd. Lets claude trace
    // a single keystroke through coord (bytes.up_relay) and worker
    // (bytes.up_recv) by matching sha8.
    void _sha8(bytes).then((sha8) => {
      diag("bytes.up_send", {
        sid: sessionId, session_trace_id: getSessionTraceId(sessionId),
        dir: "up", len: bytes.length, sha8, in_flight: this.pending.length,
      });
    });
    void this.flush();
  }

  close(): void {
    this.closed = true;
    this.pending = [];
  }

  private async flush(): Promise<void> {
    if (this.flushing || this.closed) return;
    this.flushing = true;
    try {
      while (this.pending.length > 0 && !this.closed) {
        // Coalesce the leading run of same-session frames into ONE ordered
        // POST. Different sessions stay separate calls; per-session order is
        // preserved (contiguous prefix, concatenated in queue order).
        const sid = this.pending[0]!.session_id;
        let n = 0;
        let total = 0;
        while (n < this.pending.length && this.pending[n]!.session_id === sid) {
          total += this.pending[n]!.data.length;
          n++;
        }
        const data = new Uint8Array(total);
        for (let i = 0, off = 0; i < n; i++) {
          data.set(this.pending[i]!.data, off);
          off += this.pending[i]!.data.length;
        }
        // Queue-sit time of the OLDEST keystroke in this batch: enqueue→flush.
        // Under fast spam this is the felt lag the wire-only post_dur misses.
        diag("input.queue_wait", { sid, dur_ms: performance.now() - this.pending[0]!.t });
        try {
          const _t0 = performance.now();
          await this.send(sid, data, SEND_TIMEOUT_MS);
          diag("echo.post_dur", { sid, dur_ms: performance.now() - _t0 });
        } catch (err) {
          if (err instanceof ConnectError && (err.code === Code.DeadlineExceeded || err.code === Code.Canceled)) {
            // Ambiguous: bytes may already be at the PTY. Retrying would
            // duplicate them (no input dedup) → drop the batch instead.
            this.noteDrop(sid, "timeout");
          } else {
            // Hard transport error (reset/refused) — bytes never landed, so a
            // resend can't duplicate. Back off once to ride a tailnet blip; a
            // single POST is atomic, so the batch is all-or-nothing.
            diag("bytes.up_retry", {
              sid, session_trace_id: getSessionTraceId(sid),
              err: err instanceof Error ? err.message : String(err),
            });
            const { promise, resolve } = Promise.withResolvers<void>();
            setTimeout(resolve, 80);
            await promise;
            try {
              await this.send(sid, data, SEND_TIMEOUT_MS);
            } catch {
              this.noteDrop(sid, "send_failed");
            }
          }
        }
        // Batch handled (sent, or dropped after retry/timeout) — remove it and
        // move on so a wedged send can never stall the queue behind it. Frames
        // enqueued during the await sit at index >= n and are untouched.
        this.pending.splice(0, n);
      }
    } finally {
      this.flushing = false;
    }
  }
}

export const inputChannel = new InputChannel();
