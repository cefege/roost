// Boundary tests for one admitted attachment port's finite authority lease.
// A short grant governs a new hello only; this owner separately enforces idle
// and hard expiry while valid activity keeps a long upload alive.

import { expect, test } from "bun:test";
import {
  ATTACHMENT_TRANSFER_ACTIVE_MAX_MS,
  ATTACHMENT_TRANSFER_IDLE_MS,
} from "@roost/protocol/attachment-transfer";
import { AttachmentTransferLease } from "../src/attachment-transfer-lease.ts";

interface Timer {
  callback: () => void;
  dueMs: number;
  cleared: boolean;
}

function leaseFixture() {
  let now = 0;
  let expired = 0;
  const timers: Timer[] = [];
  const lease = new AttachmentTransferLease({
    now: () => now,
    onExpired: () => { expired += 1; },
    scheduleTimeout: (callback, delayMs) => {
      const timer: Timer = { callback, dueMs: now + delayMs, cleared: false };
      timers.push(timer);
      return timer as unknown as NodeJS.Timeout;
    },
    clearTimeout: (timer) => { (timer as unknown as Timer).cleared = true; },
  });
  return {
    lease,
    expired: () => expired,
    advance(value: number): void { now = value; },
    fireDue(): void {
      for (const timer of timers) {
        if (!timer.cleared && timer.dueMs <= now) {
          timer.cleared = true;
          timer.callback();
        }
      }
    },
  };
}

test("expires exactly at the active lease idle boundary", () => {
  const fixture = leaseFixture();
  fixture.lease.start();

  fixture.advance(ATTACHMENT_TRANSFER_IDLE_MS - 1);
  expect(fixture.lease.allowsActivity()).toBe(true);
  fixture.advance(ATTACHMENT_TRANSFER_IDLE_MS);
  fixture.fireDue();

  expect(fixture.lease.allowsActivity()).toBe(false);
  expect(fixture.expired()).toBe(1);
});

test("valid activity refreshes idle but cannot extend the hard lease", () => {
  const fixture = leaseFixture();
  fixture.lease.start();

  for (let now = ATTACHMENT_TRANSFER_IDLE_MS - 1; now < ATTACHMENT_TRANSFER_ACTIVE_MAX_MS; now += ATTACHMENT_TRANSFER_IDLE_MS - 1) {
    fixture.advance(now);
    expect(fixture.lease.noteValidActivity()).toBe(true);
  }
  fixture.advance(ATTACHMENT_TRANSFER_ACTIVE_MAX_MS);

  expect(fixture.lease.noteValidActivity()).toBe(false);
  expect(fixture.expired()).toBe(1);
});
