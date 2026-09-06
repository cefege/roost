// Pins the live target and pending-correlation invariants for acknowledged UI layout apply.
// Bun discovers this file directly; each test constructs a fresh owner and deterministic clock.
// It covers every identity fence, synchronous publication races, and terminal cleanup path.
// No database or WebSocket fake is needed because this is the focused ownership boundary.

import { describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  UiApplyLayoutOutcome,
  UiApplyLayoutResultSchema,
} from "@roost/shared/proto/sync_pb";
import {
  UI_LAYOUT_REJECTED_REASON_MAX_LENGTH,
  UI_LAYOUT_TARGET_GONE_REASON,
  UiLayoutApplyCanceledError,
  UiLayoutApplyCapacityError,
  UiLayoutApplyOwner,
  type UiLayoutApplyClock,
  type UiLayoutApplyPublication,
  type UiLayoutApplyTarget,
} from "../src/connect/ui-layout-apply-owner.ts";

interface ClockTimer {
  readonly atMs: number;
  readonly callback: () => void;
}

class ApplyClock implements UiLayoutApplyClock {
  nowMs = 0;
  nextTimer = 1;
  readonly timers = new Map<number, ClockTimer>();

  setTimeout(callback: () => void, delayMs: number): Timer {
    const id = this.nextTimer++;
    this.timers.set(id, { atMs: this.nowMs + delayMs, callback });
    return id as unknown as Timer;
  }

  clearTimeout(timer: Timer): void {
    this.timers.delete(timer as unknown as number);
  }

  advance(ms: number): void {
    const targetMs = this.nowMs + ms;
    while (true) {
      const next = [...this.timers.entries()]
        .filter(([, timer]) => timer.atMs <= targetMs)
        .sort((left, right) => left[1].atMs - right[1].atMs)[0];
      if (!next) break;
      this.timers.delete(next[0]);
      this.nowMs = next[1].atMs;
      next[1].callback();
    }
    this.nowMs = targetMs;
  }
}

const BASE_TARGET: UiLayoutApplyTarget = {
  dashboardId: "dashboard-a",
  fingerprint: "fingerprint-a",
  tabId: "tab-a",
  socketId: "socket-a",
};

function result(
  correlationId: string,
  outcome: UiApplyLayoutOutcome,
  reason?: string,
) {
  return create(UiApplyLayoutResultSchema, { correlationId, outcome, reason });
}

function makeOwner(maxPending = 256) {
  const clock = new ApplyClock();
  let nextCorrelation = 0;
  const owner = new UiLayoutApplyOwner({
    clock,
    timeoutMs: 100,
    maxPending,
    createCorrelationId: () => `correlation-${++nextCorrelation}`,
  });
  return { clock, owner };
}

function beginPending(
  owner: UiLayoutApplyOwner,
  target = BASE_TARGET,
  controller = new AbortController(),
) {
  let publication: UiLayoutApplyPublication | undefined;
  const promise = owner.requestApply(
    target.dashboardId,
    target.fingerprint,
    target.tabId,
    controller.signal,
    (published) => { publication = published; },
  );
  if (!publication) throw new Error("layout apply was not published");
  return { controller, promise, publication };
}

describe("UiLayoutApplyOwner result admission", () => {
  test("registers pending before publish so an immediate applied ACK wins", async () => {
    const { clock, owner } = makeOwner();
    owner.registerTarget(BASE_TARGET);
    const response = await owner.requestApply(
      BASE_TARGET.dashboardId,
      BASE_TARGET.fingerprint,
      BASE_TARGET.tabId,
      new AbortController().signal,
      (publication) => {
        expect(owner.stats()).toEqual({ targets: 1, pending: 1 });
        expect(owner.acceptResult(BASE_TARGET, result(
          publication.correlationId,
          UiApplyLayoutOutcome.APPLIED,
        ))).toBe(true);
      },
    );
    expect(response).toEqual({
      outcome: UiApplyLayoutOutcome.APPLIED,
      correlationId: "correlation-1",
    });
    expect(owner.stats().pending).toBe(0);
    expect(clock.timers.size).toBe(0);
  });

  test("returns only a bounded control-free rejected reason", async () => {
    const { clock, owner } = makeOwner();
    owner.registerTarget(BASE_TARGET);
    const rawReason = ` rejected\n\u0000${"x".repeat(300)}`;
    const pending = beginPending(owner);
    expect(owner.acceptResult(BASE_TARGET, result(
      pending.publication.correlationId,
      UiApplyLayoutOutcome.REJECTED,
      rawReason,
    ))).toBe(true);
    const response = await pending.promise;
    expect(response.outcome).toBe(UiApplyLayoutOutcome.REJECTED);
    expect(response.reason).not.toMatch(/[\p{Cc}\p{Cf}]/u);
    expect([...(response.reason ?? "")].length)
      .toBe(UI_LAYOUT_REJECTED_REASON_MAX_LENGTH);
    expect(clock.timers.size).toBe(0);

    const fallbackPending = beginPending(owner);
    expect(owner.acceptResult(BASE_TARGET, result(
      fallbackPending.publication.correlationId,
      UiApplyLayoutOutcome.REJECTED,
      "\u0000\n",
    ))).toBe(true);
    expect(await fallbackPending.promise).toEqual({
      outcome: UiApplyLayoutOutcome.REJECTED,
      correlationId: "correlation-2",
      reason: "layout apply rejected",
    });
  });

  test("ignores every wrong fence, unsupported outcome, duplicate, and late result", async () => {
    const { clock, owner } = makeOwner();
    owner.registerTarget(BASE_TARGET);
    const pending = beginPending(owner);
    const correlationId = pending.publication.correlationId;
    for (const source of [
      { ...BASE_TARGET, dashboardId: "dashboard-b" },
      { ...BASE_TARGET, fingerprint: "fingerprint-b" },
      { ...BASE_TARGET, tabId: "tab-b" },
      { ...BASE_TARGET, socketId: "socket-b" },
    ]) {
      expect(owner.acceptResult(source, result(
        correlationId,
        UiApplyLayoutOutcome.APPLIED,
      ))).toBe(false);
    }
    expect(owner.acceptResult(BASE_TARGET, result(
      "wrong-correlation",
      UiApplyLayoutOutcome.APPLIED,
    ))).toBe(false);
    expect(owner.acceptResult(BASE_TARGET, result(
      correlationId,
      UiApplyLayoutOutcome.UNSPECIFIED,
    ))).toBe(false);
    expect(owner.acceptResult(BASE_TARGET, result(
      correlationId,
      UiApplyLayoutOutcome.TARGET_GONE,
    ))).toBe(false);
    expect(owner.stats().pending).toBe(1);

    const accepted = result(correlationId, UiApplyLayoutOutcome.APPLIED);
    expect(owner.acceptResult(BASE_TARGET, accepted)).toBe(true);
    expect(owner.acceptResult(BASE_TARGET, accepted)).toBe(false);
    expect((await pending.promise).outcome).toBe(UiApplyLayoutOutcome.APPLIED);
    clock.advance(1_000);
    expect(owner.acceptResult(BASE_TARGET, accepted)).toBe(false);
    expect(owner.stats().pending).toBe(0);
  });
});

describe("UiLayoutApplyOwner target selection", () => {
  test("returns target-gone without publishing when no live socket exists", async () => {
    const { owner } = makeOwner();
    let publications = 0;
    const response = await owner.requestApply(
      BASE_TARGET.dashboardId,
      BASE_TARGET.fingerprint,
      BASE_TARGET.tabId,
      new AbortController().signal,
      () => { publications += 1; },
    );
    expect(response).toEqual({
      outcome: UiApplyLayoutOutcome.TARGET_GONE,
      correlationId: "correlation-1",
      reason: UI_LAYOUT_TARGET_GONE_REASON,
    });
    expect(publications).toBe(0);
  });
  test("same tab across fingerprints reserves only the requested device", async () => {
    const { owner } = makeOwner();
    owner.registerTarget(BASE_TARGET);
    const attacker = {
      ...BASE_TARGET,
      fingerprint: "fingerprint-b",
      socketId: "socket-b",
    };
    owner.registerTarget(attacker);
    const pending = beginPending(owner);
    expect(pending.publication.socketId).toBe(BASE_TARGET.socketId);
    expect(owner.acceptResult(attacker, result(
      pending.publication.correlationId,
      UiApplyLayoutOutcome.APPLIED,
    ))).toBe(false);
    expect(owner.acceptResult(BASE_TARGET, result(
      pending.publication.correlationId,
      UiApplyLayoutOutcome.APPLIED,
    ))).toBe(true);
    expect((await pending.promise).outcome).toBe(UiApplyLayoutOutcome.APPLIED);
  });

  test("victim close cannot transfer its exact target to a colliding attacker", async () => {
    const { owner } = makeOwner();
    const closeVictim = owner.registerTarget(BASE_TARGET);
    owner.registerTarget({
      ...BASE_TARGET,
      fingerprint: "fingerprint-b",
      socketId: "socket-b",
    });
    closeVictim();
    let publications = 0;
    const response = await owner.requestApply(
      BASE_TARGET.dashboardId,
      BASE_TARGET.fingerprint,
      BASE_TARGET.tabId,
      new AbortController().signal,
      () => { publications += 1; },
    );
    expect(response.outcome).toBe(UiApplyLayoutOutcome.TARGET_GONE);
    expect(publications).toBe(0);
  });

  test("same tab id in another dashboard does not collide", async () => {
    const { owner } = makeOwner();
    owner.registerTarget(BASE_TARGET);
    owner.registerTarget({
      ...BASE_TARGET,
      dashboardId: "dashboard-b",
      fingerprint: "fingerprint-b",
      socketId: "socket-b",
    });
    const pending = beginPending(owner);
    expect(pending.publication.socketId).toBe(BASE_TARGET.socketId);
    expect(owner.acceptResult(BASE_TARGET, result(
      pending.publication.correlationId,
      UiApplyLayoutOutcome.APPLIED,
    ))).toBe(true);
    await pending.promise;
  });
});

describe("UiLayoutApplyOwner terminal cleanup", () => {
  test("replacement settles the old generation and its disposer preserves the new one", async () => {
    const { owner } = makeOwner();
    const disposeOld = owner.registerTarget(BASE_TARGET);
    const oldPending = beginPending(owner);
    const replacement = { ...BASE_TARGET, socketId: "socket-new" };
    const disposeNew = owner.registerTarget(replacement);
    expect((await oldPending.promise).outcome).toBe(UiApplyLayoutOutcome.TARGET_GONE);
    disposeOld();
    expect(owner.stats().targets).toBe(1);

    const newPending = beginPending(owner, replacement);
    disposeNew();
    expect((await newPending.promise).outcome).toBe(UiApplyLayoutOutcome.TARGET_GONE);
    expect(owner.stats()).toEqual({ targets: 0, pending: 0 });
  });

  test("close settlement matches every identity fence even when socket ids collide", async () => {
    const { owner } = makeOwner();
    const closeBase = owner.registerTarget(BASE_TARGET);
    const peer = {
      ...BASE_TARGET,
      fingerprint: "fingerprint-peer",
      tabId: "tab-peer",
    };
    owner.registerTarget(peer);
    const basePending = beginPending(owner);
    const peerPending = beginPending(owner, peer);

    closeBase();
    expect((await basePending.promise).outcome).toBe(UiApplyLayoutOutcome.TARGET_GONE);
    expect(owner.stats().pending).toBe(1);
    expect(owner.acceptResult(peer, result(
      peerPending.publication.correlationId,
      UiApplyLayoutOutcome.APPLIED,
    ))).toBe(true);
    expect((await peerPending.promise).outcome).toBe(UiApplyLayoutOutcome.APPLIED);
  });

  test("timeout settles target-gone and removes its timer", async () => {
    const { clock, owner } = makeOwner();
    owner.registerTarget(BASE_TARGET);
    const pending = beginPending(owner);
    clock.advance(99);
    expect(owner.stats().pending).toBe(1);
    clock.advance(1);
    expect(await pending.promise).toMatchObject({
      outcome: UiApplyLayoutOutcome.TARGET_GONE,
      reason: UI_LAYOUT_TARGET_GONE_REASON,
    });
    expect(owner.stats().pending).toBe(0);
    expect(clock.timers.size).toBe(0);
  });

  test("cancellation removes timer and listener and rejects even before target lookup", async () => {
    const { clock, owner } = makeOwner();
    owner.registerTarget(BASE_TARGET);
    const pending = beginPending(owner);
    pending.controller.abort();
    await expect(pending.promise).rejects.toBeInstanceOf(UiLayoutApplyCanceledError);
    expect(owner.stats().pending).toBe(0);
    expect(clock.timers.size).toBe(0);
    expect(owner.acceptResult(BASE_TARGET, result(
      pending.publication.correlationId,
      UiApplyLayoutOutcome.APPLIED,
    ))).toBe(false);

    const preAborted = new AbortController();
    preAborted.abort();
    await expect(owner.requestApply(
      "missing-dashboard",
      "missing-fingerprint",
      "missing-tab",
      preAborted.signal,
      () => { throw new Error("must not publish"); },
    )).rejects.toBeInstanceOf(UiLayoutApplyCanceledError);
  });

  test("bounded capacity rejects admission without publishing a second command", async () => {
    const { clock, owner } = makeOwner(1);
    owner.registerTarget(BASE_TARGET);
    const first = beginPending(owner);
    let secondPublished = false;
    await expect(owner.requestApply(
      BASE_TARGET.dashboardId,
      BASE_TARGET.fingerprint,
      BASE_TARGET.tabId,
      new AbortController().signal,
      () => { secondPublished = true; },
    )).rejects.toBeInstanceOf(UiLayoutApplyCapacityError);
    expect(secondPublished).toBe(false);
    first.controller.abort();
    await expect(first.promise).rejects.toBeInstanceOf(UiLayoutApplyCanceledError);
    expect(owner.stats().pending).toBe(0);
    expect(clock.timers.size).toBe(0);
  });
});
