import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import ts from "typescript";
import { componentInventory } from "./inventory-components";
import { SourceFlowIndex, type SourceEmitter } from "./inventory-flow";
import { SourceIndex } from "./inventory-source";

const root = mkdtempSync(path.join(tmpdir(), "parity-browser-only-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

interface Variant { tag: string; ackTag?: string; render: string; describe: string; extra?: string; handler?: string }

/** A chart component whose `tips` prop and an editor-style `ack` field are tagged on the wire declaration. */
function tree(variant: Variant): SourceIndex {
	const dir = mkdtempSync(path.join(root, "tree-"));
	for (const sub of ["packages/wire/src", "packages/tui/src/native", "packages/coding-agent/src"]) mkdirSync(path.join(dir, sub), { recursive: true });
	writeFileSync(path.join(dir, "packages/wire/src/tsp.ts"), `export interface TspChartProps {
	data: number[];
	${variant.tag}
	tips?: string[];
	${variant.ackTag ?? ""}
	ack?: number;
}
export type TspEvent = { ev: "ack"; id: string; n: number };
`);
	writeFileSync(path.join(dir, "packages/tui/src/native/node.ts"), "export type NativeUiEvent = { type: \"ack\"; key: string; n: number };\n");
	writeFileSync(path.join(dir, "packages/tui/src/tui.ts"), `import type { NativeUiEvent } from "./native/node";
export interface Component { render(width: number): string[]; handleInput?(data: string): void; describe?(): unknown; handleNativeEvent?(event: NativeUiEvent): void }
`);
	writeFileSync(path.join(dir, "packages/tui/src/native/backend.ts"), `import type { TspEvent } from "../../../wire/src/tsp";
import type { NativeUiEvent } from "./node";
export function routeUiEvent(event: TspEvent, target: { handleNativeEvent(event: NativeUiEvent): void }): void {
	if (event.ev === "ack") target.handleNativeEvent({ type: "ack", key: event.id, n: event.n });
}
`);
	writeFileSync(path.join(dir, "packages/tui/src/chart.ts"), `import type { TspChartProps } from "../../wire/src/tsp";
import type { NativeUiEvent } from "./native/node";
import type { Component } from "./tui";
export class Chart implements Component {
	#ack = 0;
	props: TspChartProps = { data: [] };
	getAck(): number { return this.#ack; }
	render(width: number): string[] { ${variant.render} }
	describe(): { k: "chart"; p: TspChartProps } { return { k: "chart", p: ${variant.describe} }; }
	handleNativeEvent(event: NativeUiEvent): void { ${variant.handler ?? "void event;"} }
	${variant.extra ?? ""}
}
`);
	return new SourceIndex(dir);
}

function members(source: SourceIndex, name: string): { declarations: ts.Node[]; emitters: SourceEmitter[] } {
	const declarations: ts.Node[] = [];
	const emitters: SourceEmitter[] = [];
	source.walk(source.file("packages/wire/src/tsp.ts"), node => { if (ts.isPropertySignature(node) && node.name.getText() === name) declarations.push(node); });
	source.walk(source.file("packages/tui/src/chart.ts"), node => {
		if (ts.isPropertyAssignment(node) && node.name.getText() === name) emitters.push({ node, role: "typed-property-emitter" });
		else if (ts.isPropertyAccessExpression(node) && node.name.text === name && !ts.isPropertyAssignment(node.parent)) emitters.push({ node, role: "typed-property-read" });
	});
	return { declarations, emitters };
}

function browserOnly(variant: Variant, name: string): Record<string, unknown> {
	const source = tree(variant);
	const flow = new SourceFlowIndex(source, componentInventory(source));
	const { declarations, emitters } = members(source, name);
	const proof = flow.browserOnly(declarations, emitters);
	if (!proof) throw new Error(`${name} carries no @browserOnly tag`);
	return proof;
}

const TAG = "/** @browserOnly Hover tooltip the terminal never draws. */";

describe("browser-only wire members", () => {
	test("a tagged member that only the native describe path writes and reads is proven", () => {
		const proof = browserOnly({ tag: TAG, render: "return [String(width)];", describe: "{ data: [], tips: [\"a\"] }" }, "tips");
		expect(proof).toMatchObject({ proven: true, ttyReads: [], unplacedReads: [] });
		expect((proof.tags as { rationale: string }[])[0]!.rationale).toContain("Hover tooltip");
	});

	test("an untagged member carries no proof, so it stays in the terminal-parity lane", () => {
		const source = tree({ tag: "", render: "return [String(width)];", describe: "{ data: [], tips: [\"a\"] }" });
		const flow = new SourceFlowIndex(source, componentInventory(source));
		const { declarations, emitters } = members(source, "tips");
		expect(flow.browserOnly(declarations, emitters)).toBeUndefined();
	});

	test("terminal code that reads the tagged member rejects the tag", () => {
		const proof = browserOnly({ tag: TAG, render: "return this.props.tips ?? [String(width)];", describe: "{ data: [], tips: [\"a\"] }" }, "tips");
		expect(proof.proven).toBe(false);
		expect(proof.ttyReads).toHaveLength(1);
	});

	test("a tagged member nothing writes is unproven", () => {
		const proof = browserOnly({ tag: TAG, render: "return [String(width)];", describe: "{ data: [] }" }, "tips");
		expect(proof.proven).toBe(false);
		expect(proof.producers).toEqual([]);
	});

	test("a read no root reaches leaves the member unplaced and unproven", () => {
		const proof = browserOnly({ tag: TAG, render: "return [String(width)];", describe: "{ data: [], tips: [\"a\"] }", extra: "orphan(): unknown { return this.props.tips; }" }, "tips");
		expect(proof.proven).toBe(false);
		expect(proof.unplacedReads).toHaveLength(1);
	});
});

describe("native-contract fields handed out by a getter helper", () => {
	const nativeTag = "/** @nativeContract Browser echo token. */";
	const handler = "if (event.type === \"ack\") this.#ack = event.n;";
	const nativeProof = (variant: Variant): Record<string, unknown> => {
		const source = tree(variant);
		const flow = new SourceFlowIndex(source, componentInventory(source));
		const declarations: ts.Node[] = [];
		const emitters: SourceEmitter[] = [];
		source.walk(source.file("packages/wire/src/tsp.ts"), node => { if (ts.isPropertySignature(node) && node.name.getText() === "ack") declarations.push(node); });
		source.walk(source.file("packages/tui/src/chart.ts"), node => { if (ts.isPropertyAssignment(node) && node.name.getText() === "ack") emitters.push({ node, role: "typed-property-emitter" }); });
		const proof = flow.nativeContract(declarations, emitters);
		if (!proof) throw new Error("ack carries no @nativeContract tag");
		return proof;
	};

	test("a single-return getter resolves to its backing field, written by the native event handler", () => {
		const proof = nativeProof({ tag: "", ackTag: nativeTag, render: "return [String(width)];", describe: "{ data: [], ack: this.getAck() }", handler });
		expect(proof).toMatchObject({ proven: true, ttyReads: [] });
		expect(proof.backingFields).toHaveLength(1);
	});

	test("terminal code that reads the same backing field rejects the tag", () => {
		const proof = nativeProof({ tag: "", ackTag: nativeTag, render: "return [String(this.getAck())];", describe: "{ data: [], ack: this.getAck() }", handler });
		expect(proof.proven).toBe(false);
		expect(proof.ttyReads).not.toHaveLength(0);
	});

	test("a getter that computes rather than returns a field is not followed, leaving the tag unproven", () => {
		const proof = nativeProof({ tag: "", ackTag: nativeTag, render: "return [String(width)];", describe: "{ data: [], ack: this.getAck() }", handler, extra: "" });
		expect(proof.backingFields).toHaveLength(1);
		const computed = nativeProof({ tag: "", ackTag: nativeTag, render: "return [String(width)];", describe: "{ data: [], ack: this.computeAck() }", handler, extra: "computeAck(): number { const next = this.#ack + 1; return next; }" });
		expect(computed.proven).toBe(false);
		expect(computed.backingFields).toEqual([]);
	});
});
