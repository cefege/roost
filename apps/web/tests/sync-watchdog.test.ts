// Regression: browser Sync-WS stale-link watchdog. When coord dies behind
// tailscale-serve the browser-side TCP stays ESTABLISHED and ws.onclose/
// onerror never fire — the terminal freezes until a manual refresh. The
// watchdog force-closes the socket after sustained silence so the reconnect
// loop re-dials with sinceEventId → backfill → recovery. Mirrors the worker
// watchdog test (apps/worker/tests/coord-link-stale-watchdog.test.ts).
//
// Real-clock integration tests: the behavior under test is real-clock silence
// detection (setInterval + performance.now) driving a real WebSocket I/O loop,
// which fake timers cannot advance coherently — the ts-no-test-timers exception
// (real platform-clock timer behavior) applies.

import { test, expect } from "bun:test";
import {
  nextRedialDelayMs,
  shouldCloseStaleLinkOnResume,
  shouldParkRedial,
  shouldRedialOnRefocus,
  startStaleWatchdog,
  SYNC_HIDDEN_PARK_FAILURES,
  SYNC_REDIAL_BASE_MS,
  SYNC_REDIAL_MAX_MS,
  SYNC_REFOCUS_STALE_MS,
  type StaleWatchdog,
} from "../src/store/sync-watchdog.ts";

// ─── Test 1: silent server → watchdog force-closes ──────────────────────

test("silent server → watchdog force-closes", async () => {
  const server = Bun.serve({
    port: 0,
    fetch(req, s) {
      if (s.upgrade(req)) return;
      return new Response("ws only", { status: 400 });
    },
    websocket: {
      open() { /* stay silent — never send a downstream frame */ },
      message() {},
      close() {},
    },
  });
  const ws = new WebSocket(`ws://127.0.0.1:${server.port}`);
  const { promise: stale, resolve: gotStale } = Promise.withResolvers<void>();
  const { promise: watchdogStarted, resolve: resolveWatchdog } = Promise.withResolvers<StaleWatchdog>();
  ws.onopen = () => {
    resolveWatchdog(startStaleWatchdog(ws, {
      staleMs: 300, checkMs: 50,
      isVisible: () => true,
      onStale: () => gotStale(),
    }));
  };
  const watchdog = await watchdogStarted;
  await stale;
  watchdog.stop();
  try { ws.close(); } catch { /* ignore */ }
  server.stop(true);
}, 10_000);

// ─── Test 2: pinged link stays up — no false-positive close ──────────────

test("server sends keepalive messages → ws stays open", async () => {
  let pingTimer: Timer | undefined;
  let ticks = 0;
  const { promise: sawEnoughPings, resolve: enough } = Promise.withResolvers<void>();
  const server = Bun.serve({
    port: 0,
    fetch(req, s) {
      if (s.upgrade(req)) return;
      return new Response("ws only", { status: 400 });
    },
    websocket: {
      open(ws) {
        // Real interval — this IS the behavior under test: coord's keepalive
        // keeps lastMsgAt fresh so the watchdog never false-fires. Faster
        // cadence (100ms) scaled to the test's 300ms stale timeout.
        pingTimer = setInterval(() => {
          try { ws.send("keepalive"); } catch { /* socket closed */ }
          ticks += 1;
          // ~1.5s of pings = 5× the 300ms stale timeout: ample window for a
          // false-positive close to have surfaced.
          if (ticks >= 15) enough();
        }, 100);
      },
      message() {},
      close() { clearInterval(pingTimer); },
    },
  });
  const ws = new WebSocket(`ws://127.0.0.1:${server.port}`);
  let gotClose = false;
  let onStaleCalled = false;
  const { promise: watchdogStarted, resolve: resolveWatchdog } = Promise.withResolvers<StaleWatchdog>();
  ws.onclose = () => { gotClose = true; };
  ws.onopen = () => {
    resolveWatchdog(startStaleWatchdog(ws, {
      staleMs: 300, checkMs: 50,
      onStale: () => { onStaleCalled = true; },
    }));
  };
  const watchdog = await watchdogStarted;
  await sawEnoughPings;
  expect(gotClose).toBe(false);
  expect(onStaleCalled).toBe(false);
  watchdog.stop();
  clearInterval(pingTimer);
  try { ws.close(); } catch { /* ignore */ }
  server.stop(true);
}, 10_000);

// ─── Test 3: hidden tab → watchdog does not close (visibility owns it) ───
// Exception to ts-no-test-timers: proving a negative (no close fired) has no
// positive signal to await. Wait past the stale window on the real clock.

test("isVisible false → no close even past stale window", async () => {
  const server = Bun.serve({
    port: 0,
    fetch(req, s) {
      if (s.upgrade(req)) return;
      return new Response("ws only", { status: 400 });
    },
    websocket: {
      open() { /* stay silent */ },
      message() {},
      close() {},
    },
  });
  const ws = new WebSocket(`ws://127.0.0.1:${server.port}`);
  let gotClose = false;
  let onStaleCalled = false;
  const { promise: watchdogStarted, resolve: resolveWatchdog } = Promise.withResolvers<StaleWatchdog>();
  const { promise: waited, resolve: done } = Promise.withResolvers<void>();
  ws.onclose = () => { gotClose = true; done(); };
  ws.onopen = () => {
    resolveWatchdog(startStaleWatchdog(ws, {
      staleMs: 300, checkMs: 50,
      isVisible: () => false,
      onStale: () => { onStaleCalled = true; },
    }));
    // Race: if close fires early (watchdog regression) the onclose handler
    // resolves `waited` immediately and the assertions fail. Otherwise the
    // timeout resolves it after 1s — 3× the stale window — proving no close.
    setTimeout(done, 1_000);
  };
  const watchdog = await watchdogStarted;
  await waited;
  expect(gotClose).toBe(false);
  expect(onStaleCalled).toBe(false);
  watchdog.stop();
  try { ws.close(); } catch { /* ignore */ }
  server.stop(true);
}, 10_000);

class FakeWatchdogSocket extends EventTarget {
  closeCount = 0;
  close(): void { this.closeCount += 1; }
}

test("watchdog timestamps and cleanup are generation-local", async () => {
  const socketA = new FakeWatchdogSocket();
  const socketB = new FakeWatchdogSocket();
  const wsA = socketA as unknown as WebSocket;
  const wsB = socketB as unknown as WebSocket;
  const { promise: staleB, resolve: resolveStaleB } = Promise.withResolvers<void>();
  const watchdogA = startStaleWatchdog(wsA, {
    staleMs: 100,
    checkMs: 10,
    isVisible: () => true,
  });
  const watchdogB = startStaleWatchdog(wsB, {
    staleMs: 100,
    checkMs: 10,
    isVisible: () => true,
    onStale: resolveStaleB,
  });
  try {
    await Bun.sleep(25);
    socketB.dispatchEvent(new Event("message"));
    expect(watchdogA.idleMs()).toBeGreaterThan(watchdogB.idleMs());

    // Generation A cleanup owns only A's interval/listener. B must still age
    // independently and fire its stale close.
    watchdogA.stop();
    await staleB;
    expect(socketA.closeCount).toBe(0);
    expect(socketB.closeCount).toBeGreaterThan(0);
  } finally {
    watchdogA.stop();
    watchdogB.stop();
  }
});

// ─── Test 4: refocus re-dial decision ────────────────────────────────────
// A returning tab must KEEP a live socket (a re-dial puts a JWT sign, a TLS
// handshake and the since= backfill ahead of the terminal's reveal frame) and
// drop a silent one. The budget straddles coord's 30s Sync keepalive so one
// dropped keepalive is tolerated and a suspended/half-open socket is not.

test("shouldRedialOnRefocus: keeps a live link, re-dials a silent one", () => {
  expect(SYNC_REFOCUS_STALE_MS).toBeGreaterThan(30_000); // one dropped keepalive
  expect(shouldRedialOnRefocus(0)).toBe(false);
  expect(shouldRedialOnRefocus(SYNC_REFOCUS_STALE_MS - 1)).toBe(false);
  expect(shouldRedialOnRefocus(SYNC_REFOCUS_STALE_MS)).toBe(false);
  expect(shouldRedialOnRefocus(SYNC_REFOCUS_STALE_MS + 1)).toBe(true);
  // No socket OPEN → the link's idle read reports Infinity → always re-dial.
  expect(shouldRedialOnRefocus(Number.POSITIVE_INFINITY)).toBe(true);
});

// ─── Test 5: redial policy ───────────────────────────────────────────────
// The retired policy stopped dialing after eight failures, so a visible page
// could end up with no socket and nothing scheduled — recoverable only by a
// reload. The delay is capped; the attempt count never is.

test("nextRedialDelayMs: capped exponential, never zero, never unbounded", () => {
  expect(nextRedialDelayMs(0)).toBe(SYNC_REDIAL_BASE_MS);
  expect(nextRedialDelayMs(1)).toBe(SYNC_REDIAL_BASE_MS);
  expect(nextRedialDelayMs(2)).toBe(2 * SYNC_REDIAL_BASE_MS);
  expect(nextRedialDelayMs(3)).toBe(4 * SYNC_REDIAL_BASE_MS);
  expect(nextRedialDelayMs(4)).toBe(8 * SYNC_REDIAL_BASE_MS);
  expect(nextRedialDelayMs(5)).toBe(16 * SYNC_REDIAL_BASE_MS);
  expect(nextRedialDelayMs(6)).toBe(SYNC_REDIAL_MAX_MS);
  // Every further failure keeps a finite, capped delay: the loop still dials.
  for (const failures of [SYNC_HIDDEN_PARK_FAILURES, 40, 1_000, 1e6]) {
    expect(nextRedialDelayMs(failures)).toBe(SYNC_REDIAL_MAX_MS);
  }
});

test("shouldParkRedial: only a hidden document sleeps", () => {
  expect(shouldParkRedial(SYNC_HIDDEN_PARK_FAILURES, true)).toBe(false);
  expect(shouldParkRedial(1e6, true)).toBe(false);
  expect(shouldParkRedial(SYNC_HIDDEN_PARK_FAILURES - 1, false)).toBe(false);
  expect(shouldParkRedial(SYNC_HIDDEN_PARK_FAILURES, false)).toBe(true);
});

test("shouldCloseStaleLinkOnResume: replaces only a silent OPEN socket", () => {
  expect(shouldCloseStaleLinkOnResume("open", SYNC_REFOCUS_STALE_MS + 1)).toBe(true);
  expect(shouldCloseStaleLinkOnResume("open", 0)).toBe(false);
  // A dial in flight already IS the redial, and nothing to close means the loop
  // is dialing or sleeping: either way a resume must not manufacture a second
  // generation out of the Infinity idle reading.
  expect(shouldCloseStaleLinkOnResume("dialing", Number.POSITIVE_INFINITY)).toBe(false);
  expect(shouldCloseStaleLinkOnResume("none", Number.POSITIVE_INFINITY)).toBe(false);
});
