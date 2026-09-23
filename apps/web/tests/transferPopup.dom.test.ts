// TransferStack DOM coverage — one popup must aggregate every live transfer record.
// Bun renders TSX through a small client-Solid virtual renderer, so assertions walk
// the tree the component returns while primitive stubs retain observable semantics.
// TransferStack is a notification-dock child, so nothing here portals to a body.

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";

type VNode = {
  tag: unknown;
  props: Record<string, unknown>;
  rendered?: unknown;
};

class FakeElement {
  constructor(private readonly vnode: VNode) {}

  get textContent(): string {
    return renderedText(this.vnode);
  }

  getAttribute(name: string): string | null {
    const value = this.vnode.props[name];
    return value === undefined || value === null ? null : String(value);
  }
}

function resolveNode(node: unknown): unknown {
  let resolved = node;
  while (typeof resolved === "function") resolved = resolved();
  return resolved;
}

function visitRendered(node: unknown, visit: (vnode: VNode) => void): void {
  const resolved = resolveNode(node);
  if (Array.isArray(resolved)) {
    for (const child of resolved) visitRendered(child, visit);
    return;
  }
  if (!resolved || typeof resolved !== "object") return;

  const vnode = resolved as VNode;
  if (typeof vnode.tag === "function") {
    visitRendered(vnode.rendered, visit);
    return;
  }
  visit(vnode);
  visitRendered(vnode.props.children, visit);
}

function renderedText(node: unknown): string {
  const resolved = resolveNode(node);
  if (typeof resolved === "string" || typeof resolved === "number") return String(resolved);
  if (Array.isArray(resolved)) return resolved.map(renderedText).join("");
  if (!resolved || typeof resolved !== "object") return "";

  const vnode = resolved as VNode;
  return typeof vnode.tag === "function"
    ? renderedText(vnode.rendered)
    : renderedText(vnode.props.children);
}

function queryAllByTestId(root: unknown, testId: string): FakeElement[] {
  const matches: FakeElement[] = [];
  visitRendered(root, (vnode) => {
    if (vnode.props["data-testid"] === testId) matches.push(new FakeElement(vnode));
  });
  return matches;
}

function findVNode(root: unknown, predicate: (vnode: VNode) => boolean): VNode | undefined {
  let match: VNode | undefined;
  visitRendered(root, (vnode) => {
    if (match === undefined && predicate(vnode)) match = vnode;
  });
  return match;
}

const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;

function synchronousShow(props: Record<string, unknown>): unknown {
  const condition = typeof props.when === "function" ? props.when() : props.when;
  if (!condition) return props.fallback ?? null;
  if (typeof props.children !== "function") return props.children;
  return props.children(() => condition);
}

function synchronousFor(props: Record<string, unknown>): unknown[] {
  if (!Array.isArray(props.each)) return [];
  const renderItem = props.children as (item: unknown, index: () => number) => unknown;
  return props.each.map((item, index) => renderItem(item, () => index));
}

mock.module("solid-js", () => ({ ...Solid, For: synchronousFor, Show: synchronousShow }));

function invokeComponent(vnode: VNode): void {
  if (typeof vnode.tag !== "function") return;
  const component = vnode.tag as (props: Record<string, unknown>) => unknown;
  vnode.rendered = Solid.runWithOwner(Solid.getOwner(), () => component(vnode.props));
}

function createElement(
  tag: unknown,
  props: Record<string, unknown> | null,
  ...children: unknown[]
): VNode {
  const descriptors = Object.getOwnPropertyDescriptors(props ?? {});
  if (children.length > 0) {
    descriptors.children = {
      configurable: true,
      enumerable: true,
      value: children.length === 1 ? children[0] : children,
      writable: true,
    };
  }
  const merged = Object.defineProperties({}, descriptors) as Record<string, unknown>;
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

function surface(props: Record<string, unknown>): VNode {
  return createElement(props.as ?? "div", props);
}

function list(props: Record<string, unknown>): VNode {
  return createElement("div", null, props.children);
}

function listRow(props: Record<string, unknown>): VNode {
  return createElement(
    "div",
    { "data-testid": props.testId },
    props.leading,
    props.headline,
    props.support,
    props.trailing,
  );
}

function iconButton(props: Record<string, unknown>): VNode {
  return createElement("button", {
    "data-testid": props["data-testid"],
    "aria-label": props.label,
    onClick: props.onClick,
  });
}

function icon(props: Record<string, unknown>): VNode {
  return createElement("span", { "data-testid": "transfer-icon", "data-icon": props.name });
}

mock.module("../src/components/Settings/md/primitives.tsx", () => ({
  Icon: icon,
  IconButton: iconButton,
  List: list,
  ListRow: listRow,
  Surface: surface,
}));

// These imports must follow mock registration so the component binds the fake
// client renderer and primitives instead of browser-only modules.
const { TransferStack } = await import("../src/components/TransferCard.tsx");
const { addTransfer, clearTransfersForLogout, transfers } = await import("../src/store/transfers.ts");

const mountedRoots: Array<() => void> = [];

function mountTransferStack(): unknown {
  let tree: unknown;
  Solid.createRoot((dispose) => {
    mountedRoots.push(dispose);
    tree = TransferStack();
  });
  return tree;
}

beforeEach(() => {
  clearTransfersForLogout();
});

afterEach(() => {
  while (mountedRoots.length > 0) mountedRoots.pop()?.();
  clearTransfersForLogout();
});

describe("TransferStack", () => {
  test("aggregates two live jobs into one popup with independent dismissals", () => {
    addTransfer({
      id: "queued-upload",
      name: "awaiting-upload.tar",
      dir: "up",
      bytes_total: 4096,
      state: "queued",
    });
    addTransfer({
      id: "hashing-upload",
      name: "archive.tar",
      dir: "up",
      bytes_total: 4096,
      state: "hashing",
    });

    const tree = mountTransferStack();

    expect(queryAllByTestId(tree, "transfer-card")).toHaveLength(1);

    expect(queryAllByTestId(tree, "transfer-icon").map((icon) => icon.getAttribute("data-icon"))).toEqual(["upload", "upload"]);
    const rows = queryAllByTestId(tree, "transfer-row");
    expect(rows).toHaveLength(2);

    const queuedRow = rows.find((row) => row.textContent.includes("awaiting-upload.tar"));
    expect(queuedRow?.textContent).toContain("Queued…");

    const hashingRow = rows.find((row) => row.textContent.includes("archive.tar"));
    expect(hashingRow?.textContent).toContain("Checking…");

    const dismissButtons = queryAllByTestId(tree, "transfer-dismiss");
    expect(dismissButtons).toHaveLength(2);
    expect(dismissButtons.map((button) => button.getAttribute("aria-label"))).toEqual(
      expect.arrayContaining(["Dismiss awaiting-upload.tar", "Dismiss archive.tar"]),
    );
  });

  test("renders a decorative preview and retains its URL after image decode errors", () => {
    addTransfer({
      id: "preview-upload",
      name: "photo.png",
      dir: "up",
      bytes_total: 4096,
      state: "active",
      preview_url: "blob:photo",
    });
    const tree = mountTransferStack();
    const preview = findVNode(tree, (vnode) => vnode.tag === "img" && vnode.props["data-testid"] === "transfer-preview");

    expect(preview?.props.src).toBe("blob:photo");
    expect(preview?.props.alt).toBe("");
    expect(preview?.props.onError).toBeFunction();

    (preview?.props.onError as () => void)();
    expect(transfers["preview-upload"]?.preview_url).toBe("blob:photo");
  });
});
