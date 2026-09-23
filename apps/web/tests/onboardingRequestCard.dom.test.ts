// Requester-card DOM coverage verifies the observable recovery controls rather
// than component wiring. It uses the shared primitive stub because Bun has no
// custom-element implementation for the Material primitives.

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

// Dynamic loading lets this suite install the client-Solid and JSX shims first.
const Solid = await import(new URL("./solid.js", import.meta.resolve("solid-js")).href) as typeof SolidApi;
mock.module("solid-js", () => Solid);

function invokeComponent(vnode: VNode): void {
  const component = vnode.tag;
  if (typeof component !== "function") return;
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

const buttons: Array<Record<string, unknown>> = [];
setPairingPrimitiveCaptures({
  button: (props) => buttons.push(props),
});
mock.module("../src/components/Settings/md/primitives.tsx", () => pairingPrimitiveStubs);

// The primitive mock must precede this component's import.
const { OnboardingRequestCard } = await import("../src/components/OnboardingRequestCard.tsx");

function renderRequestCard(pollStatus: "verification_required" | "error", onStart: () => void): string {
  let rendered: unknown;
  let dispose: (() => void) | undefined;
  Solid.createRoot((disposeRoot) => {
    dispose = disposeRoot;
    rendered = OnboardingRequestCard({
      ephemeralId: "0123456789abcdef0123456789abcdef",
      pollStatus,
      verificationCode: "",
      confirmationError: null,
      busy: false,
      onStart,
      onVerificationCodeInput: () => undefined,
      onConfirm: () => undefined,
    });
  });
  try {
    return collectText(rendered).join(" ").replace(/\s+/g, " ").trim();
  } finally {
    dispose?.();
  }
}

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

describe("OnboardingRequestCard", () => {
  test("offers Start over while a code is waiting and starts a replacement request", () => {
    buttons.length = 0;
    let starts = 0;
    const text = renderRequestCard("verification_required", () => { starts += 1; });

    expect(text).toContain("Approval received. Enter the code shown on the paired browser.");
    expect(text).toContain("Start over");
    const startOver = buttons.find((button) => button.children === "Start over");
    (startOver?.onClick as (() => void))();
    expect(starts).toBe(1);
  });

  test("offers Request again after a terminal error", () => {
    buttons.length = 0;
    let starts = 0;
    const text = renderRequestCard("error", () => { starts += 1; });

    expect(text).toContain("Poll error — try again.");
    expect(text).toContain("Request again");
    const requestAgain = buttons.find((button) => button.children === "Request again");
    (requestAgain?.onClick as (() => void))();
    expect(starts).toBe(1);
  });
});
