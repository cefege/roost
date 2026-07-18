// Regression: browser Sync-WS stale-link watchdog. When coord dies behind
// tailscale-serve the browser-side TCP stays ESTABLISHED and ws.onclose/
// onerror never fire — the terminal freezes until a manual refresh. The
// watchdog force-closes the socket after sustained silence so the reconnect
// loop re-dials with sinceEventId → backfill → recovery. Mirrors the worker
// watchdog test (apps/worker/tests/coord-link-stale-watchdog.test.ts).
//
// Real-clock integration tests: the behavior under test is real-clock silence
// detection (setInterval + Date.now) driving a real WebSocket I/O loop, which
// fake timers cannot advance coherently — the ts-no-test-timers exception
// (real platform-clock timer behavior) applies.

import { test, expect } from "bun:test";
import { startStaleWatchdog } from "../src/store/sync-watchdog.ts";

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
  ws.onopen = () => {
    startStaleWatchdog(ws, {
      staleMs: 300, checkMs: 50,
      isVisible: () => true,
      onStale: () => gotStale(),
    });
  };
  await stale;
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
  ws.onclose = () => { gotClose = true; };
  ws.onopen = () => {
    startStaleWatchdog(ws, {
      staleMs: 300, checkMs: 50,
      onStale: () => { onStaleCalled = true; },
    });
  };
  await sawEnoughPings;
  expect(gotClose).toBe(false);
  expect(onStaleCalled).toBe(false);
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
  const { promise: waited, resolve: done } = Promise.withResolvers<void>();
  ws.onclose = () => { gotClose = true; done(); };
  ws.onopen = () => {
    startStaleWatchdog(ws, {
      staleMs: 300, checkMs: 50,
      isVisible: () => false,
      onStale: () => { onStaleCalled = true; },
    });
    // Race: if close fires early (watchdog regression) the onclose handler
    // resolves `waited` immediately and the assertions fail. Otherwise the
    // timeout resolves it after 1s — 3× the stale window — proving no close.
    setTimeout(done, 1_000);
  };
  await waited;
  expect(gotClose).toBe(false);
  expect(onStaleCalled).toBe(false);
  try { ws.close(); } catch { /* ignore */ }
  server.stop(true);
}, 10_000);
