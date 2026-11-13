import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import ts from "typescript";
import { componentInventory } from "./inventory-components";
import { SourceFlowIndex } from "./inventory-flow";
import { SourceIndex } from "./inventory-source";

const root = mkdtempSync(path.join(tmpdir(), "parity-flow-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

/** A native-only editor field whose handler queues events during a paste and drains them from TTY input. */
function sourceTree(handleInput: string): SourceIndex {
	const tree = mkdtempSync(path.join(root, "tree-"));
	for (const dir of ["packages/wire/src", "packages/tui/src/native", "packages/coding-agent/src"]) mkdirSync(path.join(tree, dir), { recursive: true });
	writeFileSync(path.join(tree, "packages/wire/src/tsp.ts"), `export interface TspEditorProps {
	text: string;
	/** @nativeContract Browser send enablement. */
	sendable?: boolean;
}
export type TspEvent = { ev: "send"; id: string; text: string } | { ev: "undo"; id: string };
`);
	writeFileSync(path.join(tree, "packages/tui/src/native/node.ts"), "export type NativeUiEvent = { type: \"send\"; key: string; text: string } | { type: \"undo\"; key: string };\n");
	writeFileSync(path.join(tree, "packages/tui/src/tui.ts"), `import type { NativeUiEvent } from "./native/node";
export interface Component { render(width: number): string[]; handleInput?(data: string): void; describe?(): unknown; handleNativeEvent?(event: NativeUiEvent): void }
`);
	writeFileSync(path.join(tree, "packages/tui/src/editor.ts"), `import type { TspEditorProps } from "../../wire/src/tsp";
import type { NativeUiEvent } from "./native/node";
import type { Component } from "./tui";
export class Editor implements Component {
	nativeSendable = false;
	#pending: NativeUiEvent[] = [];
	#pasting = false;
	render(width: number): string[] { return [String(width)]; }
	handleInput(data: string): void {
		this.#pasting = data.length > 1;
		${handleInput}
	}
	describe(): { k: "editor"; p: TspEditorProps } { return { k: "editor", p: { text: "", sendable: this.nativeSendable } }; }
	handleNativeEvent(event: NativeUiEvent): void {
		if (this.#pasting) { this.#pending.push(event); return; }
		if (event.type === "send" && this.nativeSendable) this.nativeSendable = false;
	}
}
`);
	writeFileSync(path.join(tree, "packages/tui/src/native/backend.ts"), `import type { TspEvent } from "../../../wire/src/tsp";
import type { NativeUiEvent } from "./node";
export function routeUiEvent(event: TspEvent, target: { handleNativeEvent(event: NativeUiEvent): void }): void {
	if (event.ev === "send") target.handleNativeEvent({ type: "send", key: event.id, text: event.text });
}
`);
	return new SourceIndex(tree);
}

function sendableProof(handleInput: string): { proven: boolean; ttyReads: unknown[]; replayedReads: unknown[] } {
	const source = sourceTree(handleInput);
	const flow = new SourceFlowIndex(source, componentInventory(source));
	const declarations: ts.Node[] = [];
	const producers: ts.Node[] = [];
	source.walk(source.file("packages/wire/src/tsp.ts"), node => { if (ts.isPropertySignature(node) && node.name.getText() === "sendable") declarations.push(node); });
	source.walk(source.file("packages/tui/src/editor.ts"), node => { if (ts.isPropertyAssignment(node) && node.name.getText() === "sendable") producers.push(node); });
	const proof = flow.nativeContract(declarations, producers.map(node => ({ node, role: "typed-property-emitter" })));
	if (!proof) throw new Error("sendable carries no @nativeContract proof");
	return proof as { proven: boolean; ttyReads: unknown[]; replayedReads: unknown[] };
}

describe("native-contract reads replayed from TTY input", () => {
	test("a handler read reached from TTY only by draining queued wire-delivered events is a native read", () => {
		const proof = sendableProof("for (const event of this.#pending.splice(0)) this.handleNativeEvent(event);");
		expect(proof).toMatchObject({ proven: true, ttyReads: [] });
		expect(proof.replayedReads).toHaveLength(1);
	});

	test("an event TTY input constructs itself keeps the handler read a TTY read", () => {
		const proof = sendableProof("this.handleNativeEvent({ type: \"send\", key: \"editor\", text: data });");
		expect(proof).toMatchObject({ proven: false, replayedReads: [] });
		expect(proof.ttyReads).toHaveLength(1);
	});
});

function constantEmitterTree(): SourceIndex {
	const tree = mkdtempSync(path.join(root, "tree-"));
	for (const dir of ["packages/wire/src", "packages/tui/src/native", "packages/coding-agent/src"]) mkdirSync(path.join(tree, dir), { recursive: true });
	writeFileSync(path.join(tree, "packages/wire/src/tsp.ts"), "export interface TspColumn { id: string; format?: \"text\" | \"price\" | \"bar\" }\nexport interface TspTableProps { columns: readonly TspColumn[] }\n");
	writeFileSync(path.join(tree, "packages/tui/src/tui.ts"), "export interface Component { render(width: number): string[]; describe?(): unknown }\n");
	writeFileSync(path.join(tree, "packages/tui/src/table.ts"), `import type { TspColumn, TspTableProps } from "../../wire/src/tsp";
import type { Component } from "./tui";
const USED: readonly TspColumn[] = [{ id: "cost", format: "price" }];
const ORPHAN: readonly TspColumn[] = [{ id: "share", format: "bar" }];
export const ignored = ORPHAN.length;
export class Table implements Component {
	readonly #columns = [{ id: "name", format: "text" }] satisfies readonly TspColumn[];
	render(width: number): string[] { return [String(width)]; }
	describe(): { k: "table"; p: TspTableProps } { return { k: "table", p: { columns: [...USED, ...this.#columns] } }; }
}
`);
	return new SourceIndex(tree);
}

describe("emitters declared in module constants and class fields", () => {
	test("a constant is produced by the describe path that references it; an unreferenced one is not", () => {
		const source = constantEmitterTree();
		const flow = new SourceFlowIndex(source, componentInventory(source));
		const formats: Record<string, ts.Node> = {};
		source.walk(source.file("packages/tui/src/table.ts"), node => {
			if (ts.isPropertyAssignment(node) && node.name.getText() === "format" && ts.isStringLiteral(node.initializer)) formats[node.initializer.text] = node;
		});
		const emitters = (key: string) => flow.uiEmitters([{ node: formats[key]!, role: "typed-property-emitter" }]);
		expect(emitters("price")).toHaveLength(1);
		expect(emitters("price")[0]).toMatchObject({ nativeHook: { method: "describe" } });
		expect(emitters("text")).toHaveLength(1);
		expect(emitters("bar")).toEqual([]);
	});
});
