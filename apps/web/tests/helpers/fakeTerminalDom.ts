// Minimal DOM stand-in for the terminal measurement path (no jsdom, repo
// convention). It models the three layout facts measurement depends on: a
// row's height comes from `.cell-grid .cell-row`, the display box carries
// `.wterm` padding, and a parked pane stays laid out but hidden. Shared by
// wtermSizeEstimate.dom.test.ts and spawnSession.test.ts so both exercise the
// real estimate against one scene builder.

/** One monospace cell advance in this fake's px. */
export const CELL_ADVANCE_PX = 10;
/** `.cell-grid .cell-row { height: 1.2em }` (styles/sidebar.css). */
export const GRID_ROW_PX = 20;
/** UA `line-height: normal` — what a probe gets outside a `.cell-grid` box. */
export const UA_ROW_PX = 16;

export interface FakeRect {
  width: number;
  height: number;
}

export interface FakePadding {
  left: number;
  right: number;
  top: number;
  bottom: number;
}

export class FakeElement {
  className = "";
  textContent = "";
  clientWidth = 0;
  clientHeight = 0;
  visibility = "visible";
  padding: FakePadding = { left: 0, right: 0, top: 0, bottom: 0 };
  rect: FakeRect | null = null;
  parentElement: FakeElement | null = null;
  readonly attributes: Record<string, string> = {};
  readonly children: FakeElement[] = [];
  readonly style: Record<string, string> = {};

  constructor(readonly tagName: string) {}

  appendChild(child: FakeElement): FakeElement {
    child.parentElement = this;
    this.children.push(child);
    return child;
  }

  removeChild(child: FakeElement): void {
    const idx = this.children.indexOf(child);
    if (idx >= 0) this.children.splice(idx, 1);
    child.parentElement = null;
  }

  remove(): void {
    this.parentElement?.removeChild(this);
  }

  matches(selector: string): boolean {
    for (const [, key, value] of selector.matchAll(/\[([\w-]+)(?:="([^"]*)")?\]/g)) {
      const actual = this.attributes[key];
      if (actual === undefined) return false;
      if (value !== undefined && actual !== value) return false;
    }
    return true;
  }

  closest(selector: string): FakeElement | null {
    let node: FakeElement | null = this;
    while (node) {
      if (node.matches(selector)) return node;
      node = node.parentElement;
    }
    return null;
  }

  getBoundingClientRect(): FakeRect {
    if (this.rect) return this.rect;
    let node: FakeElement | null = this;
    let styledRow = false;
    while (node && !styledRow) {
      styledRow = node.className.split(" ").includes("cell-grid");
      node = node.parentElement;
    }
    return {
      width: this.textContent.length * CELL_ADVANCE_PX,
      height: styledRow ? GRID_ROW_PX : UA_ROW_PX,
    };
  }
}

export class FakeDocument {
  readonly body = new FakeElement("body");
  visibilityState = "visible";

  createElement(tagName: string): FakeElement {
    return new FakeElement(tagName);
  }

  querySelector(selector: string): FakeElement | null {
    return this.querySelectorAll(selector)[0] ?? null;
  }

  querySelectorAll(selector: string): FakeElement[] {
    const found: FakeElement[] = [];
    const walk = (node: FakeElement): void => {
      if (node !== this.body && node.matches(selector)) found.push(node);
      for (const child of node.children) walk(child);
    };
    walk(this.body);
    return found;
  }
}

export interface FakeSlotOptions {
  focused: boolean;
  visible: boolean;
  slotRect: FakeRect;
  displayWidth: number;
  displayHeight: number;
}

/** A deck box, laid out the way TerminalDeck lays it out. */
export function mountFakeDeck(fakeDocument: FakeDocument): FakeElement {
  const deck = new FakeElement("div");
  deck.attributes["data-testid"] = "terminal-deck";
  deck.clientWidth = 1_200;
  deck.clientHeight = 800;
  deck.rect = { width: 1_200, height: 800 };
  fakeDocument.body.appendChild(deck);
  return deck;
}

/** One deck slot holding one mounted display box. Returns the display. */
export function mountFakeSlot(
  deck: FakeElement,
  options: FakeSlotOptions,
): FakeElement {
  const slot = new FakeElement("div");
  slot.attributes["data-pane-slot"] = "";
  slot.attributes["data-pane"] = "";
  slot.attributes["data-focused"] = options.focused ? "true" : "false";
  slot.visibility = options.visible ? "visible" : "hidden";
  slot.rect = options.slotRect;
  const display = new FakeElement("div");
  display.attributes["data-testid"] = "terminal-display";
  display.className = "wterm cell-grid";
  display.visibility = options.visible ? "visible" : "hidden";
  display.clientWidth = options.displayWidth;
  display.clientHeight = options.displayHeight;
  display.padding = { left: 16, right: 16, top: 12, bottom: 12 };
  slot.appendChild(display);
  deck.appendChild(slot);
  return display;
}

export interface InstalledFakeDom {
  document: FakeDocument;
  restore(): void;
}

/** Install `document` + `getComputedStyle`. Sibling suites install their own
 *  fakes in the same process, so callers install per test and restore after. */
export function installFakeTerminalDom(): InstalledFakeDom {
  const globals = globalThis as unknown as Record<string, unknown>;
  const saved = {
    document: globals.document,
    getComputedStyle: globals.getComputedStyle,
  };
  const fakeDocument = new FakeDocument();
  globals.document = fakeDocument;
  globals.getComputedStyle = (element: FakeElement) => ({
    visibility: element.visibility,
    paddingLeft: `${element.padding.left}px`,
    paddingRight: `${element.padding.right}px`,
    paddingTop: `${element.padding.top}px`,
    paddingBottom: `${element.padding.bottom}px`,
  });
  return {
    document: fakeDocument,
    restore: () => {
      for (const [key, value] of Object.entries(saved)) {
        if (value === undefined) delete globals[key];
        else globals[key] = value;
      }
    },
  };
}
