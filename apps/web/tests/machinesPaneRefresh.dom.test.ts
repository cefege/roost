// MachinesPane refresh is a projection-only action: it reuses the worker
// hydration owner, retains existing rows when that owner reports failure, and
// never opens enrollment or invokes a deploy RPC.

import { afterEach, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";

type VNode = {
  tag: unknown;
  props: Record<string, unknown>;
  rendered?: unknown;
};

// The client renderer is selected by resolved test runtime URL.
const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
const refreshErrorWrites: string[] = [];
mock.module("solid-js", () => {
  const createSignal = ((initial: unknown) => {
    const [read, write] = Solid.createSignal(initial);
    return [read, (next: unknown) => {
      if (typeof next === "string") refreshErrorWrites.push(next);
      return write(next as never);
    }] as never;
  }) as unknown as typeof Solid.createSignal;
  return { ...Solid, createSignal };
});

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
  const vnode: VNode = { tag, props: merged };
  invokeComponent(vnode);
  return vnode;
}

const ReactShim = { Fragment: Symbol("Fragment"), createElement };
(globalThis as typeof globalThis & { React: unknown }).React = ReactShim;
mock.module("react/jsx-dev-runtime", () => ({
  Fragment: ReactShim.Fragment,
  jsxDEV(tag: unknown, props: Record<string, unknown> | null): VNode {
    const children = props?.children === undefined ? [] : [props.children];
    return createElement(tag, props, ...children);
  },
}));

const ButtonStub = (props: Record<string, unknown>): unknown => props.children;
mock.module("../src/components/Settings/md/primitives.tsx", () => ({
  Button: ButtonStub,
  Card: (props: Record<string, unknown>) => [props.trailing, props.children],
  EmptyState: (props: Record<string, unknown>) => props.children,
  List: (props: Record<string, unknown>) => props.children,
}));

mock.module("../src/components/MachineDeployDialog.tsx", () => ({ MachineDeployDialog: () => null }));
mock.module("../src/components/Settings/MachineCard.tsx", () => ({
  MachineCard: (props: { worker: { label: string } }) => props.worker.label,
}));

const rootStore = {
  workers: {
    ["a".repeat(64)]: { label: "Retained machine", last_seen_ms: 1 },
  },
};
mock.module("../src/store/root.ts", () => ({ rootStore }));

let refreshResult = Promise.resolve(true);
let refreshCalls = 0;
mock.module("../src/store/sync-bootstrap.ts", () => ({
  refreshCoordAndWorkers: () => {
    refreshCalls += 1;
    return refreshResult;
  },
}));

// MachinesPane binds its mocked store and refresh owner at module evaluation.
const { MachinesPane } = await import("../src/components/Settings/MachinesPane.tsx");

function resolvedNode(node: unknown): unknown {
  let resolved = node;
  while (typeof resolved === "function") resolved = resolved();
  return resolved;
}

function collect(node: unknown, output: VNode[] = []): VNode[] {
  const resolved = resolvedNode(node);
  if (Array.isArray(resolved)) {
    for (const child of resolved) collect(child, output);
    return output;
  }
  if (!resolved || typeof resolved !== "object") return output;
  const vnode = resolved as VNode;
  output.push(vnode);
  if (typeof vnode.tag === "function") collect(vnode.rendered, output);
  else collect(vnode.props.children, output);
  return output;
}

function text(node: unknown, output: string[] = []): string[] {
  const resolved = resolvedNode(node);
  if (typeof resolved === "string" || typeof resolved === "number") {
    output.push(String(resolved));
    return output;
  }
  if (Array.isArray(resolved)) {
    for (const child of resolved) text(child, output);
    return output;
  }
  if (!resolved || typeof resolved !== "object") return output;
  const vnode = resolved as VNode;
  if (typeof vnode.tag === "function") text(vnode.rendered, output);
  else text(vnode.props.children, output);
  return output;
}

const disposers: Array<() => void> = [];

function renderPane(): { tree: unknown; refreshButton: () => Record<string, unknown> | undefined } {
  let tree: unknown;
  Solid.createRoot((dispose) => {
    disposers.push(dispose);
    tree = MachinesPane();
  });
  return {
    tree,
    refreshButton: () => collect(tree)
      .find((vnode) => vnode.tag === ButtonStub && vnode.props["data-testid"] === "machines-refresh-status")
      ?.props,
  };
}

async function settleRefresh(): Promise<void> {
  for (let turn = 0; turn < 3; turn++) await Promise.resolve();
}


afterEach(() => {
  while (disposers.length > 0) disposers.pop()?.();
});

test("Refresh status only invokes worker hydration and retains known rows on error", async () => {
  const deferred = Promise.withResolvers<boolean>();
  refreshResult = deferred.promise;
  refreshCalls = 0;
  refreshErrorWrites.length = 0;
  const surface = renderPane();

  (surface.refreshButton()!.onClick as () => void)();
  expect(refreshCalls).toBe(1);
  expect(text(surface.tree).join(" ")).toContain("Retained machine");

  deferred.resolve(false);
  await settleRefresh();
  expect(refreshErrorWrites).toContain("Refresh failed. Last known machine status is still shown.");
});
