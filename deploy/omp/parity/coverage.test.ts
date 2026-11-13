import { describe, expect, test } from "bun:test";
import type { InterfaceInventory, InventoryEntry } from "./inventory-types";
import type { ProbeVisit } from "./probe";
import type { Observation } from "./scenario";
import { assertCoverage, CoverageFailure, type ScenarioEvidence } from "./coverage";
import { provenanceReasons } from "./coverage-provenance";

const source = "packages/tui/src/component-boundary.ts";
const component: InventoryEntry = { id: `component:${source}#Boundary`, category: "component", source, line: 1, details: {
  name: "Boundary", nativeStatus: "native-hooks", hooks: [
    { name: "render", inherited: false, implementation: { source, line: 2 } },
    { name: "describe", inherited: false, implementation: { source, line: 3 } },
  ], nativeOwners: [], terminalOwners: [], nullReturns: [], delegatedHelpers: [],
} };
const inventory: InterfaceInventory = { revision: "unit-boundary", entries: [component], runtimeSchema: {
  Sentinel: { source: "packages/wire/src/tsp.ts", line: 1, kind: "alias", parameters: [], extends: [], definition: { source: "packages/wire/src/tsp.ts", line: 1, kind: "literal", value: null } },
} };
function scenario(): ScenarioEvidence {
  return { id: "boundary", covers: [component.id], visits: [], wire: [], checkpoints: [{ name: "visible", passed: true, comparisons: [{ name: "visible value", terminal: { type: "screen", pattern: "value" }, browser: { type: "dom", selector: "button", read: "text" }, terminalValue: "value", browserValue: "value", passed: true,
    terminalMatch: { text: "value" }, browserTarget: { visible: true, attributes: {}, state: {} } }] }] };
}
function visit(method: string, line: number, adapter: "terminal" | "browser", sequence: number): ProbeVisit {
  return { type: "visit", component: `${source}#Boundary`, method, source, line, adapter, pid: 1, generation: "unit-boundary", sequence };
}
function failure(value: ScenarioEvidence): CoverageFailure {
  try { assertCoverage(inventory, [value]); } catch (error) {
    if (error instanceof CoverageFailure) return error;
    throw error;
  }
  throw new Error("Invalid coverage evidence was accepted");
}

describe("blanket coverage evidence boundaries", () => {
  test("covers labels and passing unrelated comparisons do not invent production execution", () => {
    const result = failure(scenario());
    expect(result.report.covered).toBe(0);
    expect(result.report.invalid).toEqual([]);
    expect(result.report.entries[0]!.missingEvidence).toContain("Unexercised production hook render");
  });

  test("passed booleans cannot override mismatching normalized observations", () => {
    const value = scenario();
    value.checkpoints[0]!.comparisons[0]!.browserValue = "different";
    expect(failure(value).report.invalid.join("\n")).toContain("Actual normalized observations differ");
  });

  test("expected native failures are not positive parity coverage", () => {
    const value = scenario();
    const comparison = value.checkpoints[0]!.comparisons[0]!;
    comparison.browserValue = "unsupported";
    comparison.expectedByAdapter = { reason: "unsupported-interactive-component", terminal: "value", browser: "unsupported" };
    expect(failure(value).report.invalid.join("\n")).toContain("positive meaningful comparison");
  });

  test("null-cleared observations alone do not blanket-cover a component", () => {
    const value = scenario();
    const comparison = value.checkpoints[0]!.comparisons[0]!;
    comparison.terminalValue = null;
    comparison.browserValue = null;
    expect(failure(value).report.invalid.join("\n")).toContain("positive meaningful comparison");
  });

  test("duplicate raw rows cannot be counted as separate executions", () => {
    const value = scenario();
    const row = visit("render", 2, "terminal", 1);
    value.visits = [row, { ...row }];
    expect(failure(value).report.invalid.join("\n")).toContain("Duplicate observed provenance row");
  });

  test("browser render fallback is not evidence of terminal-side execution", () => {
    const reasons = provenanceReasons(component, [visit("render", 2, "browser", 1), visit("describe", 3, "browser", 2)], new Set());
    expect(reasons).toContain("Unexercised production hook render");
  });

  test("terminal render plus a non-null browser descriptor on the same component covers it", () => {
    const value = scenario();
    value.visits = [visit("render", 2, "terminal", 1), visit("describe", 3, "browser", 2), { ...visit("return:describe", 3, "browser", 3), arguments: [{ result: "value" }] }];
    expect(() => assertCoverage(inventory, [value])).not.toThrow();
  });

  test("a composer hook counts as native only inside a browser describeComposerChrome call", () => {
    const style = "packages/tui/src/components/composer/band.ts";
    const entry: InventoryEntry = { id: "composer:band", category: "composer", source: style, line: 10, details: { source: style, line: 10 } };
    const hook = (method: string, adapter: "terminal" | "browser", sequence: number): ProbeVisit => ({ type: "visit", component: `${style}#bandComposerStyle`, method, source: style, line: 10, adapter, pid: 1, generation: "unit-boundary", sequence });
    const chrome = (method: string, sequence: number): ProbeVisit => ({ type: "visit", component: "chrome", method, source: "packages/tui/src/components/composer/native-chrome.ts", line: 59, adapter: "browser", pid: 1, generation: "unit-boundary", sequence });
    const remoteTerminal = [hook("renderTop", "terminal", 1), chrome("describeComposerChrome", 2), chrome("return:describeComposerChrome", 3), hook("renderTop", "browser", 4)];
    expect(provenanceReasons(entry, remoteTerminal, new Set())).toContain("No actual registry native descriptor execution");
    const nested = [hook("renderTop", "terminal", 1), chrome("describeComposerChrome", 2), hook("renderTop", "browser", 3), chrome("return:describeComposerChrome", 4)];
    expect(provenanceReasons(entry, nested, new Set())).toEqual([]);
  });

  test("unsupported native source states remain failures even when hooks execute", () => {
    const entry = { ...component, details: { ...component.details, nativeStatus: "null-description" } };
    const reasons = provenanceReasons(entry, [visit("render", 2, "terminal", 1), visit("describe", 3, "browser", 2)], new Set());
    expect(reasons).toContain("Unsupported native source state: null-description");
  });

  test("a checked but unnegotiated feature is not supported-feature coverage", () => {
    const entry: InventoryEntry = { id: "feature:input", category: "feature", source, line: 4, details: { name: "input", checks: [{ source, line: 4 }] } };
    const row = { ...visit("feature", 4, "browser", 1), arguments: ["input"] };
    expect(provenanceReasons(entry, [row], new Set())).toContain("No negotiated feature and executed canonical check with its actual name");
  });

  test("only a hook no source ever reads is exempt from execution", () => {
    const hooks = [{ name: "render", inherited: false, referenced: true, implementation: { source, line: 2 } }, { name: "renderContent", inherited: true, referenced: false, implementation: { source, line: 5 } }];
    const entry: InventoryEntry = { ...component, details: { ...component.details, hooks } };
    const reasons = provenanceReasons(entry, [], new Set());
    expect(reasons).toContain("Unexercised production hook render");
    expect(reasons).not.toContain("Unexercised production hook renderContent");
  });

  test("a kind fallback helper is exempt only with the host's advertisement proof", () => {
    const gate = { kind: "tool", source, line: 6, expression: "dataFirst" };
    const helper = (kindGate: Record<string, unknown>) => ({ owner: component.id, hook: "describe", expression: "describeCard", source, line: 7, kindGate });
    const reasons = (kindGate: Record<string, unknown>) => provenanceReasons({ ...component, details: { ...component.details, delegatedHelpers: [helper(kindGate)] } }, [], new Set());
    expect(reasons({ ...gate, hostAdvertised: true, host: { source: "crates/roost-web/src/open.rs", line: 1 } })).not.toContain("Unexercised delegated helper describeCard");
    expect(reasons({ ...gate, hostAdvertised: false, host: { source: "crates/roost-web/src/open.rs", line: 1 } })).toContain("Unexercised delegated helper describeCard");
    expect(reasons({ ...gate, hostAdvertised: true })).toContain("Unexercised delegated helper describeCard");
  });

  test("a hook or an empty return behind a host-advertised kind is exempt only with the host's proof", () => {
    const proof = { kind: "picker", source, line: 6, expression: "cx.supports(\"picker\")", hostAdvertised: true, host: { source: "crates/roost-web/src/open.rs", line: 1 } };
    const hook = (kindGate?: Record<string, unknown>) => ({ name: "describeList", inherited: false, referenced: true, implementation: { source, line: 9 }, ...(kindGate ? { kindGate } : {}) });
    const reasons = (kindGate: Record<string, unknown> | undefined, extra: Record<string, unknown> = {}) => provenanceReasons({ ...component, details: { ...component.details, hooks: [hook(kindGate)], ...extra } }, [], new Set());
    expect(reasons(proof)).not.toContain("Unexercised production hook describeList");
    expect(reasons({ ...proof, hostAdvertised: false })).toContain("Unexercised production hook describeList");
    expect(reasons({ ...proof, host: undefined })).toContain("Unexercised production hook describeList");
    expect(reasons(undefined)).toContain("Unexercised production hook describeList");
    const nullReturns = (kindGate?: Record<string, unknown>) => ({ nullReturns: [{ hook: "describeList", source, line: 10, ...(kindGate ? { kindGate } : {}) }] });
    expect(reasons({ ...proof, hostAdvertised: false }, nullReturns(proof))).not.toContain("Unexercised conditional native empty-return path");
    expect(reasons({ ...proof, hostAdvertised: false }, nullReturns())).toContain("Unexercised conditional native empty-return path");
    expect(reasons({ ...proof, hostAdvertised: false }, nullReturns({ ...proof, hostAdvertised: false }))).toContain("Unexercised conditional native empty-return path");
  });

  test("calling a shared handler under another alias cannot cover an unexecuted alias", () => {
    const entry: InventoryEntry = { id: "command:exit", category: "command", source, line: 4, details: { handlers: { handleTui: { implementation: { source, line: 4 } } } } };
    const rows = ["terminal", "browser"].map((adapter, index) => ({ ...visit("shutdownHandlerTui", 4, adapter as "terminal" | "browser", index + 1), arguments: [{ name: "quit", text: "/quit", args: "" }] }));
    expect(provenanceReasons(entry, rows, new Set())).toContain("No invocation of the actual command/subcommand/alias handler with its parsed arguments on both adapters");
  });

  test.each(["visible", "rightGap", "bottomGap", "foreground", "background"] as const)("accepts the declared current-screen %s read without granting missing coverage", read => {
    const value = scenario();
    value.covers = [];
    value.checkpoints[0]!.comparisons[0]!.terminal = { type: "screen", pattern: "value", read };
    expect(failure(value).report.invalid).toEqual([]);
  });

  test.each(["css", "visible", "rightGap", "bottomGap"] as const)("accepts the declared DOM %s read without granting missing coverage", read => {
    const value = scenario();
    value.covers = [];
    value.checkpoints[0]!.comparisons[0]!.browser = { type: "dom", selector: "button", read, ...(read === "css" ? { property: "color" } : {}) };
    expect(failure(value).report.invalid).toEqual([]);
  });

  test("ANSI history is terminal-only and cannot alone cover a visible component", () => {
    const value = scenario();
    value.checkpoints[0]!.comparisons[0]!.terminal = { type: "ansi", pattern: "value" };
    expect(failure(value).report.invalid.join("\n")).toContain("positive current terminal screen and browser DOM comparison");
    value.covers = [];
    expect(failure(value).report.invalid).toEqual([]);
    value.checkpoints[0]!.comparisons[0]!.browser = { type: "ansi", pattern: "value" };
    expect(failure(value).report.invalid.join("\n")).toContain("Invalid terminal screen/ANSI observation");
    value.checkpoints[0]!.comparisons[0]!.browser = { type: "dom", selector: "button", read: "text" };
    value.checkpoints[0]!.comparisons[0]!.terminal = { type: "ansi", pattern: "value", read: "visible" } as unknown as Observation;
    expect(failure(value).report.invalid.join("\n")).toContain("ANSI history observations have no screen read");
  });

  test("ANSI history requires a separate positive current rendering comparison for visible UI coverage", () => {
    const value = scenario();
    const current = value.checkpoints[0]!.comparisons[0]!;
    value.checkpoints[0]!.comparisons.unshift({ ...current, name: "emitted history", terminal: { type: "ansi", pattern: "value" } });
    const report = failure(value).report;
    expect(report.invalid.join("\n")).not.toContain("positive current terminal screen and browser DOM comparison");
    expect(report.entries[0]!.missingEvidence).toContain("Unexercised production hook render");
  });

  test("CSS reads require an actual property and unknown screen/DOM reads remain refused", () => {
    const value = scenario();
    value.covers = [];
    value.checkpoints[0]!.comparisons[0]!.browser = { type: "dom", selector: "button", read: "css" };
    expect(failure(value).report.invalid.join("\n")).toContain("Invalid browser DOM observation");
    value.checkpoints[0]!.comparisons[0]!.browser = { type: "dom", selector: "button", read: "unknown" } as unknown as Observation;
    expect(failure(value).report.invalid.join("\n")).toContain("Invalid browser DOM observation");
    value.checkpoints[0]!.comparisons[0]!.browser = { type: "dom", selector: "button", read: "text" };
    value.checkpoints[0]!.comparisons[0]!.terminal = { type: "screen", pattern: "value", read: "unknown" } as unknown as Observation;
    expect(failure(value).report.invalid.join("\n")).toContain("Unsupported terminal screen read");
  });
});
