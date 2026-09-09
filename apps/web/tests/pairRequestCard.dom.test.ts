// PairRequestCard is the one approval surface that renders server-observed
// pairing provenance. Bun has no browser DOM, so this suite uses the same
// client-Solid virtual renderer as the existing DOM tests and stubs only the
// M3 primitives that register browser custom elements.

import { afterEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import type { PairRequest } from "../src/store/root.ts";

type VNode = {
  tag: unknown;
  props: Record<string, unknown>;
  rendered?: unknown;
};

const disposedRoots: Array<() => void> = [];
const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
mock.module("solid-js", () => Solid);
mock.module("../src/components/Settings/md/tokens.css", () => ({}));

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

mock.module("../src/components/Settings/md/primitives.tsx", () => ({
  Button: passthrough,
  Card: (props: Record<string, unknown>) => [props.title, props.children],
  Chip: (props: Record<string, unknown>) => props.label,
  List: passthrough,
  ListRow: (props: Record<string, unknown>) => [props.headline, props.support, props.trailing],
  StatusDot: () => null,
}));

const { PairRequestCard } = await import("../src/components/PairRequestCard.tsx");

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

const baseRequest: PairRequest = {
  ephemeral_id: "0123456789abcdef0123456789abcdef",
  label: "Self-declared browser",
  created_at_ms: Date.now() - 120_000,
  userAgent: "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) Safari/604.1",
  clientBrowser: "Safari",
  clientOs: "iOS",
  clientDeviceType: "mobile",
  sourceIp: "203.0.113.7",
  countryCode: "DE",
  region: "Berlin",
  city: "Berlin",
  edgeIdentityProvider: "cloudflare-access",
  edgeIdentity: "owner@example.com",
  edgeIdentityVerified: true,
  expiresAtMs: Date.now() + 8 * 60_000,
};

function makeRequest(overrides: Partial<PairRequest>): PairRequest {
  return { ...baseRequest, ...overrides };
}

function renderCard(request: PairRequest): string {
  let rendered: unknown;
  let dispose: (() => void) | undefined;
  Solid.createRoot((rootDispose) => {
    dispose = rootDispose;
    rendered = PairRequestCard({
      request,
      onApprove: () => undefined,
      onDeny: () => undefined,
    });
  });
  try {
    return collectText(rendered).join(" ").replace(/\s+/g, " ").trim();
  } finally {
    dispose?.();
  }
}

afterEach(() => {
  while (disposedRoots.length > 0) disposedRoots.pop()?.();
});

describe("PairRequestCard", () => {
  test("renders populated device, location, network, identity, and raw UA details", () => {
    const text = renderCard(baseRequest);

    expect(text).toContain("New browser wants to pair");
    expect(text).toContain("Safari · iOS");
    expect(text).toContain("mobile");
    expect(text).toContain("Berlin, Berlin, DE");
    expect(text).toContain("IP 203.0.113.7");
    expect(text).toContain("Signed in as owner@example.com");
    expect(text).toContain(baseRequest.userAgent);
    expect(text).toContain(`Code: ${baseRequest.ephemeral_id}`);
    expect(text).toContain("Expires in");
  });

  test("renders legacy requests with explicit unavailable provenance", () => {
    const text = renderCard(makeRequest({
      userAgent: "",
      clientBrowser: "",
      clientOs: "",
      clientDeviceType: "",
      sourceIp: "",
      countryCode: "",
      region: "",
      city: "",
      edgeIdentityProvider: "",
      edgeIdentity: "",
      edgeIdentityVerified: false,
      expiresAtMs: 0,
    }));

    expect(text).toContain("Self-declared browser");
    expect(text).toContain("Location unavailable");
    expect(text).toContain("IP unavailable");
    expect(text).toContain("No front-door identity");
    expect(text).not.toContain("verified");
  });

  test("labels a non-empty unverified identity as a claim, never as verified", () => {
    const text = renderCard(makeRequest({
      edgeIdentity: "claimed@example.com",
      edgeIdentityVerified: false,
    }));

    expect(text).toContain("Claimed identity claimed@example.com");
    expect(text).not.toContain("verified");
    expect(text).not.toContain("Signed in as claimed@example.com");
  });

  test("does not render an expired request", () => {
    const text = renderCard(makeRequest({ expiresAtMs: Date.now() - 1 }));

    expect(text).toBe("");
  });
});
