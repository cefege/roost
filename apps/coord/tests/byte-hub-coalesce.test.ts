// Coord's PTY-byte coalescer (byte-hub publishBytes). The governor must ship the
// first chunk after idle with zero added latency, collapse a continuing burst
// into one frame per 16 ms window, flush early once a window exceeds the byte
// cap, and drop a pending buffer on `closed`. A regression re-floods the Sync
// socket with frames that paint nothing (head-of-line-blocking the cell frames
// that do) or silently reorders/loses PTY bytes, which the three carry-based
// consumers cannot recover from.

import { describe, test, expect, beforeEach, afterEach, vi } from "bun:test";
import { asWorkerFp, asChannelId, asSessionId } from "@roost/shared/wire";
import type { SessionEvent } from "@roost/shared/wire";
import { publishBytes, primeChannelMap, applyDurableChannelIndex } from "../src/byte-hub.ts";
import { globalBytesBus } from "../src/buses.ts";

const WF = asWorkerFp("bc".repeat(32));
const COALESCE_MS = 16;
const CAP_BYTES = 256 * 1024;

function bindChannel(sessionId: string, channel: number): void {
  primeChannelMap([{ id: sessionId, worker_fp: String(WF), channel }]);
}

function collect(sessionId: string): { got: string[]; stop: () => void } {
  const got: string[] = [];
  const unsub = globalBytesBus.subscribe((m) => {
    if (m.session_id === sessionId) got.push(new TextDecoder().decode(m.bytes));
  });
  return { got, stop: unsub };
}

function chunk(text: string): Uint8Array {
  return new TextEncoder().encode(text);
}

describe("byte-hub PTY coalescer", () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

  test("the first chunk after idle publishes synchronously", () => {
    const sid = "sess-coalesce-leading";
    bindChannel(sid, 11);
    const c = collect(sid);

    publishBytes(WF, asChannelId(11), chunk("echo"));

    // Leading edge: no window elapsed, so a single keystroke echo pays nothing.
    expect(c.got).toEqual(["echo"]);
    c.stop();
  });

  test("three back-to-back chunks yield the leading frame plus one coalesced frame", () => {
    const sid = "sess-coalesce-burst";
    bindChannel(sid, 12);
    const c = collect(sid);

    publishBytes(WF, asChannelId(12), chunk("one"));
    publishBytes(WF, asChannelId(12), chunk("two"));
    publishBytes(WF, asChannelId(12), chunk("three"));

    expect(c.got).toEqual(["one"]);
    vi.advanceTimersByTime(COALESCE_MS);
    // Chunks 2+3 arrive as one frame, concatenated in publish order.
    expect(c.got).toEqual(["one", "twothree"]);

    // Idle through the next window: the re-armed timer finds nothing, retires
    // the entry, and the following chunk is a leading edge again.
    vi.advanceTimersByTime(COALESCE_MS);
    publishBytes(WF, asChannelId(12), chunk("four"));
    expect(c.got).toEqual(["one", "twothree", "four"]);
    c.stop();
  });

  test("exceeding the byte cap mid-window flushes without waiting", () => {
    const sid = "sess-coalesce-cap";
    bindChannel(sid, 13);
    const c = collect(sid);

    publishBytes(WF, asChannelId(13), chunk("head"));
    expect(c.got.length).toBe(1);

    publishBytes(WF, asChannelId(13), new Uint8Array(CAP_BYTES).fill(0x41));

    // No clock advance: the cap alone forced the flush.
    expect(c.got.length).toBe(2);
    expect(c.got[1]!.length).toBe(CAP_BYTES);
    c.stop();
  });

  test("a closed event drops the pending buffer and leaves no live timer", () => {
    const sid = "00000000-0000-4000-8000-0000000000bc";
    bindChannel(sid, 14);
    const c = collect(sid);

    publishBytes(WF, asChannelId(14), chunk("live"));
    publishBytes(WF, asChannelId(14), chunk("tail-a"));
    publishBytes(WF, asChannelId(14), chunk("tail-b"));
    expect(c.got).toEqual(["live"]);

    const closed: SessionEvent = {
      kind: "closed",
      session_id: asSessionId(sid),
      exit_code: 0,
      ts: 1_780_000_000_000,
    };
    applyDurableChannelIndex(closed, null);

    // The buffered tail is discarded and no re-armed timer survives to flush it.
    vi.advanceTimersByTime(COALESCE_MS * 4);
    expect(c.got).toEqual(["live"]);
    c.stop();
  });
});
