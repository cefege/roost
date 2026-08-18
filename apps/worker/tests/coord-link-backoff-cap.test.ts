// Regression: a dial that never fires ws.onopen is NOT proof of auth rejection.
// Coord answers a bad JWT with an HTTP 401 on the upgrade, which Bun's client
// WebSocket surfaces identically to a handshake timeout or a tailscale-serve
// 502 — so a worker throttled by its own cgroup used to hit AUTH_REJECT_THRESHOLD
// after 3 dials and arm the 5-minute backoff cap (2026-08-01: ovh1 invisible to
// coord for 361s while roost-worker.service sat over MemoryHigh). The escalation
// now keys on hasOpened: a link that has worked in this process needs a long
// streak, so a transient stall costs one 30s cap instead of five minutes.

import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// MUST precede any startCoordLink() call: ClientSeq (constructed inside
// startCoordLink) falls back to the production data dir when this is unset,
// which would corrupt the live client-seq watermark. Set at module eval,
// before any test body runs.
process.env.ROOST_WORKER_DATA_DIR = mkdtempSync(join(tmpdir(), "coordlink-backoff-test-"));
process.env.ROOST_KEEPER_QUIET = "1";

import { expect, test } from "bun:test";
import {
  AUTH_REJECT_BACKOFF_CAP_MS,
  AUTH_REJECT_THRESHOLD,
  BACKOFF_MAX_MS,
  backoffCapMs,
} from "../src/transport/coord-link-constants.ts";
import { startCoordLink } from "../src/transport/coord-link.ts";
import type { WorkerFp } from "@roost/shared/wire";

// This is the regression proper: it fails on the pre-fix code, which read every
// non-open dial as an auth rejection and returned the 5-minute cap for
// backoffCapMs(3, true). Pure, so no dials are driven to prove it.
test("backoffCapMs escalates only for links that never opened", () => {
  // Never opened = the real stale-binary / wrong-JWT-aud case: escalate at 3.
  expect(backoffCapMs(AUTH_REJECT_THRESHOLD, false)).toBe(AUTH_REJECT_BACKOFF_CAP_MS);
  expect(backoffCapMs(AUTH_REJECT_THRESHOLD - 1, false)).toBe(BACKOFF_MAX_MS);
  // Had opened: 3 failures is a stall, not a revocation.
  expect(backoffCapMs(AUTH_REJECT_THRESHOLD, true)).toBe(BACKOFF_MAX_MS);
  // A long streak after open still escalates, preserving the noise reduction
  // the escalation exists for (a permanently revoked worker settles at 5 min).
  expect(backoffCapMs(60, true)).toBe(AUTH_REJECT_BACKOFF_CAP_MS);
});

// Wiring check for the same fix: hasOpened must actually reach the cap decision
// through a real socket lifecycle. Driving the 5-minute cap through the socket
// would need ~7 dials / ~61s of real backoff, so the cap *value* is asserted by
// the pure test above; what this proves is that a link which opened and then
// lost coord keeps re-dialing on the normal ladder and never wedges.
//
// ts-no-test-timers exception: reconnect backoff is real setTimeout interleaved
// with real socket I/O, which fake timers cannot advance coherently (same reason
// as coord-link-stale-watchdog.test.ts). Every wait below is on an observed
// event — an upgrade hitting the server, or a state predicate — never a guessed
// duration.
test("link that opened then lost coord re-dials on the normal ladder", async () => {
  let opens = 0;
  let rejectedDials = 0;
  let rejectUpgrades = false;
  const { promise: opened, resolve: gotOpen } = Promise.withResolvers<void>();
  const { promise: threeRejected, resolve: gotThreeRejected } =
    Promise.withResolvers<void>();

  const server = Bun.serve({
    port: 0,
    fetch(req, s) {
      if (rejectUpgrades) {
        // Exactly what coord does to a bad JWT (worker-ws-handler.ts): HTTP 401
        // on the upgrade. The client sees this as a dial that never opened.
        rejectedDials += 1;
        if (rejectedDials >= AUTH_REJECT_THRESHOLD) gotThreeRejected();
        return new Response("unauthorized", { status: 401 });
      }
      if (s.upgrade(req)) return undefined;
      return new Response("ws only", { status: 400 });
    },
    websocket: {
      open(ws) {
        opens += 1;
        gotOpen();
        // Drop the link so the worker re-dials into the 401 wall below.
        rejectUpgrades = true;
        ws.close();
      },
      message() {},
      close() {},
    },
  });

  const link = startCoordLink({
    coordHttpUrl: `http://127.0.0.1:${server.port}`,
    workerFp: "backoff-fp" as WorkerFp,
    workerVersion: "test",
    mintJwt: async () => "jwt",
  });

  await opened;
  expect(opens).toBe(1);

  // Backoff ladder is 500/1000/2000ms, so three rejections land in ~3.5s.
  await threeRejected;

  // state() is poll-only (CoordLink exposes no state-change hook), so settle on
  // the predicate rather than a fixed wait.
  const deadline = Date.now() + 5_000;
  let st = link.state();
  while (st.kind !== "reconnecting" && Date.now() < deadline) {
    await Bun.sleep(10);
    st = link.state();
  }
  link.dispose();
  server.stop(true);

  expect(st.kind).toBe("reconnecting");
  if (st.kind !== "reconnecting") throw new Error("unreachable");
  // Still climbing the normal ladder — the link never gave up and never jumped
  // to a cap it has no business arming.
  expect(st.backoffMs).toBeGreaterThan(0);
  expect(st.backoffMs).toBeLessThanOrEqual(BACKOFF_MAX_MS);
  expect(rejectedDials).toBeGreaterThanOrEqual(AUTH_REJECT_THRESHOLD);
}, 20_000);
