import { describe, expect, test } from "bun:test";
import type { InventoryEntry } from "./inventory-types";
import type { ProbeVisit } from "./probe";
import { provenanceReasons } from "./coverage-provenance";

const partSource = "packages/tui/src/chat/part.ts";
const ownerSource = "packages/tui/src/chat/owner.ts";
const ownerId = `component:${ownerSource}#Owner`;
const partIdentity = `${partSource}#Part`;
const chromeSource = "packages/tui/src/components/composer/native-chrome.ts";
const styleSource = "packages/tui/src/components/composer/band.ts";
const styleIdentity = `${styleSource}#BandStyle`;

const baseDetails = { nullReturns: [], delegatedHelpers: [], nativeOwners: [], terminalOwners: [] };
const hook = (name: string, source: string, line: number) => ({ name, inherited: false, referenced: true, implementation: { source, line } });
const use = (nativeReachable: boolean, terminalReachable: boolean): Record<string, unknown> => ({ source: ownerSource, line: 30, owner: ownerId, member: "render", memberKind: "terminal", nativeReachable, terminalReachable });
const ownerProof = () => ({
  id: ownerId, chain: [], construction: { source: ownerSource, line: 4, expression: "new Part()" }, hops: [], uses: [use(false, true)],
  nativeHooks: [{ name: "describe", source: ownerSource, line: 10, endLine: 20 }],
});
const partEntry = (proof: unknown, status = "terminal-part"): InventoryEntry => ({
  id: `component:${partIdentity}`, category: "component", source: partSource, line: 1,
  details: { ...baseDetails, name: "Part", nativeStatus: status, nativeAbsence: proof, hooks: [hook("render", partSource, 5)] },
});
const partProof = () => ({ class: "terminal-part", owners: [ownerProof()] });

let counter = 0;
function visit(overrides: Partial<ProbeVisit> & Pick<ProbeVisit, "method" | "source" | "line" | "adapter">): ProbeVisit {
  return { type: "visit", pid: 1, generation: "g", sequence: ++counter, component: partIdentity, ...overrides };
}
const partRender = () => visit({ method: "render", source: partSource, line: 5, adapter: "terminal" });
const ownerDescribe = () => visit({ method: "describe", source: ownerSource, line: 10, adapter: "browser", component: `${ownerSource}#Owner` });
const ownerReturn = (line = 15, result: unknown = "value") => visit({ method: "return:describe", source: ownerSource, line, adapter: "browser", component: `${ownerSource}#Owner`, arguments: [{ result }] });
const executed = () => [partRender(), ownerDescribe(), ownerReturn()];
const reasons = (entry: InventoryEntry, ...scenarios: ProbeVisit[][]) => provenanceReasons(entry, scenarios.flat(), new Set(), scenarios);

describe("terminal-part absence class", () => {
  test("is accepted with a located proof and one scenario executing the part's render and its owner's non-null describe", () => {
    expect(reasons(partEntry(partProof()), executed())).toEqual([]);
  });

  test("is rejected when no scenario executes the owner's native hook", () => {
    expect(reasons(partEntry(partProof()), [partRender()]).join("\n")).toContain("No scenario executes both");
  });

  test("is rejected when the part's own render never ran on the terminal", () => {
    const entry = partEntry(partProof());
    expect(reasons(entry, [ownerDescribe(), ownerReturn()]).join("\n")).toContain("Unexercised production hook render");
  });

  test("is rejected when the owner's describe returned null", () => {
    expect(reasons(partEntry(partProof()), [partRender(), ownerDescribe(), ownerReturn(15, "null")]).join("\n")).toContain("No scenario executes both");
  });

  test("is rejected when the descriptor return is outside the owner hook span", () => {
    expect(reasons(partEntry(partProof()), [partRender(), ownerDescribe(), ownerReturn(40)]).join("\n")).toContain("No scenario executes both");
  });

  test("is rejected when the evidence is split across scenarios", () => {
    expect(reasons(partEntry(partProof()), [partRender()], [ownerDescribe(), ownerReturn()]).join("\n")).toContain("No scenario executes both");
  });

  test("is rejected when the describe ran on another component's receiver", () => {
    const other = [partRender(), { ...ownerDescribe(), component: "packages/tui/src/chat/other.ts#Other" }, { ...ownerReturn(), component: "packages/tui/src/chat/other.ts#Other" }];
    expect(reasons(partEntry(partProof()), other).join("\n")).toContain("No scenario executes both");
  });

  test("is rejected when a part use is reachable from a native hook", () => {
    const proof = partProof();
    proof.owners[0]!.uses = [use(true, true)];
    expect(reasons(partEntry(proof), executed()).join("\n")).toContain("Malformed terminal-part owner proof");
  });

  test("accepts a native-reachable use only when it carries a hook-inert justification", () => {
    const inert = partProof();
    inert.owners[0]!.uses = [{ ...use(true, true), hookInert: "stored in a field the hooks never call" }];
    expect(reasons(partEntry(inert), executed())).toEqual([]);
    const blank = partProof();
    blank.owners[0]!.uses = [{ ...use(true, true), hookInert: "" }];
    expect(reasons(partEntry(blank), executed()).join("\n")).toContain("Malformed terminal-part owner proof");
  });

  test("is rejected without a use that reaches a terminal hook", () => {
    const proof = partProof();
    proof.owners[0]!.uses = [use(false, false)];
    expect(reasons(partEntry(proof), executed()).join("\n")).toContain("terminal-hook reach");
  });

  test("is rejected when a proof location is missing or malformed", () => {
    const noConstruction = partProof();
    (noConstruction.owners[0] as Record<string, unknown>).construction = { source: ownerSource };
    expect(reasons(partEntry(noConstruction), executed()).join("\n")).toContain("construction location");
    const noSpan = partProof();
    noSpan.owners[0]!.nativeHooks = [{ name: "describe", source: ownerSource, line: 10 } as never];
    expect(reasons(partEntry(noSpan), executed()).join("\n")).toContain("owner native hook locations");
    const badLine = partProof();
    badLine.owners[0]!.uses = [{ ...use(false, true), line: 0 }];
    expect(reasons(partEntry(badLine), executed()).join("\n")).toContain("part-use locations");
  });

  test("is rejected with no owner, with itself as owner, or with a non-describe owner hook", () => {
    expect(reasons(partEntry({ class: "terminal-part", owners: [] }), executed()).join("\n")).toContain("lists no owner");
    const self = partProof();
    self.owners[0]!.id = `component:${partIdentity}`;
    expect(reasons(partEntry(self), executed()).join("\n")).toContain("owner identity");
    const render = partProof();
    render.owners[0]!.nativeHooks = [{ name: "render", source: ownerSource, line: 10, endLine: 20 }];
    expect(reasons(partEntry(render), executed()).join("\n")).toContain("owner native hook locations");
  });

  test("a status without its proof, or a proof of another class, is an unsupported source state", () => {
    expect(reasons(partEntry(undefined), executed())).toContain("Unsupported native source state: terminal-part");
    expect(reasons(partEntry({ ...partProof(), class: "composer-style" }), executed())).toContain("Unsupported native source state: terminal-part");
  });

  test("an unclassified absent hook keeps failing even when everything executes", () => {
    expect(reasons(partEntry(partProof(), "native-hook-absent"), executed())).toContain("Unsupported native source state: native-hook-absent");
  });
});

const chromeCall = (hookName: string) => ({ hook: hookName, source: chromeSource, line: 60 });
const styleEntry = (proof: unknown): InventoryEntry => ({
  id: `component:${styleIdentity}`, category: "component", source: styleSource, line: 1,
  details: { ...baseDetails, name: "BandStyle", nativeStatus: "composer-style", nativeAbsence: proof, hooks: [hook("renderTop", styleSource, 27), hook("renderBottom", styleSource, 40)] },
});
const styleProof = () => ({ class: "composer-style", chrome: [chromeCall("renderTop"), chromeCall("renderBottom")] });
const styleVisit = (method: string, line: number, adapter: "terminal" | "browser") => visit({ method, source: styleSource, line, adapter, component: styleIdentity });
const chromeVisit = (method: string) => visit({ method, source: chromeSource, line: 59, adapter: "browser", component: "chrome" });
const rendered = () => [styleVisit("renderTop", 27, "terminal"), styleVisit("renderBottom", 40, "terminal")];
const nativeChrome = () => [chromeVisit("describeComposerChrome"), styleVisit("renderTop", 27, "browser"), styleVisit("renderBottom", 40, "browser"), chromeVisit("return:describeComposerChrome")];

describe("composer-style absence class", () => {
  test("is accepted when every hook ran on the terminal and inside a browser chrome window", () => {
    expect(reasons(styleEntry(styleProof()), [...rendered(), ...nativeChrome()])).toEqual([]);
  });

  test("is rejected when the browser calls happen outside any chrome window", () => {
    const outside = [...rendered(), styleVisit("renderTop", 27, "browser"), styleVisit("renderBottom", 40, "browser")];
    expect(reasons(styleEntry(styleProof()), outside).join("\n")).toContain("inside native chrome");
  });

  test("is rejected when a hook ran on the terminal only or on the browser only", () => {
    expect(reasons(styleEntry(styleProof()), nativeChrome()).join("\n")).toContain("Unexercised production hook renderTop");
    const partial = [...rendered(), chromeVisit("describeComposerChrome"), styleVisit("renderTop", 27, "browser"), chromeVisit("return:describeComposerChrome")];
    expect(reasons(styleEntry(styleProof()), partial).join("\n")).toContain("inside native chrome");
  });

  test("is rejected when the evidence is split across scenarios", () => {
    expect(reasons(styleEntry(styleProof()), rendered(), nativeChrome()).join("\n")).toContain("inside native chrome");
  });

  test("is rejected when the proof lacks a chrome call site for a hook or names another file", () => {
    const missing = { class: "composer-style", chrome: [chromeCall("renderTop")] };
    expect(reasons(styleEntry(missing), [...rendered(), ...nativeChrome()]).join("\n")).toContain("no native chrome call site for renderBottom");
    const elsewhere = { class: "composer-style", chrome: [{ ...chromeCall("renderTop"), source: "packages/tui/src/other.ts" }, chromeCall("renderBottom")] };
    expect(reasons(styleEntry(elsewhere), [...rendered(), ...nativeChrome()]).join("\n")).toContain("Malformed composer-style proof");
    const unlocated = { class: "composer-style", chrome: [{ hook: "renderTop", source: chromeSource }, chromeCall("renderBottom")] };
    expect(reasons(styleEntry(unlocated), [...rendered(), ...nativeChrome()]).join("\n")).toContain("Malformed composer-style proof");
  });
});

const fixtureSource = "packages/coding-agent/src/cli/gallery-fixtures/composer.ts";
const launcher = (role: string, text: string) => ({ repository: "roost", role, text, source: "crates/launcher.rs", line: 3 });
const cliProof = () => ({
  class: "cli-unreachable",
  edges: [{ importer: "packages/coding-agent/src/cli/gallery-fixtures/index.ts", imported: fixtureSource, dynamic: false, source: "packages/coding-agent/src/cli/gallery-fixtures/index.ts", line: 29 }],
  registration: { source: "packages/coding-agent/src/cli-commands.ts", line: 132, command: "gallery", module: "packages/coding-agent/src/commands/gallery.ts" },
  launchers: [launcher("roost-worker", '.args(["acp"])'), launcher("parity-terminal-leg", "command: [launcher, ...args]")],
});
const cliEntry = (proof: unknown): InventoryEntry => ({
  id: `component:${fixtureSource}#fixture`, category: "component", source: fixtureSource, line: 1,
  details: { ...baseDetails, name: "fixture", nativeStatus: "cli-unreachable", nativeAbsence: proof, hooks: [hook("renderTop", fixtureSource, 9)] },
});

describe("cli-unreachable absence class", () => {
  test("is accepted from its located proof without any browser execution", () => {
    expect(reasons(cliEntry(cliProof()), [])).toEqual([]);
  });

  test("is rejected when a launcher role or the fixture's import edge is missing", () => {
    const noParity = cliProof();
    noParity.launchers = [noParity.launchers[0]!];
    expect(reasons(cliEntry(noParity), []).join("\n")).toContain("launcher argv locations");
    const noEdge = cliProof();
    noEdge.edges = [{ ...noEdge.edges[0]!, imported: "packages/coding-agent/src/cli/gallery-fixtures/fs.ts" }];
    expect(reasons(cliEntry(noEdge), []).join("\n")).toContain("import chain from the fixture module");
  });

  test("is rejected when a launcher passes the registered subcommand", () => {
    const reachable = cliProof();
    reachable.launchers = [launcher("roost-worker", '.args(["gallery"])'), launcher("parity-terminal-leg", "command: [launcher]")];
    expect(reasons(cliEntry(reachable), []).join("\n")).toContain("launcher that passes the registered subcommand");
  });

  test("is rejected without a registration location or without a proof", () => {
    const noRegistration = cliProof();
    (noRegistration as Record<string, unknown>).registration = { command: "gallery", module: "x.ts" };
    expect(reasons(cliEntry(noRegistration), []).join("\n")).toContain("command-table registration");
    expect(reasons(cliEntry(undefined), [])).toContain("Unsupported native source state: cli-unreachable");
  });
});

const backendSource = "packages/tui/src/native/backend.ts";
const backendIdentity = `${backendSource}#NativeBackend`;
const hostCall = (hookName: string, line: number) => ({ source: "packages/tui/src/tui.ts", line, expression: `this.#native!.${hookName}()`, hook: hookName, guard: { source: "packages/tui/src/tui.ts", line: line - 1, expression: "this.#nativeLive" } });
const transportProof = () => ({
  class: "native-transport",
  component: { source: backendSource, line: 305, name: "NativeBackend", renderReturns: "void", contract: { source: "packages/tui/src/tui.ts", line: 253 } },
  construction: { source: "packages/tui/src/tui.ts", line: 1807, expression: "new NativeBackend(host, hello)", holder: "#native", helloParameter: "hello", method: "#startNative" },
  callers: [{ source: "packages/tui/src/tui.ts", line: 1796, expression: "this.#startNative(hello)" }],
  hookCalls: [hostCall("render", 3955), hostCall("handleInput", 2989)],
});
const transportEntry = (proof: unknown): InventoryEntry => ({
  id: `component:${backendIdentity}`, category: "component", source: backendSource, line: 305,
  details: { ...baseDetails, name: "NativeBackend", interactive: true, nativeStatus: "native-transport", nativeAbsence: proof, hooks: [hook("render", backendSource, 475), hook("handleInput", backendSource, 546)] },
});
const backendVisit = (method: string, line: number, adapter: "terminal" | "browser") => visit({ method, source: backendSource, line, adapter, component: backendIdentity });

describe("native-transport absence class", () => {
  test("is accepted when the browser leg executes every hook and no terminal execution exists", () => {
    expect(reasons(transportEntry(transportProof()), [backendVisit("render", 475, "browser"), backendVisit("handleInput", 546, "browser")])).toEqual([]);
  });

  test("requires the browser leg's hook execution, and a terminal visit does not substitute", () => {
    expect(reasons(transportEntry(transportProof()), [backendVisit("render", 475, "browser")]).join("\n")).toContain("every transport hook on the browser leg");
    expect(reasons(transportEntry(transportProof()), [backendVisit("render", 475, "terminal"), backendVisit("handleInput", 546, "terminal")]).join("\n")).toContain("every transport hook on the browser leg");
  });

  test("is rejected for a forged structure: unguarded or missing host call, non-void render, construction inside the class", () => {
    const missingCall = transportProof();
    missingCall.hookCalls = [missingCall.hookCalls[0]!];
    expect(reasons(transportEntry(missingCall), []).join("\n")).toContain("a host call site for every hook");
    const noGuard = transportProof();
    (noGuard.hookCalls[0] as Record<string, unknown>).guard = {};
    expect(reasons(transportEntry(noGuard), []).join("\n")).toContain("guarded hook-call locations");
    const rendersRows = transportProof();
    rendersRows.component.renderReturns = "string[]";
    expect(reasons(transportEntry(rendersRows), []).join("\n")).toContain("render must return void");
    const selfConstructed = transportProof();
    selfConstructed.construction.source = backendSource;
    expect(reasons(transportEntry(selfConstructed), []).join("\n")).toContain("construction outside the class's own module");
    expect(reasons(transportEntry(undefined), [])).toContain("Unsupported native source state: native-transport");
  });
});

describe("native-held-part absence class", () => {
  const heldUse = (hookInert?: string): Record<string, unknown> => ({ source: ownerSource, line: 12, owner: ownerId, member: "describe", memberKind: "native", nativeReachable: true, terminalReachable: false, ...hookInert ? { hookInert } : {} });
  const heldProof = (uses: unknown[] = [heldUse()]) => ({ class: "native-held-part", owners: [{ ...ownerProof(), uses }] });
  const heldEntry = (proof: unknown) => partEntry(proof, "native-held-part");
  const partRenderNatively = () => visit({ method: "render", source: partSource, line: 5, adapter: "browser" });

  test("is accepted when one scenario runs the part's render on both legs and its owner's non-null describe", () => {
    expect(reasons(heldEntry(heldProof()), [...executed(), partRenderNatively()])).toEqual([]);
  });

  test("is rejected when the part's render never ran on the browser leg", () => {
    expect(reasons(heldEntry(heldProof()), executed()).join("\n")).toContain("on both legs");
  });

  test("is rejected when the browser-leg render and the owner's describe are in different scenarios", () => {
    expect(reasons(heldEntry(heldProof()), executed(), [partRenderNatively()]).join("\n")).toContain("on both legs");
  });

  test("is rejected when no use of the part is reached by a native hook", () => {
    expect(reasons(heldEntry(heldProof([use(false, true)])), [...executed(), partRenderNatively()]).join("\n")).toContain("no owner's native hook holds the part");
    const heldOwner = { ...ownerProof(), uses: [heldUse()] };
    const terminalOnlyOwner = { ...ownerProof(), id: "component:packages/tui/src/other.ts#Other", uses: [use(false, true)] };
    expect(reasons(heldEntry({ class: "native-held-part", owners: [heldOwner, terminalOnlyOwner] }), [...executed(), partRenderNatively()])).toEqual([]);
  });

  test("a terminal-part proof still refuses a native-reachable use that is not hook-inert", () => {
    expect(reasons(partEntry({ class: "terminal-part", owners: [{ ...ownerProof(), uses: [heldUse()] }] }), [...executed(), partRenderNatively()]).join("\n")).toContain("Malformed terminal-part owner proof");
  });
});

describe("input-listener absence class", () => {
  const listenerId = "packages/tui/src/chat/listener.ts#Listener";
  const listenerSource = "packages/tui/src/chat/listener.ts";
  const claim = { name: "handleInput", source: listenerSource, line: 8 };
  const listenerEntry = (hooks: unknown[] = [claim]): InventoryEntry => ({
    id: `component:${listenerId}`, category: "component", source: listenerSource, line: 1,
    details: { ...baseDetails, name: "Listener", interactive: true, nativeStatus: "input-listener", nativeAbsence: { class: "input-listener", hooks },
      hooks: [{ name: "handleInput", inherited: false, referenced: true, implementation: { source: listenerSource, line: 8 } }] },
  });
  const input = (adapter: ProbeVisit["adapter"]) => visit({ method: "handleInput", source: listenerSource, line: 8, adapter, component: listenerId });

  test("is accepted when one scenario delivers input to the hook on both legs", () => {
    expect(reasons(listenerEntry(), [input("terminal"), input("browser")])).toEqual([]);
  });

  test("is rejected when the browser leg never delivered input", () => {
    expect(reasons(listenerEntry(), [input("terminal")]).join("\n")).toContain("both the terminal leg and the browser leg");
  });

  test("is rejected when the legs ran in different scenarios", () => {
    expect(reasons(listenerEntry(), [input("terminal")], [input("browser")]).join("\n")).toContain("both the terminal leg and the browser leg");
  });

  test("is rejected when the proof does not name exactly the component's hooks", () => {
    expect(reasons(listenerEntry([]), [input("terminal"), input("browser")]).join("\n")).toContain("Malformed input-listener proof");
    expect(reasons(listenerEntry([{ ...claim, line: 9 }]), [input("terminal"), input("browser")]).join("\n")).toContain("Malformed input-listener proof");
  });
});

describe("abstract base hook demands", () => {
  const baseSource = "packages/tui/src/chrome/base.ts";
  const baseId = `${baseSource}#Base`;
  const leafId = "packages/tui/src/chrome/leaf.ts#Leaf";
  const baseEntry = (subclasses: string[]): InventoryEntry => ({
    id: `component:${baseId}`, category: "component", source: baseSource, line: 1,
    details: { ...baseDetails, name: "Base", abstract: true, concreteSubclasses: subclasses, nativeStatus: "native-hooks", hooks: [{ ...hook("render", baseSource, 6), inherited: true }, { ...hook("describe", baseSource, 9), inherited: true }] },
  });
  const describeRun = (receiverComponent: string) => [visit({ method: "describe", source: baseSource, line: 9, adapter: "browser", component: baseId, receiverComponent }),
    visit({ method: "return:describe", source: baseSource, line: 10, adapter: "browser", component: baseId, receiverComponent, arguments: [{ result: "value" }] })];
  const run = (receiverComponent: string) => visit({ method: "render", source: baseSource, line: 6, adapter: "terminal", component: baseId, receiverComponent });

  test("a concrete subclass running the base's hook satisfies it", () => {
    expect(reasons(baseEntry([`component:${leafId}`]), [run(leafId), ...describeRun(leafId)])).toEqual([]);
  });

  test("an unrelated receiver does not, and a base with no concrete subclass keeps the demand on itself", () => {
    expect(reasons(baseEntry([`component:${leafId}`]), [run("packages/tui/src/chrome/other.ts#Other"), ...describeRun("packages/tui/src/chrome/other.ts#Other")]).join("\n")).toContain("Unexercised production hook render");
    expect(reasons(baseEntry([]), [run(leafId), ...describeRun(leafId)]).join("\n")).toContain("Unexercised production hook render");
  });
});
