import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import ts from "typescript";
import { componentInventory } from "./inventory-components";
import { ComponentGraph } from "./inventory-native-absence";
import { traceValue } from "./inventory-native-flow";
import { recordConcreteSubclasses } from "./inventory-abstract-bases";
import { classifyNativeAbsence } from "./inventory-native-classify";
import { SourceIndex } from "./inventory-source";
import type { InventoryEntry } from "./inventory-types";

const roots: string[] = [];
afterAll(() => { for (const root of roots) rmSync(root, { recursive: true, force: true }); });

function classify(files: Record<string, string>): Map<string, InventoryEntry> {
  const root = mkdtempSync(path.join(tmpdir(), "native-absence-"));
  roots.push(root);
  for (const name of ["tui", "coding-agent", "wire"]) {
    mkdirSync(path.join(root, "packages", name, "src"), { recursive: true });
    writeFileSync(path.join(root, "packages", name, "package.json"), JSON.stringify({ name: `@unit/${name}` }));
    writeFileSync(path.join(root, "packages", name, "src", "index.ts"), "export {};\n");
  }
  writeFileSync(path.join(root, "packages/tui/src/descriptor-globals.ts"), "declare global { type NativeNode = Record<string, unknown>; }\nexport {};\n");
  for (const [relative, text] of Object.entries(files)) writeFileSync(path.join(root, "packages/tui/src", relative), text);
  const source = new SourceIndex(root);
  const entries = componentInventory(source);
  recordConcreteSubclasses(entries);
  classifyNativeAbsence(source, entries);
  return new Map(entries.map(entry => [entry.id.split("#")[1]!, entry]));
}

const part = `export class Part { render(width: number): string[] { return [String(width)]; } }\n`;
const owner = (describeBody: string, renderBody: string) => `import { Part } from "./part";
export class Owner {
  #part = new Part();
  render(width: number): string[] { ${renderBody} }
  describe(): NativeNode | null { ${describeBody} }
}
`;

describe("native absence classification", () => {
  test("a part only a terminal hook reaches is a terminal-part with located proof", () => {
    const entries = classify({ "part.ts": part, "owner.ts": owner("return { kind: 'text' };", "return this.#part.render(width);") });
    const entry = entries.get("Part")!;
    expect(entry.details.nativeStatusInSource).toBe("native-hook-absent");
    expect(entry.details.nativeStatus).toBe("terminal-part");
    const proof = entry.details.nativeAbsence as { owners: { id: string; construction: { source: string; line: number }; uses: { nativeReachable: boolean; terminalReachable: boolean }[]; nativeHooks: { name: string; endLine: number }[] }[] };
    expect(proof.owners).toHaveLength(1);
    expect(proof.owners[0]!.id).toContain("#Owner");
    expect(proof.owners[0]!.construction).toMatchObject({ source: "packages/tui/src/owner.ts" });
    expect(proof.owners[0]!.uses.every(use => !use.nativeReachable)).toBe(true);
    expect(proof.owners[0]!.uses.some(use => use.terminalReachable)).toBe(true);
    expect(proof.owners[0]!.nativeHooks[0]).toMatchObject({ name: "describe" });
  });

  test("a part a native hook reaches through a helper member is a native-held-part, never a terminal-part", () => {
    const entry = classify({
      "part.ts": part,
      "owner.ts": `import { Part } from "./part";
export class Owner {
  #part = new Part();
  #rows(width: number): string[] { return this.#part.render(width); }
  render(width: number): string[] { return this.#rows(width); }
  describe(): NativeNode | null { return { rows: this.#rows(10) }; }
}
`,
    }).get("Part")!;
    expect(entry.details.nativeStatus).toBe("native-held-part");
    const proof = entry.details.nativeAbsence as { owners: { uses: { nativeReachable: boolean; hookInert?: string }[] }[] };
    expect(proof.owners[0]!.uses.some(use => use.nativeReachable && use.hookInert === undefined)).toBe(true);
  });

  test("a native hook that holds the part without constructing it is an owner, so the witness can require its describe", () => {
    const entries = classify({
      "part.ts": part,
      "holder.ts": `import { Part } from "./part";
export class Holder {
  #part: Part | undefined;
  setPart(next: Part): void { this.#part = next; }
  #rows(width: number): string[] { return this.#part?.render(width) ?? []; }
  render(width: number): string[] { return this.#rows(width); }
  describe(): NativeNode | null { return { rows: this.#rows(10) }; }
}
`,
      "maker.ts": `import { Part } from "./part";
import { Holder } from "./holder";
export class Maker {
  constructor(private readonly holder: Holder) {}
  render(width: number): string[] { const created = new Part(); this.holder.setPart(created); return created.render(width); }
}
`,
      "root.ts": `import { Holder } from "./holder";
import { Maker } from "./maker";
export class Root {
  #holder = new Holder();
  #maker = new Maker(this.#holder);
  render(width: number): string[] { return [...this.#maker.render(width), ...this.#holder.render(width)]; }
  describe(): NativeNode | null { return { rows: this.#holder.render(10) }; }
}
`,
    });
    const entry = entries.get("Part")!;
    expect(entry.details.nativeStatus).toBe("native-held-part");
    const proof = entry.details.nativeAbsence as { owners: { id: string; uses: { nativeReachable: boolean; hookInert?: string }[] }[] };
    const holder = proof.owners.find(candidate => candidate.id.endsWith("#Holder"));
    expect(holder?.uses.some(use => use.nativeReachable && use.hookInert === undefined)).toBe(true);
  });

  test("a component with only input hooks is an input-listener, but one that also declares a render hook is not", () => {
    const listener = classify({ "listener.ts": `export class Listener { handleInput(data: string): boolean { return data.length > 0; } }\n` }).get("Listener");
    expect(listener?.details.nativeStatus).toBe("input-listener");
    expect(listener?.details.nativeAbsence).toMatchObject({ class: "input-listener", hooks: [{ name: "handleInput", source: "packages/tui/src/listener.ts" }] });
    const painted = classify({ "painted.ts": `export class Painted { handleInput(data: string): boolean { return data.length > 0; } render(width: number): string[] { return [String(width)]; } }\n` }).get("Painted");
    expect(painted?.details.nativeStatus).not.toBe("input-listener");
  });

  test("an abstract base records every concrete descendant as its receivers, and a concrete class records none", () => {
    const entries = classify({
      "base.ts": `export abstract class Base { render(width: number): string[] { return [String(width)]; } }\nexport abstract class Middle extends Base {}\n`,
      "leaf.ts": `import { Middle } from "./base";\nexport class Leaf extends Middle { describe(): NativeNode | null { return { n: 1 }; } }\nexport class Other { render(width: number): string[] { return [String(width)]; } }\n`,
    });
    expect(entries.get("Base")!.details.abstract).toBe(true);
    expect(entries.get("Base")!.details.concreteSubclasses).toEqual(["component:packages/tui/src/leaf.ts#Leaf"]);
    expect(entries.get("Middle")!.details.concreteSubclasses).toEqual(["component:packages/tui/src/leaf.ts#Leaf"]);
    expect(entries.get("Leaf")!.details.concreteSubclasses).toBeUndefined();
    expect(entries.get("Other")!.details.abstract).toBe(false);
  });

  test("a part constructed where no component owns it stays refused", () => {
    const entry = classify({ "part.ts": part, "free.ts": `import { Part } from "./part";\nexport const loose = new Part();\n` }).get("Part")!;
    expect(entry.details.nativeStatus).toBe("native-hook-absent");
    expect(entry.details.nativeAbsenceRefusal).toBeDefined();
  });

  test("a part handed to a callback parameter whose source function cannot be followed stays refused", () => {
    const entry = classify({
      "part.ts": part,
      "owner.ts": `import { Part } from "./part";
export class Owner {
  #part = new Part();
  render(width: number, sink?: (value: Part) => void): string[] { sink?.(this.#part); return []; }
  describe(): NativeNode | null { return { kind: 'text' }; }
}
`,
      "caller.ts": `import { Owner } from "./owner";\ndeclare const external: (value: unknown) => void;\nnew Owner().render(1, external);\n`,
    }).get("Part")!;
    expect(entry.details.nativeStatus).toBe("native-hook-absent");
    expect(JSON.stringify(entry.details.nativeAbsenceRefusal)).toContain("source function");
  });

  test("an owner without a native hook cannot prove a part terminal-only", () => {
    const entry = classify({
      "part.ts": part,
      "owner.ts": `import { Part } from "./part";\nexport class Owner {\n  #part = new Part();\n  render(width: number): string[] { return this.#part.render(width); }\n}\n`,
    }).get("Part")!;
    expect(entry.details.nativeStatus).toBe("native-hook-absent");
    expect(entry.details.nativeAbsence).toBeUndefined();
  });
});

const componentContract = `export interface Component { render(width: number): string[] }\n`;
const backend = `export interface TspHello { r: string }
export class Backend {
  render(): void {}
  handleInput(sequence: string): boolean { return sequence.length > 0; }
}
`;
const host = (startBody: string, renderBody: string) => `import type { Component, TspHello } from "./tui";
import { Backend } from "./backend";
export class Host implements Component {
  #native: Backend | undefined;
  #live = false;
  startNative(hello: TspHello): void { this.#startNative(hello); }
  #startNative(hello: TspHello): void { ${startBody} }
  render(width: number): string[] { ${renderBody} return [String(width)]; }
  handleInput(data: string): void { if (this.#live) this.#native!.handleInput(data); }
}
`;
const transportFiles = (startBody: string, renderBody: string) => ({ "tui.ts": `${componentContract}export type TspHello = { r: string };\n`, "backend.ts": backend.replace("export interface TspHello { r: string }\n", 'import type { TspHello } from "./tui";\n').replace("class Backend {", "class Backend {\n  constructor(readonly host: object, readonly hello: TspHello) {}"), "host.ts": host(startBody, renderBody) });
const guarded = "if (this.#live) this.#native!.render();";

describe("native transport classification", () => {
  test("a backend built only from a negotiated hello and called only under the live flag is a native-transport", () => {
    const entry = classify(transportFiles("this.#native = new Backend(this, hello); this.#live = true;", guarded)).get("Backend")!;
    expect(JSON.stringify(entry.details.nativeAbsenceRefusal ?? entry.details.nativeStatus)).toBe('"native-transport"');
    const proof = entry.details.nativeAbsence as { construction: { holder: string; method: string }; callers: unknown[]; hookCalls: { hook: string; guard: { expression: string } }[] };
    expect(proof.construction).toMatchObject({ holder: "#native", method: "#startNative" });
    expect(proof.callers).toHaveLength(1);
    expect(proof.hookCalls.map(call => call.hook).sort()).toEqual(["handleInput", "render"]);
    expect(proof.hookCalls.every(call => call.guard.expression.includes("#live"))).toBe(true);
  });

  test("an unguarded hook call is refused", () => {
    expect(classify(transportFiles("this.#native = new Backend(this, hello); this.#live = true;", "this.#native!.render();")).get("Backend")!.details.nativeStatus).toBe("native-hook-absent");
  });

  test("a second construction outside the negotiated method is refused", () => {
    const files = transportFiles("this.#native = new Backend(this, hello); this.#live = true;", guarded);
    files["host.ts"] = files["host.ts"]!.replace("render(width: number): string[] {", "render(width: number): string[] { this.#native = new Backend(this, { r: 'x' });");
    expect(classify(files).get("Backend")!.details.nativeStatus).toBe("native-hook-absent");
  });

  test("a field handed to other code is refused", () => {
    const files = transportFiles("this.#native = new Backend(this, hello); this.#live = true;", guarded);
    files["host.ts"] = files["host.ts"]!.replace("render(width: number): string[] {", "render(width: number): string[] { globalThis.leaked = this.#native;");
    expect(classify(files).get("Backend")!.details.nativeStatus).toBe("native-hook-absent");
  });

  test("a class that satisfies Component is never a transport", () => {
    const files = transportFiles("this.#native = new Backend(this, hello); this.#live = true;", guarded);
    files["backend.ts"] = files["backend.ts"]!.replace("render(): void {}", "render(width: number): string[] { return [String(width)]; }");
    expect(classify(files).get("Backend")!.details.nativeStatus).toBe("native-hook-absent");
  });
});

describe("resolution precision", () => {
  const ownerOf = (body: string) => `import { Part } from "./part";\n${body}\n`;
  test("a callback stored through a promise resolver still reaches the part", () => {
    const entry = classify({ "part.ts": part, "owner.ts": ownerOf(`export class Owner {
  #part = new Part();
  render(width: number): string[] { return this.#part.render(width); }
  describe(): NativeNode | null { return { n: 1 }; }
}`) }).get("Part")!;
    expect(entry.details.nativeStatus).toBe("terminal-part");
  });

  test("array-rest, spread and recursion do not hide a native reach", () => {
    const entry = classify({ "part.ts": part, "owner.ts": ownerOf(`function grab(...rest: Part[]): Part | undefined { return rest[0]; }
function deep(depth: number, ...items: Part[]): Part | undefined { return depth > 0 ? deep(depth - 1, ...items) : grab(...items); }
export class Owner {
  #part = new Part();
  render(width: number): string[] { return deep(3, this.#part)!.render(width); }
  describe(): NativeNode | null { return { rows: deep(3, this.#part)!.render(10) }; }
}`) }).get("Part")!;
    expect(entry.details.nativeStatus).not.toBe("terminal-part");
  });

  test("an object-literal member is entered through its member calls, not the enclosing function", () => {
    const entry = classify({ "part.ts": part, "owner.ts": ownerOf(`function build(part: Part) { return { show(width: number) { return part.render(width); } }; }
export class Owner {
  #part = new Part();
  #view = build(this.#part);
  render(width: number): string[] { return this.#view.show(width); }
  describe(): NativeNode | null { return { n: 1 }; }
}`) }).get("Part")!;
    expect(entry.details.nativeStatus).toBe("terminal-part");
  });
});

describe("flow engine precision", () => {
  const ownerOf = (body: string) => `import { Part } from "./part";\n${body}\n`;
  const status = (body: string) => classify({ "part.ts": part, "owner.ts": ownerOf(body) }).get("Part")!.details.nativeStatus;

  test("a shorthand-stored callback is followed to its callers, and a native caller refuses the proof", () => {
    const body = (describeBody: string) => `function show(part: Part, width: number): string[] { return part.render(width); }
export class Owner {
  #part = new Part();
  #config = { show };
  render(width: number): string[] { return this.#config.show(this.#part, width); }
  describe(): NativeNode | null { ${describeBody} }
}`;
    expect(status(body("return { n: 1 };"))).toBe("terminal-part");
    expect(status(body("return { rows: this.#config.show(this.#part, 10) };"))).not.toBe("terminal-part");
  });

  test("a conditional receiver of a container push still holds the part for a native reader", () => {
    const body = (describeBody: string) => `export class Owner {
  #left: Part[] = [];
  #right: Part[] = [];
  constructor(flag: boolean) { const part = new Part(); (flag ? this.#left : this.#right).push(part); }
  render(width: number): string[] { return [...this.#left, ...this.#right].flatMap(part => part.render(width)); }
  describe(): NativeNode | null { ${describeBody} }
}`;
    expect(status(body("return { n: 1 };"))).toBe("terminal-part");
    expect(status(body("return { rows: this.#left.concat(this.#right).map(part => part.render(10)) };"))).not.toBe("terminal-part");
  });

  test("a callback stored two levels deep is read back only through the keys that hold it", () => {
    const body = (describeBody: string) => `function makePart(): Part { return new Part(); }
const renderers: Record<string, any> = { box: { renderResult: makePart, animated: (count: number) => count > 0 } };
export class Owner {
  render(width: number): string[] {
    const renderer = renderers[String(width)];
    const animated = renderer.animated;
    const spinning = typeof animated === "function" ? animated(width) : animated === true;
    return spinning ? [] : renderer.renderResult().render(width);
  }
  describe(): NativeNode | null { ${describeBody} }
}`;
    expect(status(body("return { n: 1 };"))).toBe("terminal-part");
    expect(status(body("return { rows: renderers.box.renderResult().render(10) };"))).not.toBe("terminal-part");
    expect(status(body("return { rows: renderers.box.animated(1) ? makePart().render(10) : [] };"))).not.toBe("terminal-part");
  });

  test("a callback passed through a spread reaches only the parameters its type can hold", () => {
    const callsOf = (themeType: string) => {
      const root = mkdtempSync(path.join(tmpdir(), "native-spread-"));
      roots.push(root);
      mkdirSync(path.join(root, "packages/tui/src"), { recursive: true });
      writeFileSync(path.join(root, "packages/tui/package.json"), JSON.stringify({ name: "@unit/tui" }));
      writeFileSync(path.join(root, "packages/tui/src/index.ts"), `class Theme { fg(text: string): string { return text; } }
type Factory = (theme: ${themeType}, done: (result: string) => void) => void;
const finish = (result: string): void => { void result; };
function open(factory: Factory): void {
  const wrap = (...args: Parameters<Factory>) => factory(...args);
  wrap(new Theme(), finish);
}
open((theme, done) => { theme.fg("x"); done("y"); });
`);
      const source = new SourceIndex(root);
      const graph = new ComponentGraph(source, []);
      const reference = graph.named("finish").find(identifier => ts.isCallExpression(identifier.parent));
      const trace = traceValue(graph, reference!, "function");
      if ("rejected" in trace) throw new Error(trace.rejected);
      return trace.calls.map(call => call.getText());
    };
    expect(callsOf("Theme")).toEqual(['done("y")']);
    expect(callsOf("any").sort()).toEqual(['done("y")', 'theme.fg("x")']);
  });

  test("a function written through a destructuring assignment reaches only the targets its key names", () => {
    const callsOf = (source: string) => {
      const root = mkdtempSync(path.join(tmpdir(), "native-destructure-"));
      roots.push(root);
      mkdirSync(path.join(root, "packages/tui/src"), { recursive: true });
      writeFileSync(path.join(root, "packages/tui/package.json"), JSON.stringify({ name: "@unit/tui" }));
      writeFileSync(path.join(root, "packages/tui/src/index.ts"), source);
      const graph = new ComponentGraph(new SourceIndex(root), []);
      const reference = graph.named("finish").find(identifier => !ts.isVariableDeclaration(identifier.parent));
      const trace = traceValue(graph, reference!, "function");
      if ("rejected" in trace) throw new Error(trace.rejected);
      return trace.calls.map(call => call.getText());
    };
    const declarations = `let chosen: (() => void) | undefined;\nlet other: (() => void) | undefined;\nconst finish = (): void => undefined;\n`;
    expect(callsOf(`${declarations}({ keep: chosen, drop: other } = { keep: finish, drop: undefined });\nchosen?.();\nother?.();\n`)).toEqual(["chosen?.()"]);
    expect(callsOf(`${declarations}[chosen] = [finish];\nchosen?.();\nother?.();\n`)).toEqual(["chosen?.()"]);
  });

  test("an element callback receives the traced value, so a use inside it is found", () => {
    const usesOf = (source: string) => {
      const root = mkdtempSync(path.join(tmpdir(), "native-element-callback-"));
      roots.push(root);
      mkdirSync(path.join(root, "packages/tui/src"), { recursive: true });
      writeFileSync(path.join(root, "packages/tui/package.json"), JSON.stringify({ name: "@unit/tui" }));
      writeFileSync(path.join(root, "packages/tui/src/index.ts"), source);
      const graph = new ComponentGraph(new SourceIndex(root), []);
      const construction = graph.named("Figure").find(identifier => ts.isNewExpression(identifier.parent))!.parent;
      const trace = traceValue(graph, construction);
      return "rejected" in trace ? trace.rejected : trace.values.map(node => node.getText());
    };
    const head = `class Figure { draw(): string { return "x"; } }\nconst parts: Figure[] = [];\nparts.push(new Figure());\n`;
    expect(usesOf(`${head}parts.forEach(figure => figure.draw());\n`)).toContain("figure.draw");
    expect(usesOf(`${head}parts.reduce((total, figure) => total + figure.draw(), "");\n`)).toContain("figure.draw");
    expect(usesOf(`${head}parts.reduce((total, figure) => total + figure.draw(), "");\n`)).not.toContain("total.draw");
    expect(usesOf(`${head}const handlers: Array<(figure: Figure) => void> = [];\nparts.forEach(handlers[0]!);\n`)).toMatch(/no enumerable source function/);
  });

  test("a call through a slot only primitive values were written to runs no function, but a written function receives the part", () => {
    const body = (written: string, describeBody: string) => `interface Spec { animated?: boolean | ((arg: unknown) => boolean) }
const spec: Spec = { animated: ${written} };
export class Owner {
  #part = new Part();
  render(width: number): string[] {
    const slot = spec.animated;
    const spinning = typeof slot === "function" ? slot(this.#part) : slot === true;
    return spinning ? [] : this.#part.render(width);
  }
  describe(): NativeNode | null { ${describeBody} }
}`;
    expect(status(body("true", "return { n: 1 };"))).toBe("terminal-part");
    expect(status(body("(arg: unknown) => (arg as Part).render(10).length > 0", "return { n: 1 };"))).toBe("terminal-part");
    expect(status(body("(arg: unknown) => (arg as Part).render(10).length > 0", "const slot = spec.animated; return { n: typeof slot === 'function' && slot(this.#part) ? 1 : 0 };"))).not.toBe("terminal-part");
  });

  test("text rendered by the part is data, but the part parked in an any-typed slot is still the part", () => {
    expect(status(`export class Owner {
  #part = new Part();
  #text = "";
  render(width: number): string[] { this.#text = this.#part.render(width).join(""); return [this.#text]; }
  describe(): NativeNode | null { return { text: this.#text }; }
}`)).toBe("terminal-part");
    expect(status(`export class Owner {
  #part = new Part();
  #held: any = this.#part;
  render(width: number): string[] { return this.#part.render(width); }
  describe(): NativeNode | null { return { rows: this.#held.render(10) }; }
}`)).not.toBe("terminal-part");
  });
});

describe("terminal owners of components without a terminal hook", () => {
  const native = `export type NativeNode = { k: string };\n`;
  const sheet = (readsFrom: string) => ({
    "native.ts": native,
    "list.ts": `export class List {
  #items: string[] = [];
  count(): number { return this.#items.length; }
  render(width: number): string[] { return ${readsFrom}; }
}\n`,
    "sheet.ts": `import { List } from "./list";\nimport type { NativeNode } from "./native";
export class Sheet {
  list = new List();
  describe(): NativeNode { return { k: String(this.list.count()) }; }
}\n`,
  });

  test("a terminal render that reads the state the native hook reads carries it", () => {
    const sheetEntry = classify(sheet("this.#items.map(item => item.slice(0, width))")).get("Sheet")!;
    const owners = sheetEntry.details.terminalOwners as { owner: string; hook: string; expression: string; source: string; line: number }[];
    expect(owners).toHaveLength(1);
    expect(owners[0]).toMatchObject({ hook: "render", expression: "this.#items", source: "packages/tui/src/list.ts" });
    expect(owners[0]!.owner).toContain("#List");
  });

  test("a terminal render that never touches the shared state carries nothing", () => {
    expect(classify(sheet("[String(width)]")).get("Sheet")!.details.terminalOwners).toEqual([]);
  });

  test("a literal merged onto a factory product is carried by the product's render calling its supplied renderer", () => {
    const entries = classify({
      "native.ts": native,
      "factory.ts": `import type { NativeNode } from "./native";
function cached(compute: (width: number) => string[]) { return { render(width: number): string[] { return compute(width); } }; }
export function card(node: NativeNode) {
  const component = cached(width => [String(width)]);
  return Object.assign(component, { describe: () => node });
}\n`,
    });
    const owners = [...entries.values()].flatMap(entry => entry.details.terminalOwners as { expression: string }[]);
    expect(owners.map(use => use.expression)).toEqual(["compute"]);
  });

  test("a describe-named data query on a non-Component is not a native hook", () => {
    const entries = classify({
      "native.ts": native,
      "router.ts": `export class Router { describe(host: string): string[] { return [host]; } }\n`,
      "sheet.ts": `import type { NativeNode } from "./native";\nexport class Described { describe(): NativeNode { return { k: "x" }; } }\n`,
    });
    expect(entries.has("Router")).toBe(false);
    expect(entries.has("Described")).toBe(true);
  });
});
