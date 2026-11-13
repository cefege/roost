import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { classifyUnproduced } from "./inventory-unproduced";
import { SourceIndex } from "./inventory-source";
import type { InventoryEntry } from "./inventory-types";

const root = mkdtempSync(path.join(tmpdir(), "parity-exposure-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

function sourceTree(producer: string, wireExtra = ""): SourceIndex {
	const tree = mkdtempSync(path.join(root, "tree-"));
	for (const name of ["wire", "tui", "coding-agent"]) mkdirSync(path.join(tree, "packages", name, "src"), { recursive: true });
	writeFileSync(path.join(tree, "packages/wire/src/tsp.ts"), "export interface TspPagesProps { pages: { id: string; changed?: number; disabled?: string }[]; secret?: boolean; marks?: { tone?: \"accent\" | \"warning\" | \"error\" }[]; mode?: \"keys\" | \"text\" }\n" + wireExtra);
	writeFileSync(path.join(tree, "packages/tui/src/pages.ts"), `import type { TspPagesProps } from "../../wire/src/tsp";\n${producer}\n`);
	writeFileSync(path.join(tree, "packages/coding-agent/src/index.ts"), "export const unrelated = { changed: 1 };\n");
	return new SourceIndex(tree);
}

const member = (key: string): InventoryEntry => ({
	id: `property:TspPagesProps.pages[].${key}`,
	category: "property",
	source: "packages/wire/src/tsp.ts",
	line: 1,
	details: { declaringType: "TspPagesProps", path: `pages[].${key}`, exposure: "ui", declarations: [{ source: "packages/wire/src/tsp.ts", line: 1 }], uiEmitters: [{ source: "packages/tui/src/pages.ts", line: 2, inheritedFrom: "property:TspPagesProps.pages" }] },
});

function classify(producer: string, ...keys: string[]): Record<string, unknown> {
	const entries = keys.map(member);
	classifyUnproduced(sourceTree(producer), entries);
	return Object.fromEntries(entries.map(entry => [entry.details.path, entry.details.unproducedProof ? (entry.details.unproducedProof as { proof: string }).proof : entry.details.exposure]));
}

describe("nested unproduced members", () => {
	test("an element member no typed array element carries is unproduced, one a spread carries is not", () => {
		const producer = `export function pages(tabs: { id: string; muted: boolean }[]): TspPagesProps {
	const list: TspPagesProps["pages"] = tabs.map(tab => {
		const muted = tab.muted ? { disabled: "No matches" } : {};
		return { id: tab.id, ...muted };
	});
	return { pages: list };
}`;
		expect(classify(producer, "changed", "disabled")).toEqual({ "pages[].changed": "every-typed-site-lacks-key", "pages[].disabled": "ui" });
	});

	test("re-spreading an existing element passes it through without producing its members", () => {
		const producer = `export function relabel(props: TspPagesProps): TspPagesProps {
	return { pages: [{ ...props.pages[0]!, id: "x" }] };
}`;
		expect(classify(producer, "changed")).toEqual({ "pages[].changed": "every-typed-site-lacks-key" });
	});

	test("an untyped write keeps the obligation unless a primitive-returning library call consumes it", () => {
		const untyped = `export function pages(store: any): TspPagesProps { store.pages = [{ id: "a" }]; return { pages: [{ id: "a" }] }; }
export const leak = (sink: (value: any) => void) => sink({ changed: 2 });`;
		expect(classify(untyped, "changed")).toEqual({ "pages[].changed": "ui" });
		const serialized = `export function pages(): TspPagesProps { return { pages: [{ id: "a" }] }; }
export const text = (changed: number) => JSON.stringify({ changed });`;
		expect(classify(serialized, "changed")).toEqual({ "pages[].changed": "every-typed-site-lacks-key" });
	});

	test("a literal nested inside a serialized argument is serialized with it; a non-library sink still keeps the obligation", () => {
		const nested = `export function pages(): TspPagesProps { return { pages: [{ id: "a" }] }; }
export const text = (ttl?: number) => JSON.stringify({ id: 1, ...(ttl === undefined ? {} : { changed: ttl }), rows: [{ changed: 3 }] });`;
		expect(classify(nested, "changed")).toEqual({ "pages[].changed": "every-typed-site-lacks-key" });
		const escaping = `export function pages(): TspPagesProps { return { pages: [{ id: "a" }] }; }
export const send = (sink: (value: any) => void, ttl?: number) => sink({ id: 1, ...(ttl === undefined ? {} : { changed: ttl }) });`;
		expect(classify(escaping, "changed")).toEqual({ "pages[].changed": "ui" });
	});
});

function auditValues(producer: string, key: string, enumValues: unknown[], wireExtra = ""): InventoryEntry {
	const entry: InventoryEntry = { id: `property:TspPagesProps.${key}`, category: "property", source: "packages/wire/src/tsp.ts", line: 1,
		details: { declaringType: "TspPagesProps", path: key, exposure: "ui", enumValues, declarations: [{ source: "packages/wire/src/tsp.ts", line: 1 }], uiEmitters: [{ source: "packages/tui/src/pages.ts", line: 2 }] } };
	classifyUnproduced(sourceTree(producer, wireExtra), [entry]);
	return entry;
}

describe("unproduced enum values", () => {
	test("a value every typed site excludes is unproduced; literal and assigned values are produced", () => {
		const producer = `export function pages(on: boolean, props: TspPagesProps): TspPagesProps {
	props.mode = "text";
	return { pages: [], secret: on ? true : undefined, mode: "text" };
}`;
		expect(auditValues(producer, "secret", [false, true]).details.unproducedValues).toMatchObject({ values: [false], proof: "every-typed-site-bounds-value" });
		expect(auditValues(producer, "mode", ["keys", "text"]).details.unproducedValues).toMatchObject({ values: ["keys"] });
	});

	test("a widened, assigned or untyped value keeps every value's obligation", () => {
		const widened = `export const pages = (on: boolean, mode: "keys" | "text"): TspPagesProps => ({ pages: [], secret: on, mode });`;
		expect(auditValues(widened, "secret", [false, true]).details.unproducedValues).toBeUndefined();
		const assigned = `export function pages(props: TspPagesProps): TspPagesProps { props.mode = "keys"; return { pages: [], mode: "text" }; }`;
		expect(auditValues(assigned, "mode", ["keys", "text"]).details.unproducedValues).toBeUndefined();
		const untyped = `export const pages = (sink: (value: any) => void): TspPagesProps => { sink({ mode: "keys" }); return { pages: [], mode: "text" }; };`;
		expect(auditValues(untyped, "mode", ["keys", "text"]).details.unproducedValues).toBeUndefined();
	});
});

describe("shorthand properties in narrowed scope", () => {
	test("a boolean only spread when truthy never produces false; an unconditional shorthand does", () => {
		const guarded = `export function pages(secret: boolean): TspPagesProps { return { pages: [], ...(secret ? { secret } : {}) }; }`;
		expect(auditValues(guarded, "secret", [false, true]).details.unproducedValues).toMatchObject({ values: [false], proof: "every-typed-site-bounds-value" });
		const unconditional = `export function pages(secret: boolean): TspPagesProps { return { pages: [], secret }; }`;
		expect(auditValues(unconditional, "secret", [false, true]).details.unproducedValues).toBeUndefined();
		const alsoUnconditional = `export function pages(secret: boolean): TspPagesProps { return { pages: [], ...(secret ? { secret } : {}), ...{ secret } }; }`;
		expect(auditValues(alsoUnconditional, "secret", [false, true]).details.unproducedValues).toBeUndefined();
	});
});

describe("values passed through a helper's parameter", () => {
	const helper = `export function build(opts?: { secret?: boolean }): TspPagesProps { return { pages: [], ...opts }; }`;
	const secret = (callers: string) => auditValues(`${helper}\n${callers}`, "secret", [false, true]).details.unproducedValues;

	test("a parameter spread into the props is bounded by the literal each call passes", () => {
		expect(secret("export const a = build({ secret: true }); export const b = build();")).toMatchObject({ values: [false], proof: "every-typed-site-bounds-value" });
		expect(secret("export const a = build({ secret: true }); export const b = build({});")).toMatchObject({ values: [false] });
	});

	test("a call that can pass false, or a use other than a plain call, keeps every value demanded", () => {
		expect(secret("export const a = (flag: boolean) => build({ secret: flag });")).toBeUndefined();
		expect(secret("export const a = build({ secret: true }); export const b = build({ secret: false });")).toBeUndefined();
		expect(secret("export const a = build({ secret: true }); export const each = [{ secret: false }].map(build);")).toBeUndefined();
		expect(secret("export const a = build({ secret: true }); export const b = (rest: [{ secret: boolean }]) => build(...rest);")).toBeUndefined();
		expect(secret("export const a = (opts: { secret?: boolean }) => build(opts);")).toBeUndefined();
	});

	test("a scalar parameter narrowed by the helper's own condition produces only what both sides allow", () => {
		const guarded = `export function build(secret: boolean): TspPagesProps { return { pages: [], ...(secret ? { secret } : {}) }; }\nexport const a = build(true); export const b = build(false);`;
		expect(auditValues(guarded, "secret", [false, true]).details.unproducedValues).toMatchObject({ values: [false] });
		const direct = `export function build(secret: boolean): TspPagesProps { return { pages: [], secret }; }\nexport const a = build(true);`;
		expect(auditValues(direct, "secret", [false, true]).details.unproducedValues).toMatchObject({ values: [false] });
		const both = `export function build(secret: boolean): TspPagesProps { return { pages: [], secret }; }\nexport const a = build(true); export const b = build(false);`;
		expect(auditValues(both, "secret", [false, true]).details.unproducedValues).toBeUndefined();
	});
});

describe("props handed back to a props slot", () => {
	const wire = "export interface TspNotesProps { note: string; mode?: string }\nexport type TspAnyProps = TspPagesProps | TspNotesProps;\n";
	const literal = "export const fresh = (): TspPagesProps => ({ pages: [], mode: \"text\" });\n";
	const mode = (producer: string) => auditValues(literal + producer, "mode", ["keys", "text"], wire).details.unproducedValues;

	test("a union of the wire's own kinds re-spread into a literal carries no sibling kind's key", () => {
		expect(mode("import type { TspAnyProps } from \"../../wire/src/tsp\";\nexport const hide = (props: TspAnyProps): TspPagesProps => ({ ...props, pages: [] } as TspPagesProps);")).toMatchObject({ values: ["keys"] });
	});

	test("a union with an ad-hoc member, or an untyped operand, keeps the obligation", () => {
		expect(mode("export const hide = (props: TspPagesProps | { mode: string }): TspPagesProps => ({ ...props, pages: [] } as TspPagesProps);")).toBeUndefined();
		expect(mode("export const hide = (props: any): TspPagesProps => ({ ...props, pages: [] });")).toBeUndefined();
	});
});

describe("write sites outside the props", () => {
	test("an array slot's elements are the sites of their key; a spread array carries none", () => {
		const warned = `export function pages(show: boolean): TspPagesProps {
	const marks = show ? [{ tone: "warning" as const }] : undefined;
	return { pages: [], ...(marks ? { marks } : {}) };
}`;
		expect(auditValues(warned, "marks[].tone", ["accent", "warning", "error"]).details.unproducedValues).toMatchObject({ values: ["accent", "error"] });
		const widened = `${warned}\nexport const more = (tone: "accent" | "warning" | "error"): TspPagesProps => ({ pages: [], marks: [{ tone }] });`;
		expect(auditValues(widened, "marks[].tone", ["accent", "warning", "error"]).details.unproducedValues).toBeUndefined();
	});

	test("a write to a class's own field is no write into the props; an untyped target still is", () => {
		const own = `export class Box { secret = false; flip(): void { this.secret = false; } }
export const pages = (): TspPagesProps => ({ pages: [], secret: true });`;
		expect(auditValues(own, "secret", [false, true]).details.unproducedValues).toMatchObject({ values: [false] });
		const untyped = `${own}\nexport const leak = (target: any): void => { target.secret = false; };`;
		expect(auditValues(untyped, "secret", [false, true]).details.unproducedValues).toBeUndefined();
	});
});
