// InputChannel flush semantics (ws/input-channel.ts) — the batched, timeout-
// bounded keystroke up-path. No live coord: the send is injected (the class's
// DI seam), so these assert the queue logic directly. Covers: burst coalescing
// into one ordered POST, per-session isolation, retry-once on a HARD transport
// error, and NO retry on a deadline (the ambiguous case — a resend would
// duplicate keystrokes because PTY input has no dedup).
//
// Time is never slept: tests await the REAL signal (the Nth send being issued)
// via `untilCalls`, so a flush step is observed the moment it happens, never
// after a guessed duration. The retry's backoff lives in the code under test;
// the test just awaits the 2nd send it produces.

import { describe, test, expect } from "bun:test";
import { Code, ConnectError } from "@connectrpc/connect";
import { InputChannel } from "../src/ws/input-channel.ts";

const enc = (s: string): Uint8Array => new TextEncoder().encode(s);
const dec = (b: Uint8Array): string => new TextDecoder().decode(b);

interface SendCall { sid: string; data: Uint8Array; timeoutMs: number; }

// A channel whose send outcome is driven per-call. `outcome` returns the promise
// each batched POST settles with; `calls` records every issue; `untilCalls(n)`
// resolves once the nth send has been issued (event-driven, no wall clock).
function makeChannel(outcome: (call: SendCall) => Promise<unknown>): {
  ch: InputChannel;
  calls: SendCall[];
  untilCalls: (n: number) => Promise<void>;
} {
  const calls: SendCall[] = [];
  let waiters: Array<{ n: number; resolve: () => void }> = [];
  const ch = new InputChannel((sid, data, timeoutMs) => {
    calls.push({ sid, data, timeoutMs });
    waiters = waiters.filter((w) => {
      if (calls.length >= w.n) { w.resolve(); return false; }
      return true;
    });
    return outcome(calls[calls.length - 1]!);
  });
  const untilCalls = (n: number): Promise<void> => {
    if (calls.length >= n) return Promise.resolve();
    const { promise, resolve } = Promise.withResolvers<void>();
    waiters.push({ n, resolve });
    return promise;
  };
  return { ch, calls, untilCalls };
}

describe("InputChannel flush", () => {
  test("coalesces a burst queued behind an in-flight send into one ordered POST", async () => {
    const gates: PromiseWithResolvers<unknown>[] = [];
    const { ch, calls, untilCalls } = makeChannel(() => {
      const g = Promise.withResolvers<unknown>();
      gates.push(g);
      return g.promise;
    });

    ch.sendInput("s1", enc("a")); // POST #0 issued immediately, now in flight
    await untilCalls(1);
    expect(dec(calls[0]!.data)).toBe("a");

    ch.sendInput("s1", enc("b")); // both queue behind the in-flight #0
    ch.sendInput("s1", enc("c"));
    gates[0]!.resolve(undefined); // #0 acks → the queued run drains as one POST
    await untilCalls(2);

    expect(calls.length).toBe(2);
    expect(dec(calls[1]!.data)).toBe("bc"); // coalesced, in queue order
    expect(calls[1]!.timeoutMs).toBe(2500); // deadline plumbed through
    gates[1]!.resolve(undefined);
  });

  test("never coalesces across sessions", async () => {
    const gates: PromiseWithResolvers<unknown>[] = [];
    const { ch, calls, untilCalls } = makeChannel(() => {
      const g = Promise.withResolvers<unknown>();
      gates.push(g);
      return g.promise;
    });

    ch.sendInput("s1", enc("a")); // #0 in flight
    await untilCalls(1);
    ch.sendInput("s1", enc("b")); // queued: s1
    ch.sendInput("s2", enc("z")); // queued: s2 — must not merge with s1
    gates[0]!.resolve(undefined);
    await untilCalls(2);
    gates[1]!.resolve(undefined);
    await untilCalls(3);

    expect(calls.map((c) => c.sid)).toEqual(["s1", "s1", "s2"]);
    expect(dec(calls[1]!.data)).toBe("b");
    expect(dec(calls[2]!.data)).toBe("z");
    gates[2]!.resolve(undefined);
  });

  test("does NOT retry on a deadline — a resend would duplicate keystrokes", async () => {
    const { ch, calls, untilCalls } = makeChannel(() =>
      Promise.reject(new ConnectError("deadline", Code.DeadlineExceeded)),
    );
    ch.sendInput("s1", enc("x")); // #0 → deadline → dropped, must NOT be resent
    await untilCalls(1);
    // A retry would resend {s1,"x"} as the next call; instead the next call must
    // be this fresh, distinct frame — proving #0 was dropped, not retried.
    ch.sendInput("s2", enc("Z"));
    await untilCalls(2);
    expect(calls[1]!.sid).toBe("s2");
    expect(dec(calls[1]!.data)).toBe("Z");
  });

  test("retries once on a hard transport error", async () => {
    const { ch, calls, untilCalls } = makeChannel(() =>
      Promise.reject(new ConnectError("down", Code.Unavailable)),
    );
    ch.sendInput("s1", enc("y")); // #0 hard-fails → resend is safe (never landed)
    await untilCalls(2); // the retry issues a 2nd send for the SAME frame
    expect(dec(calls[1]!.data)).toBe("y");
  });

  test("a wedged send blocks only until it settles, then the queue drains", async () => {
    const first = Promise.withResolvers<unknown>();
    let n = 0;
    const { ch, calls, untilCalls } = makeChannel(() =>
      n++ === 0 ? first.promise : Promise.resolve(undefined),
    );

    ch.sendInput("s1", enc("a")); // #0 hangs in flight
    await untilCalls(1);
    ch.sendInput("s2", enc("z")); // queued behind the hung #0
    expect(calls.length).toBe(1); // serial drain: s2 not issued while #0 hangs
    first.resolve(undefined); // #0 settles → queue MUST progress, not wedge
    await untilCalls(2);
    expect(calls.map((c) => c.sid)).toEqual(["s1", "s2"]);
  });
});
