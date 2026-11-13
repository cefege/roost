import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { classifyUnemittedOperations } from "./inventory-host-audit";
import { SourceIndex } from "./inventory-source";
import type { InventoryEntry } from "./inventory-types";

const root = mkdtempSync(path.join(tmpdir(), "parity-operations-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

function classify(consumer: string): InventoryEntry {
	const tree = mkdtempSync(path.join(root, "tree-"));
	for (const [name, text] of Object.entries({ "packages/wire/src/tsp.ts": `export type TspOp = ["suspend"] | ["go"];\n`, "packages/tui/src/consumer.ts": consumer })) {
		mkdirSync(path.dirname(path.join(tree, name)), { recursive: true });
		writeFileSync(path.join(tree, name), text);
	}
	const entry: InventoryEntry = { id: "operation:suspend", category: "operation", source: "packages/wire/src/tsp.ts", line: 1,
		details: { exposure: "native-contract", exposureProof: { source: "packages/wire/src/tsp.ts", line: 1 }, discriminator: { value: "suspend" }, emitters: [{ role: "typed-variant-consumer" }] } };
	classifyUnemittedOperations(new SourceIndex(tree), [entry]);
	return entry;
}

describe("operations OMP only consumes", () => {
	test("a tag that is only compared, or named in a type, is unemitted", () => {
		const entry = classify(`type Handled = "suspend" | "go";\nexport const run = (op: Handled) => { switch (op) { case "suspend": return 1; default: return op === "go" ? 2 : 3; } };\n`);
		expect(entry.details.exposure).toBe("unproduced");
		expect(entry.details.unproducedProof).toMatchObject({ proof: "tag-only-compared", key: "suspend" });
	});

	test("a tag used as a value could build the operation, so it stays a contract", () => {
		expect(classify(`export const build = () => ["suspend"];\n`).details.exposure).toBe("native-contract");
		expect(classify(`export const names = ["suspend", "go"] as unknown[];\n`).details.exposure).toBe("native-contract");
	});

	test("a tag typed as another union or as string cannot build the tuple, and is listed in the proof", () => {
		const entry = classify(`type Sub = "suspend" | "go";\nexport const SUBS = new Set<Sub>(["suspend", "go"]);\nexport const unannotated = new Set(["suspend", "go"]);\nexport const label = { name: "suspend" } satisfies { name: string };\nexport function pick(): string { return "suspend"; }\nexport const parse = (text: Sub) => text;\nparse("suspend");\n`);
		expect(entry.details.exposure).toBe("unproduced");
		expect((entry.details.unproducedProof as { typedElsewhere: unknown[] }).typedElsewhere.length).toBe(5);
	});

	test("a tag in an any, unknown, untyped or tuple-typed position still blocks", () => {
		for (const consumer of [
			`export const items: any[] = []; items.push("suspend");\n`,
			`export const value: unknown = "suspend";\n`,
			`export const head = "suspend";\n`,
			`type Op = ["suspend"] | ["go"];\nexport const op: Op = ["suspend"];\n`,
			`export function make<T>(value: T): T { return value; }\nmake("suspend");\n`,
		]) expect(classify(consumer).details.exposure).toBe("native-contract");
	});
});
