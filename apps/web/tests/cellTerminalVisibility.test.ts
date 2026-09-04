// Terminal foreground-work gates share the canonical pane accessor and page visibility.
// The interaction mount test exercises focus acquisition and release through real Solid effects;
// the renderer gate test covers initial focus and cursor-poll admission without mounting a grid.

import { afterEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import type { CellTerminalInteractions } from "../src/components/cell-terminal-interactions.ts";

const fakeDocument = Object.assign(new EventTarget(), {
  visibilityState: "visible",
  activeElement: null as EventTarget | null,
  getSelection: () => null,
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
});

// Intentional module-loading boundary: browser Solid and DOM-owner mocks must
// exist before the component controller modules evaluate. Resolving the public
// entry first keeps the client-runtime import typed without naming an undeclared
// package subpath in source.
const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
mock.module("solid-js", () => ({ ...Solid }));
mock.module("../src/components/TerminalComposeButton.tsx", () => ({
  activeComposeSessionId: () => null,
}));
mock.module("../src/components/terminal-links.ts", () => ({
  attachTerminalLinks: () => ({ dispose: () => undefined }),
}));
mock.module("../src/lib/terminalMouseForwarding.ts", () => ({
  attachTerminalMouseForwarding: () => ({
    onWindowMouseMove: () => undefined,
    onWindowMouseUp: () => undefined,
    bindWheelAndTouchMove: () => undefined,
    dispose: () => undefined,
  }),
}));
mock.module("../src/lib/windowSizeClass.ts", () => ({
  isCompact: () => false,
  isTouchDevice: () => false,
}));
mock.module("../src/lib/copyOnSelectPref.ts", () => ({
  copyOnSelect: () => false,
}));
mock.module("../src/lib/userTerminalInput.ts", () => ({
  sendUserTerminalInput: () => undefined,
  registerUserTerminalInput: () => () => undefined,
}));
mock.module("../src/lib/sessionTitle.ts", () => ({
  sessionTitle: () => "test",
}));

const {
  _terminalFocusAllowed,
  mountCellTerminalInteractions,
} = await import("../src/components/cell-terminal-interactions.ts");
const { _terminalForegroundWorkAllowed } = await import(
  "../src/components/cell-terminal-renderer.ts"
);
const {
  setForceHidden,
  setForceVisible,
} = await import("../src/lib/pageVisible.ts");

afterEach(() => {
  setForceHidden(false);
  setForceVisible(false);
  fakeDocument.activeElement = null;
});

describe("terminal foreground visibility", () => {
  test("covered and hidden panes release focus while an active visible pane owns it", () => {
    const [viewActive, setViewActive] = Solid.createSignal(true);
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
        () => undefined,
        { mouseTracking: () => "none" } as never,
      );
    });

    expect(controller.forceFocusCalls).toBe(1);
    expect(fakeDocument.activeElement).toBe(textarea);
    expect(_terminalFocusAllowed(viewport as never, true)).toBe(true);

    setViewActive(false);
    expect(controller.forceFocusCalls).toBe(1);
    expect(textarea.blurCalls).toBe(1);
    expect(fakeDocument.activeElement).toBeNull();
    expect(_terminalFocusAllowed(viewport as never, true)).toBe(false);

    setViewActive(true);
    expect(controller.forceFocusCalls).toBe(2);
    setForceHidden(true);
    expect(controller.forceFocusCalls).toBe(2);
    expect(textarea.blurCalls).toBe(2);
    expect(fakeDocument.activeElement).toBeNull();
    expect(_terminalFocusAllowed(viewport as never, true)).toBe(false);

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
