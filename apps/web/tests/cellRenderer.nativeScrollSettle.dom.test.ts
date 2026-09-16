// CellGridRenderer native-scroll settle regression coverage.
// Layout can clamp a reader to literal bottom without a second scroll event.
// Real off-bottom reader intent must remain immutable across the same settle turn.
// A rest INSIDE the band gets no scroll event of its own, so frame arrival has
// to recruit the pane's settle window — once, and only for the band.
import { expect, test } from "bun:test";
import { CellGridRenderer } from "../src/lib/cellRenderer.ts";
import type { CellGridFrame, CellRow } from "@roost/shared/cell";
import {
  ROW_PX,
  deltaFrame,
  historyNode,
  makeContainer,
  row,
  sbEl,
  sbRows,
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

// The pane's quiet window, modelled exactly as cell-terminal-renderer.ts opens
// one: a frame may OPEN a window, and further frames must not re-arm it.
function createFollowSettleWindow(): { request: () => void; opened: () => number } {
  let pending = false;
  let opened = 0;
  return {
    request: () => {
      if (pending) return;
      pending = true;
      opened += 1;
    },
    opened: () => opened,
  };
}

test("native scroll settle resumes a frame after silent bottom clamping", async () => {
  await withAnimationFrames(async (drainAnimationFrames) => {
    const container = makeContainer();
    const renderer = new CellGridRenderer(container as unknown as HTMLElement);
    seedHeldHistory(renderer, 80, [row(0, "v")], scrollbackRows(400));
    container.scrollTop = container.scrollHeight - container.clientHeight - 3 * ROW_PX;
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
// A shrunken maximum withholds only an ANCHOR park's release: a position-only
// park has nothing to protect at the bottom, however it got there.
test("a wheel park clamped onto the bottom by a box grow resumes", async () => {
  await withAnimationFrames(async () => {
    const container = makeContainer();
    const renderer = new CellGridRenderer(container as unknown as HTMLElement);
    seedHeldHistory(renderer, 80, [row(0, "v")], scrollbackRows(400));
    container.scrollTop = container.scrollHeight - container.clientHeight - 4 * ROW_PX;
    renderer.handleScroll(); // observes the pre-grow maximum
    renderer.enterReading("wheel");
    renderer.apply(appendedFrame(3));
    expect(renderer.currentFrame!.seq).toBe(2);

    container.clientHeight = 700; // the maximum drops below the parked position
    container.scrollTop = container.scrollHeight - container.clientHeight;

    expect(renderer.handleScroll().reconciled).toBe(true);
    expect(renderer.readerIntent).toBe("live");
    expect(renderer.currentFrame!.seq).toBe(3);
  });
});
test("native scroll settle keeps a true off-bottom reader held", async () => {
  await withAnimationFrames(async (drainAnimationFrames) => {
    const container = makeContainer();
    const renderer = new CellGridRenderer(container as unknown as HTMLElement);
    seedHeldHistory(renderer, 80, [row(0, "v")], scrollbackRows(400));
    container.scrollTop = container.scrollHeight - container.clientHeight - 3 * ROW_PX;
    renderer.handleScroll();
    renderer.apply(appendedFrame(3));
    drainAnimationFrames();

    expect(renderer.readerIntent).toBe("reading");
    expect(renderer.readerReason).toBe("native_scroll");
    expect(renderer.reconcileBlockReason()).toBe("reader_pending_frame");
  });
});

// The pane's scroll listener arms these once the scroll stream goes quiet; a
// resume mid-gesture would write scrollTop and cancel the reader's own scroll.
test("a wheel park resting inside the follow band resumes on the settle", () => {
  const container = makeContainer();
  const renderer = new CellGridRenderer(container as unknown as HTMLElement);
  seedHeldHistory(renderer, 80, [row(0, "v")], scrollbackRows(400));
  renderer.enterReading("wheel"); // a real gesture parks before the box moves
  container.scrollTop = container.scrollHeight - container.clientHeight - ROW_PX;
  renderer.handleScroll();
  renderer.apply(appendedFrame(3));
  expect(renderer.reconcileBlockReason()).toBe("reader_pending_frame");

  expect(renderer.settleFollowBand().reconciled).toBe(true);

  expect(renderer.readerIntent).toBe("live");
  expect(container.scrollTop).toBe(container.scrollHeight - container.clientHeight);
  expect(renderer.reconcileBlockReason()).toBeNull();
});
test("a park beyond the follow band survives the settle", () => {
  const container = makeContainer();
  const renderer = new CellGridRenderer(container as unknown as HTMLElement);
  seedHeldHistory(renderer, 80, [row(0, "v")], scrollbackRows(400));
  container.scrollTop = container.scrollHeight - container.clientHeight - 3 * ROW_PX;
  renderer.handleScroll();
  renderer.apply(appendedFrame(3));

  expect(renderer.settleFollowBand()).toEqual({ reconciled: false, anchorChanged: false });

  expect(renderer.readerIntent).toBe("reading");
  expect(renderer.readerReason).toBe("native_scroll");
});
test("a find park inside the follow band keeps its anchor through the settle", () => {
  const container = makeContainer();
  const renderer = new CellGridRenderer(container as unknown as HTMLElement);
  seedHeldHistory(renderer, 80, [row(0, "v")], scrollbackRows(400));
  container.scrollTop = container.scrollHeight - container.clientHeight - ROW_PX;
  renderer.enterReading("find");

  expect(renderer.settleFollowBand()).toEqual({ reconciled: false, anchorChanged: false });

  expect(renderer.readerIntent).toBe("reading");
  expect(renderer.readerReason).toBe("find");
});

// The wheel/touch classifier parks from a capture-phase listener, so it can
// park AFTER the gesture's last scroll event: no scroll listener will ever arm
// the band settle for that park, and frame arrival is the pane's only
// guaranteed signal.
test("a band rest parked with no scroll event recruits the settle on a frame", async () => {
  await withAnimationFrames(async (drainAnimationFrames) => {
    const container = makeContainer();
    const settle = createFollowSettleWindow();
    const renderer = new CellGridRenderer(
      container as unknown as HTMLElement, undefined, undefined, settle.request,
    );
    seedHeldHistory(renderer, 80, [row(0, "v")], scrollbackRows(400));
    container.scrollTop = container.scrollHeight - container.clientHeight - ROW_PX;
    renderer.enterReading("wheel"); // the gesture's last scroll already fired
    container.resetScrollTopWrites();

    renderer.apply(appendedFrame(3));
    drainAnimationFrames();

    expect(settle.opened()).toBe(1);
    // The frame may only ASK: writing scrollTop here cancels the gesture.
    expect(container.scrollTopWrites).toBe(0);
    expect(renderer.readerIntent).toBe("reading");
    expect(renderer.reconcileBlockReason()).toBe("reader_pending_frame");

    expect(renderer.settleFollowBand().reconciled).toBe(true);

    expect(renderer.readerIntent).toBe("live");
    expect(renderer.canonicalEpochSeq()).toEqual(renderer.reconciledEpochSeq());
    expect(renderer.currentFrame!.seq).toBe(3);
    expect(historyNode(container, 400).textContent).toBe("after-settle");
  });
});
// The deliberate-read control: a reader who left the band owns the pane, so a
// frame must not recruit a window that would re-pin them.
test("a park beyond the follow band recruits no settle window", async () => {
  await withAnimationFrames(async (drainAnimationFrames) => {
    const container = makeContainer();
    const settle = createFollowSettleWindow();
    const renderer = new CellGridRenderer(
      container as unknown as HTMLElement, undefined, undefined, settle.request,
    );
    seedHeldHistory(renderer, 80, [row(0, "v")], scrollbackRows(400));
    container.scrollTop = container.scrollHeight - container.clientHeight - 3 * ROW_PX;
    renderer.enterReading("wheel");

    renderer.apply(appendedFrame(3));
    drainAnimationFrames();

    expect(settle.opened()).toBe(0);
    expect(renderer.readerIntent).toBe("reading");
    expect(renderer.readerReason).toBe("wheel");
    expect(renderer.currentFrame!.seq).toBe(2);
    // The DOM stays frozen on the row the reader was reading.
    expect(sbRows(sbEl(container)).at(-1)!.textContent).toBe("s399");
  });
});
// The exact clamp is the frame's own settle turn; routing it through the pane's
// quiet window would delay every clamped follower by BOTTOM_FOLLOW_SETTLE_MS.
test("a park on the exact clamp resumes on the frame with no settle window", async () => {
  await withAnimationFrames(async (drainAnimationFrames) => {
    const container = makeContainer();
    const settle = createFollowSettleWindow();
    const renderer = new CellGridRenderer(
      container as unknown as HTMLElement, undefined, undefined, settle.request,
    );
    seedHeldHistory(renderer, 80, [row(0, "v")], scrollbackRows(400));
    container.scrollTop = container.scrollHeight - container.clientHeight;
    renderer.enterReading("wheel");

    renderer.apply(appendedFrame(3));
    drainAnimationFrames();

    expect(settle.opened()).toBe(0);
    expect(renderer.readerIntent).toBe("live");
    expect(renderer.currentFrame!.seq).toBe(3);
  });
});
// A busy PTY delivers a frame every few milliseconds: one window per park is
// what keeps the resume reachable, since re-arming per frame defers it for as
// long as output continues.
test("a stream of frames over a band rest keeps exactly one settle window", async () => {
  await withAnimationFrames(async (drainAnimationFrames) => {
    const container = makeContainer();
    const settle = createFollowSettleWindow();
    const renderer = new CellGridRenderer(
      container as unknown as HTMLElement, undefined, undefined, settle.request,
    );
    seedHeldHistory(renderer, 80, [row(0, "v")], scrollbackRows(400));
    container.scrollTop = container.scrollHeight - container.clientHeight - ROW_PX;
    renderer.enterReading("wheel");

    for (let seq = 3; seq <= 8; seq++) {
      expect(renderer.apply(appendedFrame(seq))).toBe(true);
      drainAnimationFrames();
    }

    expect(settle.opened()).toBe(1);

    expect(renderer.settleFollowBand().reconciled).toBe(true);

    expect(renderer.readerIntent).toBe("live");
    expect(renderer.canonicalEpochSeq()).toEqual(renderer.reconciledEpochSeq());
    expect(renderer.currentFrame!.seq).toBe(8);
    expect(historyNode(container, 405).textContent).toBe("after-settle");
  });
});
