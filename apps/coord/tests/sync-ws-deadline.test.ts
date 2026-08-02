import { describe, expect, test } from "bun:test";
import {
  scheduleDeadline,
  type SyncDeadlineClock,
} from "../src/connect/sync-ws-handler.ts";

interface PendingTimer {
  at: number;
  callback: () => void;
}

class FakeClock implements SyncDeadlineClock {
  nowMs = 0;
  maxDelayMs = 50;
  nextId = 1;
  timers = new Map<number, PendingTimer>();

  now(): number {
    return this.nowMs;
  }

  setTimeout(callback: () => void, delayMs: number): Timer {
    const id = this.nextId++;
    this.timers.set(id, { at: this.nowMs + delayMs, callback });
    return id as unknown as Timer;
  }

  clearTimeout(timer: Timer): void {
    this.timers.delete(timer as unknown as number);
  }

  advance(ms: number): void {
    const target = this.nowMs + ms;
    while (true) {
      const due = [...this.timers.entries()]
        .filter(([, timer]) => timer.at <= target)
        .sort((a, b) => a[1].at - b[1].at)[0];
      if (!due) break;
      this.timers.delete(due[0]);
      this.nowMs = due[1].at;
      due[1].callback();
    }
    this.nowMs = target;
  }
}

describe("public Sync identity deadline", () => {
  test("closes at Access expiry with the reauth code and reason", () => {
    const clock = new FakeClock();
    const closes: Array<[number | undefined, string | undefined]> = [];
    scheduleDeadline({ close: (code, reason) => { closes.push([code, reason]); } }, 100, clock);
    clock.advance(99);
    expect(closes).toEqual([]);
    clock.advance(1);
    expect(closes).toEqual([[4003, "reauth required"]]);
  });

  test("re-arms deadlines beyond the platform timer maximum", () => {
    const clock = new FakeClock();
    const closes: number[] = [];
    scheduleDeadline({ close: (code) => { closes.push(code ?? 0); } }, 120, clock);
    clock.advance(50);
    expect(closes).toEqual([]);
    clock.advance(50);
    expect(closes).toEqual([]);
    clock.advance(20);
    expect(closes).toEqual([4003]);
  });

  test("ordinary close cleanup cancels the active timer handle", () => {
    const clock = new FakeClock();
    const closes: number[] = [];
    const handle = scheduleDeadline({ close: (code) => { closes.push(code ?? 0); } }, 100, clock);
    expect(clock.timers.size).toBe(1);
    if (handle.current) clock.clearTimeout(handle.current);
    handle.current = null;
    clock.advance(200);
    expect(clock.timers.size).toBe(0);
    expect(closes).toEqual([]);
  });
});
