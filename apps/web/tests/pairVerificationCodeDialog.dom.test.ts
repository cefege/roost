// Approver-dialog DOM test pins grouped code disclosure to the local modal and
// proves dismissal is not a requester confirmation action.

import { describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import {
  pairingPrimitiveStubs,
  setPairingPrimitiveCaptures,
} from "./helpers/pairingPrimitiveStubs.ts";

type VNode = {
  tag: unknown;
  props: Record<string, unknown>;
  rendered?: unknown;
};

// Dynamic loading follows the client-Solid mock required by this DOM suite.
const Solid = await import(new URL("./solid.js", import.meta.resolve("solid-js")).href) as typeof SolidApi;
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
(globalThis as typeof globalThis & { React: unknown }).React = ReactShim;
mock.module("react/jsx-dev-runtime", () => ({
  Fragment: ReactShim.Fragment,
  jsxDEV(tag: unknown, props: Record<string, unknown> | null): VNode {
    const children = props?.children === undefined ? [] : [props.children];
    return createElement(tag, props, ...children);
  },
}));

let dismissButtonProps: Record<string, unknown> | null = null;
setPairingPrimitiveCaptures({
  button: (props) => {
    if (props["data-testid"] === "pair-verification-code-done") {
      dismissButtonProps = props;
    }
  },
});
mock.module("../src/components/Settings/md/primitives.tsx", () => pairingPrimitiveStubs);

// The component must load after its primitive mock is installed.
const { PairVerificationCodeDialog } = await import(
  "../src/components/PairVerificationCodeDialog.tsx"
);

function collectText(node: unknown, output: string[] = []): string[] {
  let resolved = node;
  while (typeof resolved === "function") resolved = resolved();
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
  collectText(typeof vnode.tag === "function" ? vnode.rendered : vnode.props.children, output);
  return output;
}
describe("PairVerificationCodeDialog", () => {
  test("renders the approval code locally and dismissal only closes the dialog", () => {
    let closes = 0;
    let rendered: unknown;
    const dispose = Solid.createRoot((disposeRoot) => {
      rendered = PairVerificationCodeDialog({
        open: true,
        verificationCode: "123456",
        requesterLabel: "Kitchen tablet",
        onClose: () => { closes += 1; },
      });
      return disposeRoot;
    });

    const text = collectText(rendered).join(" ").replace(/\s+/g, " ").trim();
    expect(text).toContain("123 456");
    expect(text).toContain("Kitchen tablet");
    expect(text).toContain("Dismissing this dialog does not authorize the browser.");
    (dismissButtonProps?.onClick as (() => void))();
    expect(closes).toBe(1);
    dispose();
  });
});
