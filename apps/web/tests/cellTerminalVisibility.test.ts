// Terminal foreground-work gates share the canonical pane accessor and page visibility.
// The interaction mount test exercises focus acquisition and release through real Solid effects;
// the renderer gate test covers initial focus and cursor-poll admission without mounting a grid;
// the withdraw and suspension tests drive a real renderer and selection guard across a park and a composer's suspend handoff.

import { afterEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import type { CellTerminalInteractions } from "../src/components/terminal/cell-terminal-interactions.ts";
import { FakeEl, deltaFrame, row, seedHeldHistory, vpEl } from "./helpers/cellRendererFakeDom.ts";
import { mountCellTerminalPane } from "./helpers/cellTerminalPaneHarness.ts";

const linkActivity: boolean[] = [];
const initialLinkActivity: boolean[] = [];
const sentBytes: Uint8Array[] = [];
let liveSelection: Selection | null = null;

const fakeDocument = Object.assign(new EventTarget(), {
  visibilityState: "visible",
  activeElement: null as EventTarget | null,
  getSelection: () => liveSelection,
  // The selection guard resolves pane ownership through the DISPLAY's
  // ownerDocument, so the document that mints the renderer's grid and the one
  // that answers getSelection() have to be the same object.
  createElement: (tag: string) => new FakeEl(tag, fakeDocument),
  createTextNode: (text: string) => ({ textContent: text, parentElement: null }),
  createDocumentFragment: () => new FakeEl("#fragment", fakeDocument),
  fonts: { ready: Promise.resolve() },
});
const fakeWindow = new EventTarget();

class FakeTextarea extends EventTarget {
  blurCalls = 0;

  blur(): void {
    this.blurCalls += 1;
    if (fakeDocument.activeElement === this) fakeDocument.activeElement = null;
  }
}
Object.assign(globalThis, {
  document: fakeDocument,
  window: fakeWindow,
  // The guard separates element from text endpoints, and admits owned rows, through the DOM classes: here FakeEl IS both.
  Element: FakeEl,
  HTMLElement: FakeEl,
});

// Intentional module-loading boundary: browser Solid and DOM-owner mocks must
// exist before the component controller modules evaluate. Resolving the public
// entry first keeps the client-runtime import typed without naming an undeclared
// package subpath in source.
const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
mock.module("solid-js", () => ({ ...Solid }));
mock.module("../src/components/terminal/TerminalComposeButton.tsx", () => ({
  activeComposeSessionId: () => null,
}));
mock.module("../src/renderer/terminal-links.ts", () => ({
  attachTerminalLinks: (
    _display: unknown,
    options: { initialActive?: boolean } = {},
  ) => {
    initialLinkActivity.push(options.initialActive ?? true);
    return {
      setActive: (active: boolean) => linkActivity.push(active),
      releaseInteraction: () => undefined,
      dispose: () => undefined,
    };
  },
  isTerminalLinkActivationGesture: () => false,
}));
mock.module("../src/browser/windowSizeClass.ts", () => ({
  isCompact: () => false,
  isTouchDevice: () => false,
}));
mock.module("../src/store/prefs/copyOnSelectPref.ts", () => ({
  copyOnSelect: () => false,
}));
mock.module("../src/lib/userTerminalInput.ts", () => ({
  sendUserTerminalInput: (_sessionId: string, bytes: Uint8Array) => {
    sentBytes.push(bytes);
  },
  registerUserTerminalInput: () => () => undefined,
}));
mock.module("../src/lib/sessionTitle.ts", () => ({
  sessionTitle: () => "test",
}));

const {
  _terminalFocusAllowed,
  mountCellTerminalInteractions,
} = await import("../src/components/terminal/cell-terminal-interactions.ts");
const { _terminalForegroundWorkAllowed } = await import(
  "../src/components/terminal/cell-terminal-renderer.ts"
);
const {
  setForceHidden,
  setForceVisible,
} = await import("../src/browser/pageVisible.ts");
const { CellGridRenderer } = await import("../src/renderer/cellRenderer.ts");
const { createTerminalSelectionGuard } = await import("../src/renderer/terminalSelectionGuard.ts");

afterEach(() => {
  setForceHidden(false);
  setForceVisible(false);
  fakeDocument.activeElement = null;
  liveSelection = null;
  linkActivity.length = 0;
  sentBytes.length = 0;
  initialLinkActivity.length = 0;
});

describe("terminal foreground visibility", () => {
  test("pending, covered, and hidden panes wait until an active visible pane can own focus", () => {
    const [viewActive, setViewActive] = Solid.createSignal(false);
    const [pending, setPending] = Solid.createSignal(true);
    const textarea = new FakeTextarea();
    const controller = {
      textarea,
      forceFocusCalls: 0,
      forceFocus() {
        this.forceFocusCalls += 1;
        fakeDocument.activeElement = textarea;
      },
      ownsTarget: (target: EventTarget | null) => target === textarea,
      setAccessibleLabel: () => undefined,
    };
    const display = Object.assign(new EventTarget(), {
      ownerDocument: fakeDocument,
      contains: () => false,
    });
    const runtime = {
      sessionId: "session-focus",
      display: () => display,
      inputController: controller,
      renderer: null,
      linkAttachment: null,
      frameMouseSgr: false,
    };
    const input = {
      setCtrlArmed: () => undefined,
      setLinkActivationArmed: () => undefined,
      resolveFile: async () => null,
      enqueueFileItems: () => undefined,
      copySelectionToClipboard: async () => undefined,
    };
    const presentation = {
      notifyBackfill: () => undefined,
      syncNativeSelectionHold: () => undefined,
    };
    const viewport = {
      viewActive,
      measureCell: () => true,
    };
    const props = {
      focused: true,
      session: { id: "session-focus", git_remote: null },
    };

    let disposeRoot: () => void = () => undefined;
    let interactions: CellTerminalInteractions = {
      dispose: () => undefined,
    };
    Solid.createRoot((dispose) => {
      disposeRoot = dispose;
      interactions = mountCellTerminalInteractions(
        props as never,
        runtime as never,
        input as never,
        presentation as never,
        viewport as never,
        pending,
        () => undefined,
        { mouseTracking: () => "none" } as never,
      );
    });

    expect(controller.forceFocusCalls).toBe(0);
    expect(fakeDocument.activeElement).toBeNull();
    expect(initialLinkActivity).toEqual([false]);
    expect(_terminalFocusAllowed(viewport as never, true, !pending())).toBe(false);

    setPending(false);
    expect(controller.forceFocusCalls).toBe(0);
    expect(fakeDocument.activeElement).toBeNull();
    expect(_terminalFocusAllowed(viewport as never, true, !pending())).toBe(false);

    setViewActive(true);
    expect(controller.forceFocusCalls).toBe(1);
    expect(fakeDocument.activeElement).toBe(textarea);
    expect(_terminalFocusAllowed(viewport as never, true, !pending())).toBe(true);

    setViewActive(false);
    expect(controller.forceFocusCalls).toBe(1);
    expect(textarea.blurCalls).toBe(1);
    expect(fakeDocument.activeElement).toBeNull();
    expect(_terminalFocusAllowed(viewport as never, true, !pending())).toBe(false);

    setViewActive(true);
    expect(controller.forceFocusCalls).toBe(2);
    setForceHidden(true);
    expect(controller.forceFocusCalls).toBe(2);
    expect(textarea.blurCalls).toBe(2);
    expect(fakeDocument.activeElement).toBeNull();
    expect(_terminalFocusAllowed(viewport as never, true, !pending())).toBe(false);
    expect(linkActivity.slice(-3)).toEqual([false, true, false]);

    interactions.dispose();
    disposeRoot();
  });

  test("cursor polling and initial focus share the canonical foreground gate", () => {
    let active = true;
    const viewport = { viewActive: () => active };

    expect(_terminalForegroundWorkAllowed(viewport as never)).toBe(true);
    active = false;
    expect(_terminalForegroundWorkAllowed(viewport as never)).toBe(false);

    active = true;
    setForceHidden(true);
    expect(_terminalForegroundWorkAllowed(viewport as never)).toBe(false);

    setForceHidden(false);
    expect(_terminalForegroundWorkAllowed(viewport as never)).toBe(true);
  });
});

const armSelectionOn = (node: unknown): void => {
  liveSelection = {
    isCollapsed: false,
    rangeCount: 1,
    anchorNode: node,
    focusNode: node,
  } as unknown as Selection;
  fakeDocument.dispatchEvent(new Event("selectionchange"));
};

describe("terminal selection hold across a foreground withdraw", () => {
  test("a selection dropped while the pane's listeners are detached stops holding paint", async () => {
    const pane = await mountCellTerminalPane({ ownerDocument: fakeDocument });

    armSelectionOn(pane.paintedTailNode());
    expect(pane.renderer.apply(deltaFrame(80, 1, [row(0, "v1")], [], 2))).toBe(true);
    expect(pane.paintedTail()).toBe("v0");

    pane.setViewActive(false);
    liveSelection = null;
    fakeDocument.dispatchEvent(new Event("selectionchange"));
    expect(pane.renderer.holdMask).not.toBe(0);

    pane.setViewActive(true);
    expect(pane.renderer.holdMask).toBe(0);
    expect(pane.paintedTail()).toBe("v1");
    expect(pane.renderer.apply(deltaFrame(80, 1, [row(0, "v2")], [], 3))).toBe(true);
    expect(pane.paintedTail()).toBe("v2");

    pane.dispose();
  });

  test("a selection still live when the listeners re-attach keeps paint held", async () => {
    const pane = await mountCellTerminalPane({ ownerDocument: fakeDocument });

    armSelectionOn(pane.paintedTailNode());
    expect(pane.renderer.apply(deltaFrame(80, 1, [row(0, "v1")], [], 2))).toBe(true);
    expect(pane.paintedTail()).toBe("v0");

    pane.setViewActive(false);
    pane.setViewActive(true);

    expect(pane.renderer.holdMask).not.toBe(0);
    expect(pane.paintedTail()).toBe("v0");
    expect(pane.renderer.apply(deltaFrame(80, 1, [row(0, "v2")], [], 3))).toBe(true);
    expect(pane.paintedTail()).toBe("v0");

    pane.dispose();
  });
});

// One pane-owned range, yielded by a composer suspend() whose restore never runs.
const suspendedComposerPane = () => {
  const container = new FakeEl("div", fakeDocument);
  const renderer = new CellGridRenderer(container as unknown as HTMLElement);
  seedHeldHistory(renderer, 80, [row(0, "v0")], []);
  const selectedRow = Object.assign(new FakeEl("div", fakeDocument), {
    className: "cell-row", textContent: "v0", nodeValue: null, childNodes: [{}],
    isConnected: true, getRootNode: () => fakeDocument,
    closest(selector: string) { return selector === ".cell-row" ? this : null; },
  });
  const display = { ownerDocument: fakeDocument, isConnected: true,
    contains: (node: unknown) => node === selectedRow && selectedRow.isConnected };
  const range = { startContainer: selectedRow, endContainer: selectedRow, toString: () => "v0" };
  liveSelection = {
    isCollapsed: false, rangeCount: 1, toString: () => "v0",
    anchorNode: selectedRow, anchorOffset: 0, focusNode: selectedRow, focusOffset: 1,
    getRangeAt: () => ({ cloneRange: () => range }),
    removeAllRanges(): void {
      Object.assign(this, { anchorNode: null, focusNode: null, isCollapsed: true, rangeCount: 0 });
    },
  } as unknown as Selection;
  const guard = createTerminalSelectionGuard({
    getDisplay: () => display as unknown as HTMLDivElement,
    getRenderer: () => renderer, getBackfill: () => null, getLinkAttachment: () => null,
  });
  // A composer suspends the pane's range only while its own field owns focus.
  fakeDocument.activeElement = Object.assign(new FakeTextarea(), { isConnected: true });
  expect(guard.captureTerminalSelection()?.suspend()).toBe(true);
  guard.syncNativeSelectionHold();
  return { guard, renderer, selectedRow,
    paintedTail: (): string => String(vpEl(container).children[0].textContent) };
};

describe("terminal selection hold across a composer suspension", () => {
  test("a suspension whose restore never runs stops holding paint once its range is gone", () => {
    const pane = suspendedComposerPane();
    expect(pane.renderer.holdMask).not.toBe(0);
    // What a canonical repair does to the captured row; nothing restores or releases it after.
    pane.selectedRow.isConnected = false;
    pane.guard.syncNativeSelectionHold();
    expect(pane.renderer.holdMask).toBe(0);
    expect(pane.renderer.apply(deltaFrame(80, 1, [row(0, "v2")], [], 2))).toBe(true);
    expect(pane.paintedTail()).toBe("v2");
  });

  test("a suspension whose range is still live and restorable keeps paint held", () => {
    const pane = suspendedComposerPane();
    pane.guard.syncNativeSelectionHold();
    expect(pane.renderer.holdMask).not.toBe(0);
    expect(pane.renderer.apply(deltaFrame(80, 1, [row(0, "v2")], [], 2))).toBe(true);
    expect(pane.paintedTail()).toBe("v0");
  });
});

interface MouseReport {
  cb: number;
  col: number;
  row: number;
  release: boolean;
}
const decoder = new TextDecoder();
// What the PTY actually receives: SGR-1006 reports, ESC [ < cb ; col ; row M|m.
const mouseReports = (): MouseReport[] =>
  sentBytes.map((bytes) => {
    const text = decoder.decode(bytes);
    const parsed = /^\x1b\[<(\d+);(\d+);(\d+)([Mm])$/.exec(text);
    if (!parsed) throw new Error(`not an SGR mouse report: ${JSON.stringify(text)}`);
    return {
      cb: Number(parsed[1]),
      col: Number(parsed[2]),
      row: Number(parsed[3]),
      release: parsed[4] === "m",
    };
  });
const mouseEvent = (type: string, clientX: number, clientY: number): Event =>
  Object.assign(new Event(type), {
    button: 0,
    clientX,
    clientY,
    shiftKey: false,
    altKey: false,
    ctrlKey: false,
    metaKey: false,
  });

describe("terminal mouse drag across a foreground withdraw", () => {
  test("an interrupted drag is released exactly once and never resumes as a phantom", async () => {
    const pane = await mountCellTerminalPane({
      ownerDocument: fakeDocument,
      mouseTracking: 1002,
    });

    pane.dispatchDisplay("mousedown", mouseEvent("mousedown", 20, 20));
    fakeWindow.dispatchEvent(mouseEvent("mousemove", 44, 20));
    const dragged = mouseReports();
    expect(dragged.length).toBe(2);
    expect(dragged[0]).toMatchObject({ cb: 0, release: false });
    expect(dragged[1]).toMatchObject({ cb: 32, release: false });
    expect(dragged[1]!.col).not.toBe(dragged[0]!.col);

    pane.setViewActive(false);
    const settled = mouseReports();
    expect(settled.length).toBe(3);
    expect(settled[2]).toEqual({
      cb: 0,
      col: dragged[1]!.col,
      row: dragged[1]!.row,
      release: true,
    });

    fakeWindow.dispatchEvent(mouseEvent("mousemove", 76, 20));
    expect(mouseReports().length).toBe(3);

    pane.setViewActive(true);
    fakeWindow.dispatchEvent(mouseEvent("mousemove", 100, 20));
    fakeWindow.dispatchEvent(mouseEvent("mouseup", 100, 20));
    expect(mouseReports()).toEqual(settled);

    pane.dispose();
  });
});
