// TransferStack DOM coverage — one popup must aggregate every live transfer record.
// Bun renders TSX through a small client-Solid virtual renderer, so Portal output
// lands in this fake document body while primitive stubs retain observable semantics.

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

class FakeBody {
  private readonly children: unknown[] = [];

  appendChild(child: unknown): unknown {
    this.children.push(child);
    return child;
  }

  replaceChildren(): void {
    this.children.length = 0;
  }

  querySelectorAll(selector: string): FakeElement[] {
    const testId = /^\[data-testid="([^"]+)"\]$/.exec(selector)?.[1];
    if (!testId) throw new Error(`unsupported selector ${selector}`);

    const matches: FakeElement[] = [];
    for (const child of this.children) {
      visitRendered(child, (vnode) => {
        if (vnode.props["data-testid"] === testId) matches.push(new FakeElement(vnode));
      });
    }
    return matches;
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

const fakeDocument = { body: new FakeBody() };
Object.defineProperty(globalThis, "document", { configurable: true, value: fakeDocument });

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

function portal(props: Record<string, unknown>): null {
  (props.mount as FakeBody).appendChild(props.children);
  return null;
}

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

mock.module("solid-js/web", () => ({ Portal: portal }));
mock.module("../src/components/Settings/md/primitives.tsx", () => ({
  IconButton: iconButton,
  List: list,
  ListRow: listRow,
  Surface: surface,
}));

// These imports must follow mock registration so the component binds the fake
// client renderer, Portal, and primitives instead of browser-only modules.
const { TransferStack } = await import("../src/components/TransferCard.tsx");
const { addTransfer, clearTransfersForLogout } = await import("../src/store/transfers.ts");

const mountedRoots: Array<() => void> = [];

function mountTransferStack(): void {
  Solid.createRoot((dispose) => {
    mountedRoots.push(dispose);
    visitRendered(TransferStack(), () => undefined);
  });
}

beforeEach(() => {
  clearTransfersForLogout();
  fakeDocument.body.replaceChildren();
});

afterEach(() => {
  while (mountedRoots.length > 0) mountedRoots.pop()?.();
  clearTransfersForLogout();
  fakeDocument.body.replaceChildren();
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

    mountTransferStack();

    expect(fakeDocument.body.querySelectorAll('[data-testid="transfer-card"]')).toHaveLength(1);

    const rows = fakeDocument.body.querySelectorAll('[data-testid="transfer-row"]');
    expect(rows).toHaveLength(2);

    const queuedRow = rows.find((row) => row.textContent.includes("awaiting-upload.tar"));
    expect(queuedRow?.textContent).toContain("Queued…");

    const hashingRow = rows.find((row) => row.textContent.includes("archive.tar"));
    expect(hashingRow?.textContent).toContain("Checking…");

    const dismissButtons = fakeDocument.body.querySelectorAll('[data-testid="transfer-dismiss"]');
    expect(dismissButtons).toHaveLength(2);
    expect(dismissButtons.map((button) => button.getAttribute("aria-label"))).toEqual(
      expect.arrayContaining(["Dismiss awaiting-upload.tar", "Dismiss archive.tar"]),
    );
  });
});
