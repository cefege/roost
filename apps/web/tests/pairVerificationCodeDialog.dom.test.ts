// Approver-dialog DOM test pins grouped code disclosure to the local modal and
// proves every dismissal path (close, Escape, backdrop, Cancel request) reaches
// the one cancel callback, which stays disabled while a denial is in flight.

import { describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import type { PairCodeDialogState } from "../src/components/PairVerificationCodeDialog.tsx";
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

let dialogProps: Record<string, unknown> | null = null;
setPairingPrimitiveCaptures({
  dialog: (props) => { dialogProps = props; },
});
mock.module("../src/components/Settings/md/primitives.tsx", () => pairingPrimitiveStubs);

// The component must load after its primitive mock is installed.
const { PairVerificationCodeDialog } = await import(
  "../src/components/PairVerificationCodeDialog.tsx"
);

/** Walks what actually rendered: a component contributes its output, so a
 *  closed `Show` hides its children exactly as the DOM would. */
function walkRendered(node: unknown, visit: (item: string | VNode) => void): void {
  let resolved = node;
  while (typeof resolved === "function") resolved = resolved();
  if (typeof resolved === "string" || typeof resolved === "number") {
    visit(String(resolved));
    return;
  }
  if (Array.isArray(resolved)) {
    for (const child of resolved) walkRendered(child, visit);
    return;
  }
  if (!resolved || typeof resolved !== "object") return;
  const vnode = resolved as VNode;
  visit(vnode);
  walkRendered(typeof vnode.tag === "function" ? vnode.rendered : vnode.props.children, visit);
}

function renderDialog(state: PairCodeDialogState) {
  const calls = { cancel: 0, reload: 0 };
  let rendered: unknown;
  const dispose = Solid.createRoot((disposeRoot) => {
    rendered = PairVerificationCodeDialog({
      open: true,
      verificationCode: "123456",
      requesterLabel: "Kitchen tablet",
      state,
      onCancel: () => { calls.cancel += 1; },
      onReload: () => { calls.reload += 1; },
    });
    return disposeRoot;
  });
  const text: string[] = [];
  const buttons = new Map<string, Record<string, unknown>>();
  let alerts = 0;
  walkRendered(rendered, (item) => {
    if (typeof item === "string") text.push(item);
    else {
      if (item.props.role === "alert") alerts += 1;
      if (typeof item.props["data-testid"] === "string") buttons.set(item.props["data-testid"], item.props);
    }
  });
  return {
    calls,
    dispose,
    text: text.join("").replace(/\s+/g, " ").trim(),
    alerts,
    button: (testId: string) => buttons.get(testId),
  };
}

describe("PairVerificationCodeDialog", () => {
  test("renders the code locally and routes every dismissal to the cancel callback", () => {
    const view = renderDialog("awaiting");
    try {
      expect(view.text).toContain("123 456");
      expect(view.text).toContain("Kitchen tablet");
      expect(view.text).toContain("closes automatically");
      const cancel = view.button("pair-verification-code-cancel")!;
      expect(cancel.disabled).toBe(false);
      // Kobalte reports the close button, Escape, and backdrop as onClose.
      (dialogProps?.onClose as () => void)();
      (cancel.onClick as () => void)();
      expect(view.calls.cancel).toBe(2);
      expect(view.button("pair-verification-code-reload")).toBeUndefined();
      expect(view.alerts).toBe(0);
    } finally {
      view.dispose();
    }
  });

  test("disables Cancel request while the denial is in flight", () => {
    const view = renderDialog("cancelling");
    try {
      const cancel = view.button("pair-verification-code-cancel")!;
      expect(cancel.disabled).toBe(true);
      expect(cancel["aria-busy"]).toBe(true);
      expect(view.text).toContain("Cancelling…");
      expect(view.text).not.toContain("Cancel request");
      expect(view.text).toContain("123 456");
    } finally {
      view.dispose();
    }
  });

  test("keeps the code but withdraws the progress claim when a reload is required", () => {
    const view = renderDialog("reload_required");
    try {
      expect(view.text).toContain("123 456");
      expect(view.text).not.toContain("closes automatically");
      expect(view.alerts).toBe(1);
      (view.button("pair-verification-code-reload")!.onClick as () => void)();
      expect(view.calls.reload).toBe(1);
      expect(view.button("pair-verification-code-cancel")!.disabled).toBe(false);
    } finally {
      view.dispose();
    }
  });
});
