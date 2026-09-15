// Dormant terminal link attachments must not retain hidden-pane listeners.
// Activation runs one post-paint current-tail scan so a plain terminal URL is
// linkified without revisiting materialized retained history, and the armed
// hold it hands the renderer tracks the live modifier level, never a stale edge.

import { afterEach, describe, expect, test } from "bun:test";
import { ROW_COLUMNS_ATTR } from "../src/lib/cellRow.ts";
import { attachTerminalLinks } from "../src/components/terminal-links.ts";
import { CellGridRenderer } from "../src/lib/cellRenderer.ts";
import {
  deltaFrame,
  makeContainer,
  row,
  seedHeldHistory,
  vpEl,
} from "./helpers/cellRendererFakeDom.ts";

class FakeEvents {
  private readonly listeners = new Map<string, Set<(event: unknown) => void>>();

  addEventListener(type: string, listener: (event: unknown) => void): void {
    let listeners = this.listeners.get(type);
    if (!listeners) {
      listeners = new Set();
      this.listeners.set(type, listeners);
    }
    listeners.add(listener);
  }

  removeEventListener(type: string, listener: (event: unknown) => void): void {
    this.listeners.get(type)?.delete(listener);
  }

  dispatch(type: string, event: unknown): void {
    for (const listener of [...(this.listeners.get(type) ?? [])]) listener(event);
  }

  listenerCount(type: string): number {
    return this.listeners.get(type)?.size ?? 0;
  }
}

class FakeText {
  parentElement: FakeElement | null = null;

  constructor(readonly data: string) {}

  get length(): number {
    return this.data.length;
  }

  get textContent(): string {
    return this.data;
  }
}

class FakeElement extends FakeEvents {
  readonly childNodes: Array<FakeElement | FakeText> = [];
  readonly style: { getPropertyValue: (name: string) => string };
  className = "";
  parentElement: FakeElement | null = null;
  querySelector: (selector: string) => FakeElement | null = () => null;
  querySelectorAll: (selector: string) => FakeElement[] = () => [];
  private readonly attributes = new Map<string, string>();

  constructor(readonly tagName: string, cols = "") {
    super();
    this.style = { getPropertyValue: (name) => name === "--cell-cols" ? cols : "" };
  }

  get textContent(): string {
    return this.childNodes.map((child) => child.textContent).join("");
  }

  set textContent(value: string) {
    this.childNodes.splice(0);
    if (value) this.appendChild(new FakeText(value));
  }

  appendChild(child: FakeElement | FakeText): FakeElement | FakeText {
    child.parentElement = this;
    this.childNodes.push(child);
    return child;
  }

  replaceText(text: FakeText, replacement: Array<FakeElement | FakeText>): void {
    const index = this.childNodes.indexOf(text);
    if (index < 0) return;
    this.childNodes.splice(index, 1, ...replacement);
    for (const child of replacement) child.parentElement = this;
  }

  setAttribute(name: string, value: string): void {
    this.attributes.set(name, value);
  }

  getAttribute(name: string): string | null {
    return this.attributes.get(name) ?? null;
  }

  hasAttribute(name: string): boolean {
    return this.attributes.has(name);
  }

  removeAttribute(name: string): void {
    this.attributes.delete(name);
  }

  remove(): void {}
}

class FakeRange {
  private start: FakeText | null = null;
  private startOffset = 0;
  private endOffset = 0;

  setStart(node: FakeText, offset: number): void {
    this.start = node;
    this.startOffset = offset;
  }

  setEnd(_node: FakeText, offset: number): void {
    this.endOffset = offset;
  }

  surroundContents(anchor: FakeElement): void {
    const text = this.start;
    const parent = text?.parentElement;
    if (!text || !parent) return;
    const before = text.data.slice(0, this.startOffset);
    const selected = text.data.slice(this.startOffset, this.endOffset);
    const after = text.data.slice(this.endOffset);
    anchor.appendChild(new FakeText(selected));
    parent.replaceText(text, [
      ...(before ? [new FakeText(before)] : []),
      anchor,
      ...(after ? [new FakeText(after)] : []),
    ]);
  }
}

class FakeDocument extends FakeEvents {
  readonly head = new FakeElement("head");
  readonly body = new FakeElement("body");
  visibilityState = "visible";

  createElement(tagName: string): FakeElement {
    return new FakeElement(tagName);
  }

  createTreeWalker(root: FakeElement): { nextNode: () => FakeText | null } {
    const nodes = root.childNodes.filter((child): child is FakeText => child instanceof FakeText);
    let index = 0;
    return { nextNode: () => nodes[index++] ?? null };
  }

  createRange(): FakeRange {
    return new FakeRange();
  }
}

class FakeMutationObserver {
  static instances: FakeMutationObserver[] = [];
  observeCalls = 0;

  constructor(_callback: MutationCallback) {
    FakeMutationObserver.instances.push(this);
  }

  observe(): void {
    this.observeCalls += 1;
  }

  disconnect(): void {}
}

interface AnimationFrameEntry {
  handle: number;
  callback: () => void;
}

interface Harness {
  readonly container: FakeElement;
  readonly document: FakeDocument;
  readonly frames: AnimationFrameEntry[];
  readonly window: FakeEvents;
  fireFrame(): void;
  restore(): void;
}

function createHarness(): Harness {
  FakeMutationObserver.instances = [];
  const document = new FakeDocument();
  const window = new FakeEvents();
  const container = new FakeElement("div", "40");
  const frames: AnimationFrameEntry[] = [];
  let nextHandle = 1;
  const globals = globalThis as unknown as Record<string, unknown>;
  const symbolGlobals = globalThis as unknown as Record<PropertyKey, unknown>;
  const saved = Object.fromEntries([
    "document",
    "window",
    "navigator",
    "HTMLElement",
    "MutationObserver",
    "NodeFilter",
    "requestAnimationFrame",
    "cancelAnimationFrame",
  ].map((name) => [name, globals[name]]));
  const cssKey = Symbol.for("roost.wterm-link.css");
  const savedCss = symbolGlobals[cssKey];
  delete symbolGlobals[cssKey];
  globals.document = document;
  globals.window = window;
  globals.navigator = { userAgent: "Macintosh", platform: "MacIntel" };
  globals.HTMLElement = FakeElement;
  globals.MutationObserver = FakeMutationObserver;
  globals.NodeFilter = { SHOW_TEXT: 4 };
  globals.requestAnimationFrame = (callback: () => void) => {
    const handle = nextHandle++;
    frames.push({ handle, callback });
    return handle;
  };
  globals.cancelAnimationFrame = (handle: number) => {
    const index = frames.findIndex((frame) => frame.handle === handle);
    if (index >= 0) frames.splice(index, 1);
  };
  return {
    container,
    document,
    frames,
    window,
    fireFrame: () => frames.shift()?.callback(),
    restore: () => {
      for (const [name, value] of Object.entries(saved)) globals[name] = value;
      if (savedCss === undefined) delete symbolGlobals[cssKey];
      else symbolGlobals[cssKey] = savedCss;
    },
  };
}

describe("attachTerminalLinks initial activity", () => {
  let harness: Harness | undefined;

  afterEach(() => {
    harness?.restore();
    harness = undefined;
  });

  test("constructs dormant and linkifies a current plain URL only after activation", () => {
    harness = createHarness();
    const row = new FakeElement("div");
    row.className = "cell-row";
    row.setAttribute(ROW_COLUMNS_ATTR, "40");
    row.textContent = "https://example.test/terminal";
    const viewport = new FakeElement("div");
    viewport.querySelectorAll = () => [row];
    harness.container.querySelector = (selector) =>
      selector === ".cell-viewport" ? viewport : null;

    const attachment = attachTerminalLinks(harness.container as unknown as HTMLElement, {
      initialActive: false,
    });

    expect(FakeMutationObserver.instances).toHaveLength(0);
    expect(harness.document.listenerCount("visibilitychange")).toBe(0);
    expect(harness.window.listenerCount("keydown")).toBe(0);
    expect(harness.window.listenerCount("keyup")).toBe(0);
    expect(harness.window.listenerCount("blur")).toBe(0);
    expect(harness.container.listenerCount("click")).toBe(0);
    expect(harness.frames).toHaveLength(0);
    expect(row.childNodes[0]).toBeInstanceOf(FakeText);

    attachment.setActive(true);
    expect(FakeMutationObserver.instances).toHaveLength(1);
    expect(FakeMutationObserver.instances[0]?.observeCalls).toBe(1);
    expect(harness.document.listenerCount("visibilitychange")).toBe(1);
    expect(harness.window.listenerCount("keydown")).toBe(1);
    expect(harness.window.listenerCount("keyup")).toBe(1);
    expect(harness.window.listenerCount("blur")).toBe(1);
    expect(harness.container.listenerCount("click")).toBe(1);
    expect(harness.frames).toHaveLength(1);

    harness.fireFrame();
    expect(row.childNodes[0]).toBeInstanceOf(FakeText);
    harness.fireFrame();
    const anchor = row.childNodes[0] as FakeElement;
    expect(anchor.tagName).toBe("a");
    expect(anchor.className).toBe("wterm-link");
    expect(anchor.getAttribute("href")).toBe("https://example.test/terminal");
    attachment.dispose();
  });
});

// The harness platform reports macOS, so the link modifier is Meta.
const pointerEvent = (metaKey: boolean): unknown => ({
  metaKey,
  ctrlKey: false,
  target: null,
});

describe("attachTerminalLinks armed hold level", () => {
  let harness: Harness | undefined;

  afterEach(() => {
    harness?.restore();
    harness = undefined;
  });

  // The pane's own wiring: onArmedHoverChange drives RENDERER_HOLD_LINK
  // (cell-terminal-interactions.ts), so the hold is observed as paint.
  function armedPane() {
    const created = createHarness();
    harness = created;
    const paint = makeContainer();
    const renderer = new CellGridRenderer(paint as unknown as HTMLElement);
    seedHeldHistory(renderer, 80, [row(0, "v0")], []);
    const attachment = attachTerminalLinks(created.container as unknown as HTMLElement, {
      initialActive: true,
      onArmedHoverChange: (active) => { renderer.setArmedHold(active); },
    });
    created.window.dispatch("keydown", { key: "Meta" });
    created.container.dispatch("mouseenter", pointerEvent(true));
    expect(renderer.apply(deltaFrame(80, 1, [row(0, "v1")], [], 2))).toBe(true);
    return {
      created,
      attachment,
      renderer,
      paintedTail: (): string => String(vpEl(paint).children[0].textContent),
    };
  }

  test("a lost modifier keyup is healed by the next pointer event, which repaints", () => {
    const pane = armedPane();
    expect(pane.paintedTail()).toBe("v0");

    // No keyup ever arrives; the pointer simply moves over the pane again.
    pane.created.container.dispatch("mouseover", pointerEvent(false));

    expect(pane.renderer.holdMask).toBe(0);
    expect(pane.paintedTail()).toBe("v1");
    pane.attachment.dispose();
  });

  test("a pointer event with the modifier still held keeps the pane held", () => {
    const pane = armedPane();

    pane.created.container.dispatch("mouseover", pointerEvent(true));

    expect(pane.renderer.holdMask).not.toBe(0);
    expect(pane.paintedTail()).toBe("v0");
    pane.attachment.dispose();
  });

  test("re-entering the pane without the modifier cannot revive the hold", () => {
    const pane = armedPane();
    pane.created.container.dispatch("mouseleave", pointerEvent(true));
    expect(pane.renderer.holdMask).toBe(0);

    pane.created.container.dispatch("mouseenter", pointerEvent(false));

    expect(pane.renderer.holdMask).toBe(0);
    expect(pane.paintedTail()).toBe("v1");
    pane.attachment.dispose();
  });
});
