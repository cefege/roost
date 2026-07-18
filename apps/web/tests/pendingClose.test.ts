// pendingClose.test.ts — the soft-close undo queue. Covers the onUndo routing
// added for pane-restore: undoOne/undoAll fire onUndo and NEVER killNow, and the
// published view carries the three snackbar labels. See lib/pendingClose.ts.

import { describe, it, expect, afterEach } from "bun:test";
import { scheduleClose, undoOne, undoAll, pendingCloses } from "../src/lib/pendingClose.ts";

afterEach(() => undoAll()); // clear any lingering timers + queue between tests

describe("pendingClose", () => {
  it("publishes the three snackbar labels", () => {
    let killed = false;
    scheduleClose("s1", { terminalName: "Terminal 3", folder: "idea", server: "m1-old" }, () => { killed = true; });
    const views = pendingCloses();
    expect(views.length).toBe(1);
    expect(views[0]).toMatchObject({ sessionId: "s1", terminalName: "Terminal 3", folder: "idea", server: "m1-old" });
    expect(killed).toBe(false); // kill is deferred to the timer, not fired on schedule
  });

  it("undoOne fires onUndo, never killNow, and clears the row", () => {
    let killed = false;
    let restored = 0;
    scheduleClose("s2", { terminalName: "t", folder: "f", server: "sv" }, () => { killed = true; }, () => { restored++; });
    undoOne("s2");
    expect(restored).toBe(1);
    expect(killed).toBe(false);
    expect(pendingCloses().length).toBe(0);
  });

  it("undoOne without onUndo is a clean no-op restore (sidebar path)", () => {
    scheduleClose("s3", { terminalName: "t", folder: "f", server: "sv" }, () => {});
    expect(() => undoOne("s3")).not.toThrow();
    expect(pendingCloses().length).toBe(0);
  });

  it("undoAll fires every pending onUndo", () => {
    let a = 0, b = 0;
    scheduleClose("s4", { terminalName: "t", folder: "f", server: "sv" }, () => {}, () => { a++; });
    scheduleClose("s5", { terminalName: "t", folder: "f", server: "sv" }, () => {}, () => { b++; });
    undoAll();
    expect(a).toBe(1);
    expect(b).toBe(1);
    expect(pendingCloses().length).toBe(0);
  });
});
