import { describe, expect, test } from "bun:test";
import { CoordinatorMovePhase } from "@roost/shared/proto/coordinator_pb";
import { parseFragmentCredential } from "../src/auth/fragment-credential.ts";
import { coordinatorMoveControlsVisible, coordinatorRole, isFailedMovePhase, moveDialogCanClose, MOVE_POLL_FAILURE_LIMIT, type CoordinatorRole } from "../src/lib/coordinatorMove.ts";

describe("coordinator move browser contracts", () => {
  test("managed mode hides self-hosted move controls while self-hosted stays unchanged", () => {
    expect(coordinatorMoveControlsVisible({ saas_mode: true })).toBe(false);
    expect(coordinatorMoveControlsVisible({ saas_mode: false })).toBe(true);
    expect(coordinatorMoveControlsVisible(undefined)).toBe(true);
  });

  test("requires both opaque relocation fragment fields", () => {
    expect(parseFragmentCredential("/", "#move=token&handoff=handoff-id"))
      .toEqual({ kind: "relocation", token: "token", handoffId: "handoff-id" });
    expect(parseFragmentCredential("/", "#move=token")).toEqual({ kind: "invalid" });
    expect(parseFragmentCredential("/", "#handoff=handoff-id")).toEqual({ kind: "invalid" });
  });

  test("a committed move is always escapable", () => {
    // The poll stops at COMMITTED, so pollFailures can never reach the limit and
    // manualFallback only flips on a failed mint: without an explicit COMMITTED
    // case a redirect that never lands leaves a modal with no way out.
    expect(moveDialogCanClose({ started: true, phase: CoordinatorMovePhase.COMMITTED, manualFallback: false, pollFailures: 0 })).toBe(true);
  });

  test("an unstarted, in-flight or terminal move closes on the same rules as before", () => {
    expect(moveDialogCanClose({ started: false, phase: null, manualFallback: false, pollFailures: 0 })).toBe(true);
    expect(moveDialogCanClose({ started: true, phase: CoordinatorMovePhase.COPYING_STATE, manualFallback: false, pollFailures: 0 })).toBe(false);
    expect(moveDialogCanClose({ started: true, phase: CoordinatorMovePhase.COPYING_STATE, manualFallback: true, pollFailures: 0 })).toBe(true);
    expect(moveDialogCanClose({ started: true, phase: CoordinatorMovePhase.ROLLED_BACK, manualFallback: false, pollFailures: 0 })).toBe(true);
    expect(moveDialogCanClose({ started: true, phase: CoordinatorMovePhase.FAILED, manualFallback: false, pollFailures: 0 })).toBe(true);
    expect(moveDialogCanClose({ started: true, phase: CoordinatorMovePhase.COPYING_STATE, manualFallback: false, pollFailures: MOVE_POLL_FAILURE_LIMIT })).toBe(true);
  });

  test("only rollback and failure count as a failed phase", () => {
    expect(isFailedMovePhase(CoordinatorMovePhase.ROLLED_BACK)).toBe(true);
    expect(isFailedMovePhase(CoordinatorMovePhase.FAILED)).toBe(true);
    expect(isFailedMovePhase(CoordinatorMovePhase.COMMITTED)).toBe(false);
    expect(isFailedMovePhase(CoordinatorMovePhase.ROLLING_BACK)).toBe(false);
  });

  // `public_url` comes straight from ROOST_COORDINATOR_PUBLIC_URL and
  // `reachable_addr` from ROOST_REACHABLE_ADDR — neither is normalised, so the
  // comparison has to absorb a MagicDNS trailing dot and a :port suffix.
  const roleCases: Array<{ name: string; publicUrl: string | null | undefined; identity: boolean; addr: string | null | undefined; want: CoordinatorRole }> = [
    { name: "trailing dot on the coord url", publicUrl: "https://mac.ts.net./", identity: true, addr: "mac.ts.net", want: "yes" },
    { name: "trailing dot on the worker addr", publicUrl: "https://mac.ts.net/", identity: true, addr: "mac.ts.net.", want: "yes" },
    { name: "port on the worker addr", publicUrl: "https://mac.ts.net/", identity: true, addr: "mac.ts.net:4102", want: "yes" },
    { name: "trailing dot and port together", publicUrl: "https://mac.ts.net./", identity: true, addr: "MAC.ts.net.:4102", want: "yes" },
    { name: "a genuinely different host", publicUrl: "https://mac.ts.net/", identity: true, addr: "mini.ts.net", want: "no" },
    { name: "no reachable addr", publicUrl: "https://mac.ts.net/", identity: true, addr: null, want: "no" },
    { name: "null identity", publicUrl: null, identity: false, addr: "mac.ts.net", want: "unknown" },
    { name: "identity without a public url", publicUrl: "", identity: true, addr: "mac.ts.net", want: "unknown" },
    { name: "unparseable public url", publicUrl: "not a url", identity: true, addr: "mac.ts.net", want: "unknown" },
  ];

  for (const testCase of roleCases) {
    test(`coordinatorRole: ${testCase.name} → ${testCase.want}`, () => {
      const identity = testCase.identity ? { public_url: testCase.publicUrl } : null;
      expect(coordinatorRole(identity, { reachable_addr: testCase.addr })).toBe(testCase.want);
    });
  }
});
