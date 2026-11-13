import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { componentInventory } from "./inventory-components";
import { classifyHostKindFallbacks } from "./inventory-kind-gates";
import { SourceIndex } from "./inventory-source";
import type { InventoryEntry } from "./inventory-types";

const roots: string[] = [];
afterAll(() => { for (const root of roots) rmSync(root, { recursive: true, force: true }); });

function inventory(text: string): Map<string, InventoryEntry> {
  const root = mkdtempSync(path.join(tmpdir(), "kind-gates-"));
  roots.push(root);
  mkdirSync(path.join(root, "packages/tui/src"), { recursive: true });
  writeFileSync(path.join(root, "packages/tui/package.json"), JSON.stringify({ name: "@unit/tui" }));
  writeFileSync(path.join(root, "packages/tui/src/index.ts"), "export {};\n");
  writeFileSync(path.join(root, "packages/tui/src/globals.ts"), "declare global { type NativeNode = Record<string, unknown>; interface DescribeContext { supports(kind: string): boolean } }\nexport {};\n");
  writeFileSync(path.join(root, "packages/tui/src/subject.ts"), text);
  const entries = componentInventory(new SourceIndex(root));
  classifyHostKindFallbacks(entries, { kinds: new Set(["picker"]), source: "crates/web/open.rs", line: 1 });
  return new Map(entries.map(entry => [entry.id.split("#")[1]!, entry]));
}

const hooks = (entry: InventoryEntry) => entry.details.hooks as Array<{ name: string; kindGate?: { kind: string; hostAdvertised: boolean } }>;
const hook = (entry: InventoryEntry, name: string) => hooks(entry).find(candidate => candidate.name === name)!;

const dashboard = (callSite: string) => `export class List {
  describeList(): NativeNode { return {}; }
}
export class Dashboard {
  #list = new List();
  nativeSheet(cx: DescribeContext): boolean { return cx.supports("picker"); }
  describe(cx: DescribeContext): NativeNode {
    const sheet = this.nativeSheet(cx);
    return sheet ? {} : this.#page();
  }
  #page(): NativeNode { ${callSite} }
  render(width: number): string[] { return [String(width)]; }
}
`;

describe("hooks reached only without a host-advertised kind", () => {
  test("a hook called only from the rejected branch, through a predicate method and a private page, is gated and proven advertised", () => {
    const list = inventory(dashboard("return this.#list.describeList();")).get("List")!;
    expect(hook(list, "describeList").kindGate).toMatchObject({ kind: "picker", hostAdvertised: true, host: { source: "crates/web/open.rs", line: 1 } });
  });

  test("any caller outside the gated branch makes the hook ungated", () => {
    const entries = inventory(`${dashboard("return this.#list.describeList();")}export function other(list: List): NativeNode { return list.describeList(); }\n`);
    expect(hook(entries.get("List")!, "describeList").kindGate).toBeUndefined();
  });

  test("a reference that is not a call (callback, alias) makes the hook ungated", () => {
    const entries = inventory(dashboard("const make = this.#list.describeList; return make();"));
    expect(hook(entries.get("List")!, "describeList").kindGate).toBeUndefined();
  });

  test("a hook with no resolvable caller is never gated", () => {
    const entries = inventory(dashboard("return {};"));
    expect(hook(entries.get("List")!, "describeList").kindGate).toBeUndefined();
  });

  test("a hook that overrides a base declaration is never gated, since the base type's callers resolve elsewhere", () => {
    const entries = inventory(`export class Base { describeList(): NativeNode { return {}; } }
export class Derived extends Base {
  describeList(): NativeNode { return { n: 1 }; }
  describe(cx: DescribeContext): NativeNode { return cx.supports("picker") ? {} : this.describeList(); }
  render(width: number): string[] { return [String(width)]; }
}
`);
    expect(hook(entries.get("Derived")!, "describeList").kindGate).toBeUndefined();
  });

  test("a kind the browser does not advertise is recorded as not advertised", () => {
    const text = dashboard("return this.#list.describeList();").replaceAll('"picker"', '"popover"');
    expect(hook(inventory(text).get("List")!, "describeList").kindGate).toMatchObject({ kind: "popover", hostAdvertised: false });
  });

  test("an empty return after a negated supports test is gated, a null returned when the kind IS supported is not", () => {
    const entries = inventory(`export class Viewer {
  describe(cx: DescribeContext): NativeNode | null {
    if (!cx.supports("picker")) return null;
    if (cx.supports("list")) return null;
    return {};
  }
  render(width: number): string[] { return [String(width)]; }
}
`);
    const nullReturns = entries.get("Viewer")!.details.nullReturns as Array<{ line: number; kindGate?: { kind: string; hostAdvertised: boolean } }>;
    expect(nullReturns).toHaveLength(2);
    expect(nullReturns[0]!.kindGate).toMatchObject({ kind: "picker", hostAdvertised: true });
    expect(nullReturns[1]!.kindGate).toBeUndefined();
  });
  const parameterised = (extraCall: string, secondArgument = "chart") => `export class Report {
  describe(cx: DescribeContext): NativeNode {
    const chart = cx.supports("chart");
    return this.#overview(${secondArgument});${extraCall}
  }
  #overview(chart: boolean): NativeNode { return chart ? this.describeFigure() : this.describeTable(); }
  describeFigure(): NativeNode { return {}; }
  describeTable(): NativeNode { return {}; }
  render(width: number): string[] { return [String(width)]; }
}
`;
  const gateOf = (text: string, name: string) => (inventory(text).get("Report")!.details.hooks as Array<{ name: string; kindGate?: { kind: string; hostAdvertised: boolean } }>).find(candidate => candidate.name === name)!.kindGate;

  test("a branch on a parameter every caller fills with one supports test gates the rejected arm", () => {
    expect(gateOf(parameterised(""), "describeTable")).toMatchObject({ kind: "chart", hostAdvertised: false });
    expect(gateOf(parameterised(""), "describeFigure")).toBeUndefined();
  });

  test("a caller passing anything else for that parameter leaves the arm ungated", () => {
    expect(gateOf(parameterised("", "true"), "describeTable")).toBeUndefined();
    expect(gateOf(parameterised("\n    this.#overview(false);"), "describeTable")).toBeUndefined();
  });
});
