// Owner-lifecycle tripwire for CellTerminal.tsx's drag-drop listener leak.
//
// CellTerminal registers document drop/paste/mousedown/keydown listeners AND
// their teardown onCleanup AFTER `await term.init()` inside an async onMount.
// Solid binds onCleanup to the SYNCHRONOUS owner; after an await that owner is
// null, so the teardown never registers → listeners leak per unmount → a single
// OS drop fires N stale onDrop closures → N× enqueueAttachment (duplicate chips
// + abs_path typed N times). Fix: capture getOwner() BEFORE the await and wrap
// the post-await onCleanup in runWithOwner(owner, …). This test proves both the
// leak and the fix at the reactivity level (no DOM needed).

import { describe, test, expect } from "bun:test";
import { createRoot, onCleanup, getOwner, runWithOwner } from "solid-js";

// Reproduce the async-onMount shape: sync setup, then an await, then late work.
async function mountLike(
  body: (ctx: { ownerAtMount: ReturnType<typeof getOwner> }) => Promise<void>,
): Promise<() => void> {
  let dispose = () => {};
  await new Promise<void>((resolve) => {
    createRoot((d) => {
      dispose = d;
      const ownerAtMount = getOwner(); // captured synchronously — valid
      void body({ ownerAtMount }).then(resolve);
    });
  });
  return dispose;
}

describe("async onMount + onCleanup owner", () => {
  test("BUG: onCleanup registered after await never runs on dispose", async () => {
    let cleaned = false;
    const dispose = await mountLike(async () => {
      await Promise.resolve(); // the `await term.init()`
      onCleanup(() => { cleaned = true; }); // owner is null here → dropped
    });
    dispose();
    expect(cleaned).toBe(false); // characterizes the leak
  });

  test("FIX: runWithOwner(ownerAtMount, …) revives the post-await onCleanup", async () => {
    let cleaned = false;
    const dispose = await mountLike(async ({ ownerAtMount }) => {
      await Promise.resolve();
      runWithOwner(ownerAtMount, () => onCleanup(() => { cleaned = true; }));
    });
    dispose();
    expect(cleaned).toBe(true); // teardown fires → no leaked drop listener
  });
});
