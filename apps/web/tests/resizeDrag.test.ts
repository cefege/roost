import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  beginPointerResizeDrag,
  beginResizeDrag,
  isResizeDragging,
  resetResizeDrags,
} from "../src/lib/resizeDrag.ts";

class TrackingEventTarget implements EventTarget {
  readonly added = new Map<string, number>();
  readonly removed = new Map<string, number>();
  private readonly listeners = new Map<string, Set<EventListenerOrEventListenerObject>>();

  addEventListener(
    type: string,
    callback: EventListenerOrEventListenerObject | null,
    _options?: AddEventListenerOptions | boolean,
  ): void {
    this.added.set(type, (this.added.get(type) ?? 0) + 1);
    if (!callback) return;
    let listeners = this.listeners.get(type);
    if (!listeners) {
      listeners = new Set();
      this.listeners.set(type, listeners);
    }
    listeners.add(callback);
  }

  removeEventListener(
    type: string,
    callback: EventListenerOrEventListenerObject | null,
    _options?: EventListenerOptions | boolean,
  ): void {
    this.removed.set(type, (this.removed.get(type) ?? 0) + 1);
    if (callback) this.listeners.get(type)?.delete(callback);
  }

  dispatchEvent(event: Event): boolean {
    const listeners = [...(this.listeners.get(event.type) ?? [])];
    for (const listener of listeners) {
      if (typeof listener === "function") listener.call(this, event);
      else listener.handleEvent(event);
    }
    return !event.defaultPrevented;
  }
}

class FakeHost extends TrackingEventTarget {
  readonly cancelledFrames: number[] = [];
  private readonly frames = new Map<number, FrameRequestCallback>();
  private nextFrameId = 1;

  requestAnimationFrame(callback: FrameRequestCallback): number {
    const id = this.nextFrameId++;
    this.frames.set(id, callback);
    return id;
  }

  cancelAnimationFrame(id: number): void {
    this.cancelledFrames.push(id);
    this.frames.delete(id);
  }

  get queuedFrameCount(): number {
    return this.frames.size;
  }

  flushFrames(): void {
    const queued = [...this.frames.entries()];
    this.frames.clear();
    for (const [id, callback] of queued) callback(id);
  }
}

class FakePointerTarget extends TrackingEventTarget {
  readonly capturedPointers: number[] = [];
  readonly releasedPointers: number[] = [];
  private readonly captures = new Set<number>();

  setPointerCapture(pointerId: number): void {
    this.capturedPointers.push(pointerId);
    this.captures.add(pointerId);
  }

  hasPointerCapture(pointerId: number): boolean {
    return this.captures.has(pointerId);
  }

  releasePointerCapture(pointerId: number): void {
    this.releasedPointers.push(pointerId);
    this.captures.delete(pointerId);
    // Browsers emit this after explicit/implicit release. The helper must have
    // removed its listener before release so it cannot settle twice.
    this.dispatchEvent(pointerEvent("lostpointercapture", pointerId));
  }
}

function pointerEvent(type: string, pointerId: number, clientX = 0): PointerEvent {
  const event = new Event(type) as PointerEvent;
  Object.defineProperties(event, {
    pointerId: { value: pointerId },
    clientX: { value: clientX },
    clientY: { value: 0 },
  });
  return event;
}

function startGesture(pointerId = 7) {
  const host = new FakeHost();
  const target = new FakePointerTarget();
  const moves: number[] = [];
  const commits: number[] = [];
  let releases = 0;
  const dispose = beginPointerResizeDrag({
    host,
    target,
    pointerId,
    initialGeometry: 5,
    geometryFor: (event) => event.clientX,
    onMove: (geometry) => moves.push(geometry),
    onCommit: (geometry) => commits.push(geometry),
    onRelease: () => { releases++; },
  });
  return { host, target, moves, commits, dispose, releases: () => releases };
}

beforeEach(resetResizeDrags);
afterEach(resetResizeDrags);

describe("resize drag ownership", () => {
  test("overlapping owners suppress until the final release and double release is inert", () => {
    const releaseFirst = beginResizeDrag();
    const releaseSecond = beginResizeDrag();

    expect(isResizeDragging()).toBe(true);
    releaseFirst();
    expect(isResizeDragging()).toBe(true);
    releaseFirst();
    expect(isResizeDragging()).toBe(true);
    releaseSecond();
    expect(isResizeDragging()).toBe(false);

    const releaseNext = beginResizeDrag();
    releaseFirst();
    expect(isResizeDragging()).toBe(true);
    releaseNext();
    expect(isResizeDragging()).toBe(false);
  });

  test("reset invalidates stale owners without letting them release a new owner", () => {
    const staleFirst = beginResizeDrag();
    const staleSecond = beginResizeDrag();
    resetResizeDrags();
    expect(isResizeDragging()).toBe(false);

    const current = beginResizeDrag();
    staleFirst();
    staleSecond();
    expect(isResizeDragging()).toBe(true);
    current();
    expect(isResizeDragging()).toBe(false);
  });

  test("the 33rd owner resets the bounded generation and prior releases are stale", () => {
    const staleOwners = Array.from({ length: 32 }, () => beginResizeDrag());
    const current = beginResizeDrag();

    for (const release of staleOwners) release();
    expect(isResizeDragging()).toBe(true);
    current();
    expect(isResizeDragging()).toBe(false);
  });
});

describe("pointer resize drag lifecycle", () => {
  test("pointerup cancels the queued frame and commits its latest geometry once", () => {
    const gesture = startGesture();
    expect(gesture.target.capturedPointers).toEqual([7]);
    expect(isResizeDragging()).toBe(true);

    gesture.host.dispatchEvent(pointerEvent("pointermove", 99, 100));
    gesture.host.dispatchEvent(pointerEvent("pointermove", 7, 20));
    gesture.host.dispatchEvent(pointerEvent("pointermove", 7, 30));
    expect(gesture.host.queuedFrameCount).toBe(1);

    gesture.host.dispatchEvent(pointerEvent("pointerup", 99, 200));
    expect(gesture.commits).toEqual([]);
    gesture.host.dispatchEvent(pointerEvent("pointerup", 7, 200));

    expect(gesture.moves).toEqual([]);
    expect(gesture.commits).toEqual([30]);
    expect(gesture.releases()).toBe(1);
    expect(gesture.host.cancelledFrames).toEqual([1]);
    expect(gesture.target.releasedPointers).toEqual([7]);
    expect(isResizeDragging()).toBe(false);

    gesture.host.dispatchEvent(pointerEvent("pointerup", 7, 300));
    gesture.dispose();
    expect(gesture.commits).toEqual([30]);
    expect(gesture.releases()).toBe(1);
    expect(gesture.host.cancelledFrames).toEqual([1]);
  });

  test("commits before release callbacks and keeps suppression through committed layout", () => {
    const host = new FakeHost();
    const target = new FakePointerTarget();
    const order: string[] = [];
    beginPointerResizeDrag({
      host,
      target,
      pointerId: 7,
      initialGeometry: 5,
      geometryFor: (event) => event.clientX,
      onMove: () => {},
      onCommit: (geometry) => {
        order.push(`commit:${geometry}:${isResizeDragging()}`);
      },
      onRelease: () => {
        order.push(`release:${isResizeDragging()}`);
      },
    });

    host.dispatchEvent(pointerEvent("pointermove", 7, 37));
    host.dispatchEvent(pointerEvent("pointerup", 7));

    expect(order).toEqual(["commit:37:true", "release:true"]);
    expect(isResizeDragging()).toBe(false);
  });

  test("a throwing commit still releases its owner and every listener", () => {
    const host = new FakeHost();
    const target = new FakePointerTarget();
    const commitError = new Error("commit failed");
    let releases = 0;
    const dispose = beginPointerResizeDrag({
      host,
      target,
      pointerId: 7,
      initialGeometry: 5,
      geometryFor: (event) => event.clientX,
      onMove: () => {},
      onCommit: () => { throw commitError; },
      onRelease: () => { releases++; },
    });
    host.dispatchEvent(pointerEvent("pointermove", 7, 48));

    expect(() => host.dispatchEvent(pointerEvent("pointerup", 7))).toThrow(commitError);
    expect(releases).toBe(1);
    expect(isResizeDragging()).toBe(false);
    expect(host.cancelledFrames).toEqual([1]);
    expect(host.removed).toEqual(new Map([
      ["pointermove", 1],
      ["pointerup", 1],
      ["pointercancel", 1],
      ["blur", 1],
    ]));
    expect(target.removed.get("lostpointercapture")).toBe(1);

    dispose();
    expect(releases).toBe(1);
  });

  test("pointercancel and lost capture each settle the latest sample", () => {
    for (const endType of ["pointercancel", "lostpointercapture"] as const) {
      const gesture = startGesture();
      gesture.host.dispatchEvent(pointerEvent("pointermove", 7, 41));
      if (endType === "lostpointercapture") {
        gesture.target.dispatchEvent(pointerEvent(endType, 7));
      } else {
        gesture.host.dispatchEvent(pointerEvent(endType, 7));
      }

      expect(gesture.commits).toEqual([41]);
      expect(gesture.releases()).toBe(1);
      expect(gesture.host.cancelledFrames).toEqual([1]);
      expect(isResizeDragging()).toBe(false);
    }
  });

  test("window blur settles the latest sample and removes every listener", () => {
    const gesture = startGesture();
    gesture.host.dispatchEvent(pointerEvent("pointermove", 7, 52));
    gesture.host.dispatchEvent(new Event("blur"));

    expect(gesture.commits).toEqual([52]);
    expect(gesture.releases()).toBe(1);
    expect(gesture.host.removed).toEqual(new Map([
      ["pointermove", 1],
      ["pointerup", 1],
      ["pointercancel", 1],
      ["blur", 1],
    ]));
    expect(gesture.target.removed.get("lostpointercapture")).toBe(1);
    expect(isResizeDragging()).toBe(false);
  });

  test("disposal mid-drag aborts queued geometry and teardown stays idempotent", () => {
    const gesture = startGesture();
    gesture.host.dispatchEvent(pointerEvent("pointermove", 7, 64));
    gesture.dispose();

    expect(gesture.moves).toEqual([]);
    expect(gesture.commits).toEqual([]);
    expect(gesture.releases()).toBe(1);
    expect(gesture.host.cancelledFrames).toEqual([1]);
    expect(gesture.host.queuedFrameCount).toBe(0);
    expect(isResizeDragging()).toBe(false);

    gesture.dispose();
    gesture.host.dispatchEvent(pointerEvent("pointerup", 7, 64));
    gesture.host.flushFrames();
    expect(gesture.commits).toEqual([]);
    expect(gesture.releases()).toBe(1);
    expect(gesture.host.cancelledFrames).toEqual([1]);
    expect(gesture.host.removed).toEqual(new Map([
      ["pointermove", 1],
      ["pointerup", 1],
      ["pointercancel", 1],
      ["blur", 1],
    ]));
    expect(gesture.target.removed.get("lostpointercapture")).toBe(1);
  });

  test("two pointer gestures retain independent owners", () => {
    const host = new FakeHost();
    const firstTarget = new FakePointerTarget();
    const secondTarget = new FakePointerTarget();
    const firstDispose = beginPointerResizeDrag({
      host,
      target: firstTarget,
      pointerId: 1,
      initialGeometry: 0,
      geometryFor: (event) => event.clientX,
      onMove: () => {},
      onCommit: () => {},
    });
    beginPointerResizeDrag({
      host,
      target: secondTarget,
      pointerId: 2,
      initialGeometry: 0,
      geometryFor: (event) => event.clientX,
      onMove: () => {},
      onCommit: () => {},
    });

    firstDispose();
    expect(isResizeDragging()).toBe(true);
    host.dispatchEvent(pointerEvent("pointerup", 2));
    expect(isResizeDragging()).toBe(false);
  });
});
