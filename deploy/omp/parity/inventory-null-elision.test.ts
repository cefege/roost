import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { elideUnstorableNull } from "./inventory-null-elision";
import { nullDomainReasons } from "./coverage-domain";
import { SourceIndex } from "./inventory-source";
import type { InventoryEntry } from "./inventory-types";

const root = mkdtempSync(path.join(tmpdir(), "parity-null-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

const RECONCILE = `export function wire(props: Record<string, unknown>) {
	const out: { p?: Record<string, unknown> } = {};
	let p: Record<string, unknown> | undefined;
	for (const key in props) {
		const value = props[key];
		if (value !== undefined && value !== null) (p ??= {})[key] = value;
	}
	if (p) out.p = p;
	return out;
}`;
const APPLY_BUILD = `export function build(wire: Record<string, unknown>, node: { p: Record<string, unknown> }) {
	for (const key in wire) {
		const value = wire[key];
		if (value !== undefined && value !== null) node.p[key] = value;
	}
}`;
const APPLY_SET = `export function set(props: Record<string, unknown>, node: { p: Record<string, unknown> }) {
	for (const key in props) {
		const value = props[key];
		if (value === null) delete node.p[key];
		else if (value !== undefined) node.p[key] = value;
	}
}`;

function tree(reconcile: string, apply: string): SourceIndex {
	const dir = mkdtempSync(path.join(root, "tree-"));
	for (const name of ["wire", "coding-agent"]) mkdirSync(path.join(dir, "packages", name, "src"), { recursive: true });
	mkdirSync(path.join(dir, "packages/tui/src/native"), { recursive: true });
	writeFileSync(path.join(dir, "packages/wire/src/tsp.ts"), "export interface TspPickerProps { confirm?: string | null; query?: string | null }\n");
	writeFileSync(path.join(dir, "packages/tui/src/native/reconcile.ts"), `${reconcile}\n`);
	writeFileSync(path.join(dir, "packages/tui/src/native/apply.ts"), `${apply}\n`);
	writeFileSync(path.join(dir, "packages/coding-agent/src/index.ts"), "export {};\n");
	return new SourceIndex(dir);
}

const entry = (path: string, exposure = "ui", declaringType = "TspPickerProps"): InventoryEntry => ({
	id: `property:${declaringType}.${path}`, category: "property", source: "packages/wire/src/tsp.ts", line: 1,
	details: { declaringType, path, exposure, enumValues: [null, "x"], enumDomain: { values: [null, "x"], finite: true, unresolved: false, types: ["null", "\"x\""] } },
});
const roots = new Set(["TspPickerProps"]);

describe("unstorable null elision", () => {
	test("a top-level prop null leaves the domain with all three located document writes, and the gate accepts it", () => {
		const entries = [entry("confirm"), entry("items[]"), entry("confirm", "native-contract"), entry("confirm", "ui", "TspOther")];
		elideUnstorableNull(tree(RECONCILE, `${APPLY_BUILD}\n${APPLY_SET}`), entries, roots);
		expect(entries[0]!.details.enumValues).toEqual(["x"]);
		expect(entries[0]!.details.enumDomain).toMatchObject({ excludedValues: [{ value: null, reason: "top-level-null-is-absent-prop" }] });
		expect(nullDomainReasons(entries[0]!)).toEqual([]);
		for (const kept of entries.slice(1)) expect(kept.details.enumValues).toEqual([null, "x"]);
	});

	test("a missing document write keeps null demanded", () => {
		const noSet = [entry("confirm")];
		elideUnstorableNull(tree(RECONCILE, APPLY_BUILD), noSet, roots);
		expect(noSet[0]!.details.enumValues).toEqual([null, "x"]);
		const storesNull = [entry("confirm")];
		elideUnstorableNull(tree(RECONCILE.replace(" && value !== null", ""), `${APPLY_BUILD}\n${APPLY_SET}`), storesNull, roots);
		expect(storesNull[0]!.details.enumValues).toEqual([null, "x"]);
	});

	test("the gate rejects a declared null removed without its proof, or with a forged one", () => {
		const forged = entry("query");
		forged.details.enumValues = ["x"];
		expect(nullDomainReasons(forged)).toHaveLength(1);
		forged.details.enumDomain = { ...(forged.details.enumDomain as object), excludedValues: [{ value: null, reason: "top-level-null-is-absent-prop", proof: { producerWire: { source: "x.ts", line: 1, expression: "if (a !== null)" }, addBuild: {}, setClear: {} } }] };
		expect(nullDomainReasons(forged)).toHaveLength(1);
		const untouched = entry("query");
		expect(nullDomainReasons(untouched)).toEqual([]);
	});
});
