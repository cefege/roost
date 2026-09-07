// The terminal-content results header must state the TRUE session denominator
// and say out loud when coverage is partial: the coordinator caps how many
// sessions one page searches, so "32 of 32" hid every session past the cap.
// Bun uses Solid's server build by default, so this harness supplies the client
// runtime plus a minimal virtual JSX renderer; the M3 primitives are stubbed
// because their barrel registers browser custom elements.

import { afterEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import type { SessionsSearchGlobalMatch } from "@roost/shared/proto/coordinator_pb";
import type { GlobalContentSearchController } from "../src/lib/globalContentSearchController.ts";
import type { NavigationSearchDocument } from "../src/store/navigation-search.ts";

type VNode = {
  tag: unknown;
  props: Record<string, unknown>;
  rendered?: unknown;
};

const disposedRoots: Array<() => void> = [];
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

mock.module("../src/components/Settings/md/primitives.tsx", () => ({
  Button: passthrough,
  EmptyState: (props: Record<string, unknown>) => [props.title, props.supporting],
  List: passthrough,
  ListRow: (props: Record<string, unknown>) => [props.headline, props.support],
  Surface: passthrough,
}));

// Static imports would bind the real primitive barrel — and its browser custom
// element registrations — before the stubs above are installed.
const { GlobalSearchContentResults } = await import(
  "../src/components/GlobalSearchContentResults.tsx"
);

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
  collectText(typeof vnode.tag === "function" ? vnode.rendered : vnode.props.children, output);
  return output;
}

function controllerFor(
  searchedSessions: number,
  eligibleSessions: number,
): GlobalContentSearchController {
  return {
    matches: () => [] as readonly SessionsSearchGlobalMatch[],
    partials: () => [],
    nextCursor: () => undefined,
    searchedSessions: () => searchedSessions,
    eligibleSessions: () => eligibleSessions,
    truncated: () => false,
    debouncing: () => false,
    loading: () => false,
    error: () => null,
    retryable: () => false,
    hasSearched: () => true,
  } as unknown as GlobalContentSearchController;
}

/** Joined text of the rendered summary with runs of whitespace collapsed: the
 *  component splits a sentence across JSX expressions, so adjacent text nodes
 *  would otherwise read as double-spaced. */
function renderSummary(searched: number, eligible: number): string {
  let text = "";
  Solid.createRoot((dispose) => {
    disposedRoots.push(dispose);
    text = collectText(GlobalSearchContentResults({
      controller: controllerFor(searched, eligible),
      documents: () => [] as readonly NavigationSearchDocument[],
      query: () => "needle",
      onOpenResult: () => undefined,
    })).join(" ");
  });
  return text.replace(/\s+/g, " ").trim();
}

afterEach(() => {
  while (disposedRoots.length > 0) disposedRoots.pop()?.();
});

describe("terminal content result coverage", () => {
  test("names the sessions left unsearched when the page cap hides them", () => {
    const text = renderSummary(32, 100);
    expect(text).toContain("32 of 100 sessions");
    expect(text).toContain("coverage is partial");
    expect(text).toContain("Search incomplete");
    expect(text).toContain("68 of 100 eligible sessions were not searched.");
  });

  test("claims complete coverage only when every eligible session was searched", () => {
    const text = renderSummary(7, 7);
    expect(text).toContain("across all 7 sessions searched");
    expect(text).not.toContain("coverage is partial");
    expect(text).not.toContain("Search incomplete");
  });

  test("uses singular wording for a single unsearched session", () => {
    expect(renderSummary(1, 2)).toContain("1 of 2 eligible session was not searched.");
  });
});
