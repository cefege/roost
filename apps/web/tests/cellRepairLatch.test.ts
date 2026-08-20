// Repair-latch tripwire for CellTerminal's wedged-pane defect.
//
// The bug: requestFullFrame armed `awaitingFullFrame` BEFORE knowing whether the
// repair claim transmitted. sendClaim's visibility gate diverts a claim from a
// hidden tab / warm-mounted-but-unslotted pane into sendPark() → sendWithdraw(),
// which reset viewportPositive but never the latch. The latch then sat armed
// with NO repair in flight, and the cell handler's delta gate
// (`if (!frame.full) { if (awaitingFullFrame) return; }`) dropped every
// subsequent delta forever — whole regions of a full-screen TUI froze and never
// self-healed. The retry after a renderer-rejected full frame was a guaranteed
// no-op for the same reason (re-entrancy guard swallowed it).
//
// The invariant: the latch is armed ONLY by a claim that genuinely transmitted,
// and is cleared exactly on park/withdraw, on a diverted request, on an explicit
// retry re-arm, and on a successful apply — nowhere else.
//
// This is checked structurally against the source. CellTerminal cannot be
// mounted under `bun test` (no jsdom, and Solid resolves to its SSR build where
// createEffect never fires — see stuckTerminal.test.ts), and the latch is
// closure state with no exported seam. Mirrors focusOwners.test.ts, which checks
// the same component's focus contract as a string for the same reason.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";

const SRC = readFileSync(
  new URL("../src/components/CellTerminal.tsx", import.meta.url),
  "utf8",
);

/** Byte range of the brace-delimited body that follows `marker`. The regions
 *  read below carry no braces inside strings or comments, so plain depth
 *  counting is exact. */
function region(marker: string): { start: number; end: number; body: string } {
  const at = SRC.indexOf(marker);
  if (at < 0) throw new Error(`marker not found in CellTerminal.tsx: ${marker}`);
  const open = SRC.indexOf("{", at);
  let depth = 0;
  for (let i = open; i < SRC.length; i++) {
    const ch = SRC[i];
    if (ch === "{") depth++;
    else if (ch === "}" && --depth === 0) {
      return { start: open + 1, end: i, body: SRC.slice(open + 1, i) };
    }
  }
  throw new Error(`unbalanced body for ${marker}`);
}

const sendClaimHeader = SRC.slice(
  SRC.indexOf("function sendClaim("),
  SRC.indexOf("{", SRC.indexOf("function sendClaim(")),
);
const sendClaim = region("function sendClaim(");
const sendWithdraw = region("function sendWithdraw(");
const requestFullFrame = region("const requestFullFrame = (");
const onVisibility = region("const onVisibility = (");
// The renderer-rejected-frame retry inside the cell handler.
const rejected = region("if (!applied)");
// Successful application clears the latch just past the retry block.
const applySuccess = SRC.indexOf("awaitingFullFrame = false", rejected.end);

const indicesOf = (needle: string): number[] => {
  const out: number[] = [];
  for (let i = SRC.indexOf(needle); i >= 0; i = SRC.indexOf(needle, i + 1)) out.push(i);
  return out;
};
// Assignments only — the `let awaitingFullFrame = false` declaration matches the
// same text.
const clearSites = (): number[] =>
  indicesOf("awaitingFullFrame = false").filter((i) => !SRC.startsWith("let ", i - 4));
const within = (i: number, r: { start: number; end: number }) => i > r.start && i < r.end;

describe("CellTerminal repair latch", () => {
  test("sendClaim reports whether a claim TRANSMITTED", () => {
    expect(sendClaimHeader).toContain(": boolean");
    // Every bail-out is a negative answer: a caller must never read a bail as
    // "the claim went out". A bare `return;` would type-error, but would also
    // read as success if the return type were ever widened.
    expect(sendClaim.body).not.toMatch(/\breturn\s*;/);
    // ...and only the path that actually reached the viewport owner is true.
    const claimAt = sendClaim.body.indexOf("viewportOwner.claim(");
    const trueAt = sendClaim.body.indexOf("return true;");
    expect(claimAt).toBeGreaterThan(-1);
    expect(trueAt).toBeGreaterThan(claimAt);
    expect(sendClaim.body.match(/return true;/g)).toHaveLength(1);
  });

  test("the visibility gate still parks and still refuses to claim", () => {
    // Behaviour of the gate itself is unchanged — it parks, it does not claim.
    expect(sendClaim.body).toMatch(
      /if \(!isPageVisible\(\) \|\| props\.inLayout !== true \|\| !props\.surfaceActive\) \{\s*sendPark\(\);\s*return false;/,
    );
  });

  test("sendClaimNow forwards the transmitted answer", () => {
    const now = region("function sendClaimNow(");
    expect(SRC.slice(SRC.indexOf("function sendClaimNow("), now.start)).toContain(": boolean");
    expect(now.body).toMatch(/return sendClaim\(cause, repairRequired\);/);
  });

  test("the latch is armed only by a repair claim that transmitted", () => {
    const arms = indicesOf("awaitingFullFrame = true");
    expect(arms).toHaveLength(1);
    expect(within(arms[0]!, requestFullFrame)).toBe(true);
    // Armed across the send (bounds re-entrancy), then disarmed unless the
    // repair claim actually went out.
    expect(requestFullFrame.body).toMatch(
      /if \(!sendClaimNow\(ResizeCause\.TAB_VISIBLE, true\)\) awaitingFullFrame = false;/,
    );
    // The re-entrancy guard survives: one request at a time.
    expect(requestFullFrame.body).toMatch(/if \(awaitingFullFrame\) return;/);
  });

  test("park/withdraw disarms the latch — a parked pane has no repair in flight", () => {
    // Without this, the delta gate drops every frame after reveal, forever.
    expect(sendWithdraw.body).toContain("awaitingFullFrame = false");
    expect(sendWithdraw.body).toContain("viewportPositive = false");
  });

  test("a renderer-rejected full frame is genuinely re-requested", () => {
    const clear = rejected.body.indexOf("awaitingFullFrame = false");
    const retry = rejected.body.indexOf("requestFullFrame(frame.seq)");
    expect(clear).toBeGreaterThan(-1);
    expect(retry).toBeGreaterThan(clear); // re-arm BEFORE the call, or the guard eats it
  });

  test("nothing else touches the latch: exactly four safe clear sites", () => {
    // The latch lives at COMPONENT scope, not inside onMount: sendWithdraw (a
    // component-scope function) must be able to reach it.
    const decl = SRC.indexOf("let awaitingFullFrame = false");
    expect(decl).toBeGreaterThan(-1);
    expect(decl).toBeLessThan(sendWithdraw.start);
    const clears = clearSites();
    expect(clears).toHaveLength(4);
    expect(within(clears[0]!, sendWithdraw)).toBe(true); // park: claim superseded
    expect(within(clears[1]!, requestFullFrame)).toBe(true); // diverted: never sent
    expect(within(clears[2]!, rejected)).toBe(true); // retry re-arm
    expect(clears[3]!).toBe(applySuccess); // repair landed
  });

  test("a repair claim zeroes the held watermark, so the worker cannot decline it", () => {
    // `repairRequired` never reaches the wire (sync-outbound-viewport-dispatch
    // sends cols/rows/cause/heldCellSeq only), and needsClaimSnapshot
    // (apps/worker/src/session-viewport.ts:32-36) answers a claim whose seq
    // matches the last emitted frame with NOTHING. A repair asked for with a
    // current-looking watermark would therefore transmit, arm the latch, and
    // never be answered — the same wedge by another road. 0 cannot be declined.
    expect(sendClaim.body).toMatch(
      /heldCellSeq: repairRequired \? 0 : \(renderer\?\.heldFrameSeq\(\) \?\? 0\)/,
    );
  });

  test("the reveal claim does NOT force a client-side repair phase", () => {
    // repairRequired on a reveal is inert on the wire and harmful locally: it
    // holds the accepted attempt in `repairing` until VIEWPORT_REPAIR_TIMEOUT_MS
    // schedules a retry, and an idle tab-back owes no full frame, so
    // viewportLiveReady stays false and offlineWatch raises the "not responding"
    // notice on a healthy pane. A stale pane is already snapshotted by the
    // worker; a current one needs nothing.
    expect(onVisibility.body).toMatch(/sendClaimNow\(ResizeCause\.TAB_VISIBLE\);/);
    expect(onVisibility.body).not.toMatch(/sendClaimNow\([^)]*,\s*true\)/);
    expect(onVisibility.body).toContain("sendPark();"); // hidden still parks
  });
});
