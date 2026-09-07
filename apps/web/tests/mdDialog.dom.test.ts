// Invariant: a dialog the app has closed must stop holding the document inert.
// A showModal() <dialog> makes every other element inert, so while md-dialog
// runs its exit the app can render a new surface that silently refuses focus()
// — the ⌘K palette accepted no keystrokes for that window. Both close paths
// count: ESC and scrim clicks are started by md-dialog itself.
// Bun has no DOM, so inertness is observed through a double that reproduces
// md-dialog's verified close sequence (see FakeMdDialog); the browser-level
// proof is smoke/terminal/command-palette.spec.ts.

import { describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";

type VNode = {
  tag: unknown;
  props: Record<string, unknown>;
};

// Bun resolves "solid-js" to the server build, so the client runtime has to be
// loaded by its resolved URL rather than a static specifier.
const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;

mock.module("solid-js", () => Solid);
// Both register browser custom elements at module scope, which has no
// HTMLElement to extend under Bun.
mock.module("@material/web/dialog/dialog.js", () => ({}));
mock.module("solid-js/web", () => ({ Dynamic: () => null }));

const renderedNodes: VNode[] = [];
mock.module("react/jsx-dev-runtime", () => ({
  Fragment: Symbol("Fragment"),
  jsxDEV(tag: unknown, props: Record<string, unknown> | null): VNode {
    const vnode: VNode = { tag, props: { ...(props ?? {}) } };
    renderedNodes.push(vnode);
    return vnode;
  },
}));

// A static import would bind the real md-dialog module before the stubs above.
const { Dialog } = await import("../src/components/Settings/md/Dialog.tsx");

/** Reproduces @material/web/dialog/internal/dialog.js: the `open` setter calls
 *  show()/close() synchronously (:36-49); close() yields, dispatches a
 *  cancelable `close` event (:190), runs animateDialog(), which returns without
 *  animating when `quick` is set (:342-351), and only afterwards does the
 *  native dialog leave the top layer (:195-197). Escape and scrim clicks reach
 *  close() without the host's `open` property changing (:292, :320).
 *  Animations finish when the test says so, never on a wall clock. */
class FakeMdDialog {
  quick = false;
  /** True while the native <dialog> is showModal()-open — i.e. while every
   *  element outside it is inert and cannot take focus. */
  holdsDocumentInert = false;
  animatedEnters = 0;
  private readonly runningAnimations: Array<() => void> = [];
  private closeListener: ((event: { currentTarget: FakeMdDialog }) => void) | null = null;
  private opened = false;

  applyProperty(name: string, value: unknown): void {
    if (name === "prop:quick") this.quick = value === true;
    else if (name === "prop:open") this.setOpen(value === true);
    else if (name === "on:close" && typeof value === "function") {
      this.closeListener = value as (event: { currentTarget: FakeMdDialog }) => void;
    }
  }

  /** Escape or a scrim click: md-dialog runs its own close, and the app only
   *  learns about it from the `closed` event afterwards. */
  dismissWithoutAppRequest(): void {
    if (this.opened) void this.runClose();
  }

  finishAnimations(): void {
    while (this.runningAnimations.length > 0) this.runningAnimations.pop()?.();
  }

  private setOpen(next: boolean): void {
    if (next === this.opened) return;
    if (next) {
      this.opened = true;
      void this.show();
      return;
    }
    void this.runClose();
  }

  private async show(): Promise<void> {
    await Promise.resolve();
    this.holdsDocumentInert = true;
    await this.animate(true);
  }

  private async runClose(): Promise<void> {
    await Promise.resolve();
    this.closeListener?.({ currentTarget: this });
    await this.animate(false);
    this.holdsDocumentInert = false;
    this.opened = false;
  }

  private async animate(entering: boolean): Promise<void> {
    if (this.quick) return;
    if (entering) this.animatedEnters++;
    const { promise, resolve } = Promise.withResolvers<void>();
    this.runningAnimations.push(resolve);
    await promise;
  }
}

function renderMdDialog(element: FakeMdDialog, open: boolean): void {
  renderedNodes.length = 0;
  Dialog({ open, onClose: () => undefined, children: null });
  const host = renderedNodes.find((node) => node.props.component === "md-dialog");
  if (!host) throw new Error("Dialog did not render an md-dialog host");
  for (const [name, value] of Object.entries(host.props)) element.applyProperty(name, value);
}

async function settle(): Promise<void> {
  for (let turn = 0; turn < 4; turn++) await Promise.resolve();
}

async function openedDialog(): Promise<FakeMdDialog> {
  const element = new FakeMdDialog();
  renderMdDialog(element, true);
  await settle();
  expect(element.holdsDocumentInert).toBe(true);
  return element;
}

describe("md Dialog", () => {
  test("closing from the app releases the document before the exit finishes", async () => {
    const element = await openedDialog();

    renderMdDialog(element, false);
    await settle();
    // No finishAnimations() call: the exit is still outstanding, and the rest of
    // the document must already be focusable.
    expect(element.holdsDocumentInert).toBe(false);
    element.finishAnimations();
  });

  test("an Escape or scrim dismissal releases the document just as early", async () => {
    const element = await openedDialog();

    element.dismissWithoutAppRequest();
    await settle();
    expect(element.holdsDocumentInert).toBe(false);
    element.finishAnimations();
  });

  test("reopening after a close plays the enter motion again", async () => {
    const element = await openedDialog();
    expect(element.animatedEnters).toBe(1);

    renderMdDialog(element, false);
    await settle();
    element.finishAnimations();
    await settle();

    renderMdDialog(element, true);
    await settle();
    // A dialog that skipped its exit must not stay in skip-animation mode, or
    // every open after the first would snap in with no motion at all.
    expect(element.animatedEnters).toBe(2);
    element.finishAnimations();
  });
});
