import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { nativeKindEmitters } from "./inventory-native-kinds";
import { SourceIndex } from "./inventory-source";
import { nameOccurrences, unreferencedProducers } from "./inventory-unproduced";

const root = mkdtempSync(path.join(tmpdir(), "parity-kinds-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

const FACTORY = `import type { NativeNode, Kind } from "./node";
export function node<K extends Kind>(k: K): NativeNode {
	return { k } as NativeNode;
}`;

function sourceTree(producers: string): SourceIndex {
	const tree = mkdtempSync(path.join(root, "tree-"));
	mkdirSync(path.join(tree, "packages/tui/src/native"), { recursive: true });
	writeFileSync(path.join(tree, "packages/tui/src/native/node.ts"), `export type NativeNode = { k: "text" } | { k: "math" } | { k: "rows" };\nexport type Kind = NativeNode["k"];\n`);
	writeFileSync(path.join(tree, "packages/tui/src/native/describe.ts"), `${FACTORY}\n`);
	writeFileSync(path.join(tree, "packages/tui/src/use.ts"), `import { node } from "./native/describe";\nimport type { NativeNode } from "./native/node";\n${producers}\n`);
	return new SourceIndex(tree);
}

function kinds(source: SourceIndex): Record<string, number[]> {
	return Object.fromEntries([...nativeKindEmitters(source)].map(([kind, emitters]) => [kind, emitters.map(emitter => source.origin(emitter.node).line)]));
}

describe("native kind producers", () => {
	test("a generic factory produces each kind at the call that passes it, and a copied kind produces none", () => {
		const source = sourceTree(`export function label(): NativeNode { return node("text"); }
export function copy(described: NativeNode): NativeNode { return node(described.k); }
export function grid(): NativeNode | null { const made: NativeNode | null = { k: "rows" }; return made; }`);
		expect(kinds(source)).toEqual({ text: [3], rows: [5] });
	});

	test("a factory that escapes as a value keeps every kind its constraint admits", () => {
		const source = sourceTree(`export const builders = [node];\nexport function label(): NativeNode { return node("text"); }`);
		expect(Object.keys(kinds(source)).sort()).toEqual(["math", "rows", "text"]);
	});

	test("a producer inside a function no source names is unreferenced; a name spelled in a string is not", () => {
		const source = sourceTree(`export function describeLatex(): NativeNode { return node("math"); }
export function describeRows(): NativeNode { return node("rows"); }
export const registry = { rows: "describeRows" };`);
		const emitters = nativeKindEmitters(source);
		const names = nameOccurrences(source);
		const math = unreferencedProducers(source, (emitters.get("math") ?? []).map(emitter => emitter.node), names);
		expect(math).toEqual([{ producer: expect.objectContaining({ source: "packages/tui/src/use.ts", line: 3 }), function: expect.objectContaining({ name: "describeLatex", line: 3 }), occurrences: 1 }]);
		expect(unreferencedProducers(source, (emitters.get("rows") ?? []).map(emitter => emitter.node), names)).toBeUndefined();
	});
});
