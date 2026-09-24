// Mounts one real cell-terminal interaction pane over the renderer fake DOM:
// real mountCellTerminalInteractions, a real CellGridRenderer and a real
// selection guard, with the pane's display doubling as the renderer's scroll
// container so selection ownership and mouse hit-testing are both genuine.
// Consumed by cellTerminalVisibility.test.ts, which owns the fake document and
// the module mocks; the pane's modules load on the first mount, after those.

import type * as SolidApi from "solid-js";
import type { MouseTracking } from "@roost/protocol/cell";
import type { CellGridRenderer } from "../../src/renderer/cellRenderer.ts";
import type { CellTerminalInteractions } from "../../src/components/terminal/cell-terminal-interactions.ts";
import { FakeEl, row, seedHeldHistory, vpEl } from "./cellRendererFakeDom.ts";

type Listener = (event: unknown) => void;

export interface CellTerminalPaneOptions {
  /** The document installed as globalThis.document; also the display's owner. */
  ownerDocument: unknown;
  /** DECSET tracking mode carried by the newest accepted frame. */
  mouseTracking?: MouseTracking;
}

export interface CellTerminalPane {
  renderer: CellGridRenderer;
  /** The painted live row — the node a native selection has to own. */
  paintedTailNode(): unknown;
  paintedTail(): string;
  setViewActive(active: boolean): void;
  /** Fire a pane-local display event, as the browser would. */
  dispatchDisplay(type: string, event: unknown): void;
  dispose(): void;
}

export async function mountCellTerminalPane(
  options: CellTerminalPaneOptions,
): Promise<CellTerminalPane> {
  // Dynamic by necessity: these modules bind the fake document/window and the
  // caller's module mocks when they evaluate, so they must not load with this
  // helper's own static imports.
  const Solid = await import("solid-js") as unknown as typeof SolidApi;
  const { CellGridRenderer } = await import("../../src/renderer/cellRenderer.ts");
  const { createTerminalSelectionGuard } = await import(
    "../../src/renderer/terminalSelectionGuard.ts"
  );
  const { mountCellTerminalInteractions } = await import(
    "../../src/components/terminal/cell-terminal-interactions.ts"
  );

  const listeners = new Map<string, Set<Listener>>();
  const container = new FakeEl("div", options.ownerDocument);
  Object.assign(container, {
    addEventListener(type: string, listener: Listener): void {
      let bucket = listeners.get(type);
      if (!bucket) {
        bucket = new Set();
        listeners.set(type, bucket);
      }
      bucket.add(listener);
    },
    removeEventListener(type: string, listener: Listener): void {
      listeners.get(type)?.delete(listener);
    },
    // Real ancestry: the selection guard decides pane ownership through this.
    contains(node: unknown): boolean {
      let cursor = node as { parentElement?: unknown } | null;
      while (cursor) {
        if (cursor === container) return true;
        cursor = (cursor.parentElement ?? null) as { parentElement?: unknown } | null;
      }
      return false;
    },
  });

  const renderer = new CellGridRenderer(container as unknown as HTMLElement);
  seedHeldHistory(renderer, 80, [row(0, "v0")], []);
  const guard = createTerminalSelectionGuard({
    getDisplay: () => container as unknown as HTMLDivElement,
    getRenderer: () => renderer,
    getBackfill: () => null,
    getLinkAttachment: () => null,
  });

  const textarea = { blur: () => undefined };
  const runtime = {
    sessionId: "session-pane",
    display: () => container,
    inputController: {
      textarea,
      forceFocus: () => undefined,
      ownsTarget: (target: unknown) => target === textarea,
      setAccessibleLabel: () => undefined,
    },
    renderer,
    linkAttachment: null,
    frameMouseSgr: true,
  };
  const input = {
    setCtrlArmed: () => undefined,
    setLinkActivationArmed: () => undefined,
    resolveFile: async () => null,
    enqueueFileItems: () => undefined,
    copySelectionToClipboard: async () => undefined,
  };

  const [viewActive, setViewActive] = Solid.createSignal(true);
  let disposeRoot: () => void = () => undefined;
  let interactions: CellTerminalInteractions = { dispose: () => undefined };
  Solid.createRoot((dispose) => {
    disposeRoot = dispose;
    interactions = mountCellTerminalInteractions(
      { focused: true, session: { id: "session-pane", git_remote: null } } as never,
      runtime as never,
      input as never,
      {
        notifyBackfill: guard.notifyBackfill,
        syncNativeSelectionHold: guard.syncNativeSelectionHold,
      } as never,
      { viewActive, measureCell: () => true } as never,
      () => false,
      () => undefined,
      {
        mouseTracking: () => options.mouseTracking ?? 0,
        linkActivationArmed: () => false,
      } as never,
    );
  });

  return {
    renderer,
    paintedTailNode: (): unknown => vpEl(container).children[0],
    paintedTail: (): string => String(vpEl(container).children[0].textContent),
    setViewActive(active: boolean): void {
      setViewActive(active);
    },
    dispatchDisplay(type: string, event: unknown): void {
      for (const listener of listeners.get(type) ?? []) listener(event);
    },
    dispose(): void {
      interactions.dispose();
      disposeRoot();
    },
  };
}
