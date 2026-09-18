// The controller map is the only surface that tells a pad user what their
// buttons do, and the only one that must stay absent for a keyboard. This suite
// pins both, plus the live highlight and the no-text-field rule that keeps a
// controller from being trapped in it. Bun has no browser DOM, so it drives the
// same client-Solid virtual renderer as the other component DOM suites.

import { afterEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";

type VNode = {
  tag: unknown;
  props: Record<string, unknown>;
  rendered?: unknown;
};

const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
mock.module("solid-js", () => Solid);

function invokeComponent(vnode: VNode): void {
  if (typeof vnode.tag !== "function") return;
  const component = vnode.tag;
  const owner = Solid.getOwner();
  vnode.rendered = Solid.runWithOwner(owner, () => component(vnode.props));
}

function createElement(
  tag: unknown,
  props: Record<string, unknown> | null,
  ...children: unknown[]
): VNode {
  const merged = { ...(props ?? {}) };
  if (children.length > 0) merged.children = children.length === 1 ? children[0] : children;
  const vnode = { tag, props: merged };
  invokeComponent(vnode);
  return vnode;
}

const ReactShim = { Fragment: Symbol("Fragment"), createElement };
const testGlobal = globalThis as typeof globalThis & { React: unknown };
testGlobal.React = ReactShim;
mock.module("react/jsx-dev-runtime", () => ({
  Fragment: ReactShim.Fragment,
  jsxDEV(tag: unknown, props: Record<string, unknown> | null): VNode {
    const children = props?.children === undefined ? [] : [props.children];
    return createElement(tag, props, ...children);
  },
}));

function passthrough(props: Record<string, unknown>): unknown {
  return props.children;
}

// No TextField here on purpose: a filter field added to this surface would fail
// as an undefined component rather than slip through as a pad trap.
mock.module("../src/components/Settings/md/primitives.tsx", () => ({
  Surface: passthrough,
  BindingChip: passthrough,
  Chip: (props: Record<string, unknown>) => props.label,
}));

// Stub the Dialog, not the Sheet: Kobalte's portal is what cannot run under
// Bun, and keeping the real Sheet means the props this surface passes it (side,
// headline, onClose, the sizing class) are exercised instead of mocked away.
// The stub reports itself as an element so the walkers below can read them.
mock.module("../src/components/Settings/md/Dialog.tsx", () => ({
  Dialog: (props: Record<string, unknown>) =>
    (props.open ? { tag: "dialog", props: { ...props } } : null),
}));

const [mapOpen, setMapOpen] = Solid.createSignal(false);
const [inputSeen, setInputSeen] = Solid.createSignal(false);
const [heldButtons, setHeldButtons] = Solid.createSignal<ReadonlySet<number>>(new Set<number>());
const [heldActions, setHeldActions] = Solid.createSignal<ReadonlySet<string>>(new Set<string>());

mock.module("../src/lib/keyboardShortcuts.ts", () => ({
  controllerMapOpen: mapOpen,
  closeControllerMap: () => setMapOpen(false),
}));
mock.module("../src/lib/padMode.ts", () => ({ padInputSeen: inputSeen }));
mock.module("../src/lib/gamepadSource.ts", () => ({
  padHeldButtons: heldButtons,
  padHeldActions: heldActions,
}));

// Awaited imports, not static ones: every mock.module above has to be
// registered before the component's own imports resolve.
const { PAD_CONTROL_GUIDE } = await import("../src/lib/padBindings.ts");
const { ControllerMap } = await import("../src/components/ControllerMap.tsx");

/** Every physical control the v2 button map binds — the map is useless to a
 *  keyboard-free user if any one of them has no labelled entry. */
const BOUND_CAPS = [
  "A", "B", "X", "Y", "LB", "RB", "LT", "RT",
  "Back", "Start", "L3", "R3", "D-pad", "L-stick", "R-stick",
];

function resolvedNode(node: unknown): unknown {
  let resolved = node;
  while (typeof resolved === "function") resolved = resolved();
  return resolved;
}

function collectText(node: unknown, output: string[] = []): string[] {
  const resolved = resolvedNode(node);
  if (typeof resolved === "string" || typeof resolved === "number") {
    output.push(String(resolved));
    return output;
  }
  if (Array.isArray(resolved)) {
    for (const child of resolved) collectText(child, output);
    return output;
  }
  if (!resolved || typeof resolved !== "object") return output;
  const vnode = resolved as VNode;
  if (typeof vnode.tag === "function") collectText(vnode.rendered, output);
  else collectText(vnode.props.children, output);
  return output;
}

/** Real elements only — control-flow components are walked through, so the
 *  result is the tag/attribute tree a browser would actually hold. */
function collectElements(node: unknown, output: VNode[] = []): VNode[] {
  const resolved = resolvedNode(node);
  if (Array.isArray(resolved)) {
    for (const child of resolved) collectElements(child, output);
    return output;
  }
  if (!resolved || typeof resolved !== "object") return output;
  const vnode = resolved as VNode;
  if (typeof vnode.tag === "function") {
    collectElements(vnode.rendered, output);
    return output;
  }
  output.push(vnode);
  collectElements(vnode.props.children, output);
  return output;
}

function renderMap(): { text: string; elements: VNode[] } {
  let rendered: unknown;
  let dispose: (() => void) | undefined;
  Solid.createRoot((rootDispose) => {
    dispose = rootDispose;
    rendered = ControllerMap();
  });
  try {
    return {
      text: collectText(rendered).join(" ").replace(/\s+/g, " ").trim(),
      elements: collectElements(rendered),
    };
  } finally {
    dispose?.();
  }
}

function calloutFor(elements: VNode[], cap: string): VNode | undefined {
  return elements.find((element) => element.props["data-cap"] === cap);
}

afterEach(() => {
  setMapOpen(false);
  setInputSeen(false);
  setHeldButtons(new Set<number>());
  setHeldActions(new Set<string>());
});

describe("ControllerMap", () => {
  test("renders nothing until it is opened", () => {
    setInputSeen(true);

    const { text, elements } = renderMap();

    expect(text).toBe("");
    expect(elements).toHaveLength(0);
  });

  test("renders nothing while no controller input has been seen", () => {
    setMapOpen(true);

    const { text, elements } = renderMap();

    expect(text).toBe("");
    expect(elements).toHaveLength(0);
  });

  test("labels every control the button map binds, in the shared vocabulary", () => {
    setMapOpen(true);
    setInputSeen(true);

    const { elements } = renderMap();

    for (const cap of BOUND_CAPS) {
      const callout = calloutFor(elements, cap);
      const guide = PAD_CONTROL_GUIDE.find((row) => row.cap === cap);
      if (!callout) throw new Error(`no callout on the diagram for ${cap}`);
      if (!guide) throw new Error(`no PAD_CONTROL_GUIDE row for ${cap}`);
      expect(collectText(callout).join(" ")).toBe(`${cap} ${guide.label} ${guide.detail}`);
      expect(callout.props.title).toBe(guide.detail);
    }
  });

  test("gives every control in the shared guide a seat on the diagram", () => {
    setMapOpen(true);
    setInputSeen(true);

    const { elements } = renderMap();

    const seatless = PAD_CONTROL_GUIDE
      .filter((row) => !calloutFor(elements, row.cap))
      .map((row) => row.cap);
    expect(seatless).toEqual([]);
  });

  test("opens one centred sheet whose close path shuts the map", () => {
    setMapOpen(true);
    setInputSeen(true);

    const { elements } = renderMap();

    const dialog = elements.find((element) => element.tag === "dialog");
    if (!dialog) throw new Error("the map must render through the shared sheet");
    expect(dialog.props.class).toBe("roost-sheet--center roost-dialog--controller-map");
    expect(dialog.props.headline).toBe("Controller map");
    // The header close button is the only focusable element in the surface;
    // without it a pad landing here would have nothing to activate.
    expect(dialog.props.showCloseButton).toBe(true);

    (dialog.props.onClose as () => void)();

    expect(renderMap().elements).toHaveLength(0);
  });

  test("renders no text-entry element a controller could be trapped in", () => {
    setMapOpen(true);
    setInputSeen(true);

    const { elements } = renderMap();

    const tags = elements.map((element) => element.tag);
    expect(tags.length).toBeGreaterThan(0);
    expect(tags).not.toContain("input");
    expect(tags).not.toContain("textarea");
  });

  test("lights the held cap and captions what it does", () => {
    setMapOpen(true);
    setInputSeen(true);
    setHeldButtons(new Set([0]));

    const { elements, text } = renderMap();

    const guide = PAD_CONTROL_GUIDE.find((row) => row.cap === "A");
    expect(calloutFor(elements, "A")?.props["data-held"]).toBe("true");
    expect(calloutFor(elements, "B")?.props["data-held"]).toBe("false");
    expect(text).toContain(guide?.detail ?? "");
  });

  test("attributes a direction to the D-pad, not the stick, when an index is held", () => {
    setMapOpen(true);
    setInputSeen(true);
    // What the poller really publishes for a D-pad press: the index AND the
    // intent the stick would have produced too.
    setHeldButtons(new Set([15]));
    setHeldActions(new Set(["move-right"]));

    const { elements } = renderMap();

    expect(calloutFor(elements, "D-pad")?.props["data-held"]).toBe("true");
    expect(calloutFor(elements, "L-stick")?.props["data-held"]).toBe("false");
    expect(calloutFor(elements, "A")?.props["data-held"]).toBe("false");
  });

  test("lights the analogue sticks, which no button index can reach", () => {
    setMapOpen(true);
    setInputSeen(true);
    setHeldActions(new Set(["move-up", "scroll-down"]));

    const { elements } = renderMap();

    expect(calloutFor(elements, "L-stick")?.props["data-held"]).toBe("true");
    expect(calloutFor(elements, "R-stick")?.props["data-held"]).toBe("true");
    expect(calloutFor(elements, "D-pad")?.props["data-held"]).toBe("false");
  });

  test("names a held button the map does not bind", () => {
    setMapOpen(true);
    setInputSeen(true);
    setHeldButtons(new Set([16]));

    const { elements, text } = renderMap();

    const extra = calloutFor(elements, "Button 16");
    if (!extra) throw new Error("a held unbound index must still report itself");
    expect(collectText(extra).join(" "))
      .toBe("Button 16 Unbound This pad reports this button; nothing is bound to it");
    expect(extra.props["data-held"]).toBe("true");
    // An unbound index binds no action, so the caption stays on its prompt.
    expect(text).toContain("Hold a button");
  });
});
