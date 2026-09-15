// CellGridRenderer native-scroll settle regression coverage.
// Layout can clamp a reader to literal bottom without a second scroll event.
// Real off-bottom reader intent must remain immutable across the same settle turn.
import { expect, test } from "bun:test";
import { CellGridRenderer } from "../src/lib/cellRenderer.ts";
import type { CellGridFrame, CellRow } from "@roost/shared/cell";
import {
  deltaFrame,
  makeContainer,
  row,
  seedHeldHistory,
} from "./helpers/cellRendererFakeDom.ts";

const scrollbackRows = (count: number): CellRow[] =>
  Array.from({ length: count }, (_, index) => row(index, `s${index}`));

const appendedFrame = (seq: number): CellGridFrame => ({
  ...deltaFrame(80, 1, [row(0, "v")], [row(397 + seq, "after-settle")], seq),
  scrollbackTotal: 398 + seq,
});

async function withAnimationFrames(
  verify: (drainAnimationFrames: () => void) => Promise<void>,
): Promise<void> {
  const host = globalThis as typeof globalThis & {
    requestAnimationFrame?: (callback: FrameRequestCallback) => number;
  };
  const prior = host.requestAnimationFrame;
  const callbacks: FrameRequestCallback[] = [];
  host.requestAnimationFrame = (callback) => {
    callbacks.push(callback);
    return callbacks.length;
  };
  try {
    await verify(() => {
      while (callbacks.length > 0) callbacks.shift()!(0);
    });
  } finally {
    if (prior) host.requestAnimationFrame = prior;
    else Reflect.deleteProperty(host, "requestAnimationFrame");
  }
}

test("native scroll settle resumes a frame after silent bottom clamping", async () => {
  await withAnimationFrames(async (drainAnimationFrames) => {
    const container = makeContainer();
    const renderer = new CellGridRenderer(container as unknown as HTMLElement);
    seedHeldHistory(renderer, 80, [row(0, "v")], scrollbackRows(400));
    container.scrollTop = container.scrollHeight - container.clientHeight - 1;
    renderer.handleScroll();
    renderer.apply(appendedFrame(3));
    drainAnimationFrames();

    container.scrollTop = container.scrollHeight - container.clientHeight;
    renderer.apply(appendedFrame(4));
    drainAnimationFrames();

    expect(renderer.readerIntent).toBe("live");
    expect(renderer.canonicalEpochSeq()).toEqual(renderer.reconciledEpochSeq());
    expect(renderer.currentFrame!.seq).toBe(4);
  });
});
// A wheel gesture at the bottom parks WITHOUT moving the scroll position, so no
// scroll event will ever follow: the settle is the pane's only resume.
test("a wheel park clamped to the bottom settles without a second scroll event", async () => {
  await withAnimationFrames(async (drainAnimationFrames) => {
    const container = makeContainer();
    const renderer = new CellGridRenderer(container as unknown as HTMLElement);
    seedHeldHistory(renderer, 80, [row(0, "v")], scrollbackRows(400));
    container.scrollTop = container.scrollHeight - container.clientHeight;
    renderer.enterReading("wheel");
    renderer.apply(appendedFrame(3));
    expect(renderer.currentFrame!.seq).toBe(2);
    drainAnimationFrames();

    expect(renderer.readerIntent).toBe("live");
    expect(renderer.currentFrame!.seq).toBe(3);
    expect(renderer.canonicalEpochSeq()).toEqual(renderer.reconciledEpochSeq());
  });
});
test("native scroll settle keeps a true off-bottom reader held", async () => {
  await withAnimationFrames(async (drainAnimationFrames) => {
    const container = makeContainer();
    const renderer = new CellGridRenderer(container as unknown as HTMLElement);
    seedHeldHistory(renderer, 80, [row(0, "v")], scrollbackRows(400));
    container.scrollTop = container.scrollHeight - container.clientHeight - 1;
    renderer.handleScroll();
    renderer.apply(appendedFrame(3));
    drainAnimationFrames();

    expect(renderer.readerIntent).toBe("reading");
    expect(renderer.readerReason).toBe("native_scroll");
    expect(renderer.reconcileBlockReason()).toBe("reader_pending_frame");
  });
});
