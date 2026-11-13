import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { schemaInventory } from "./inventory-schema";
import { SourceIndex } from "./inventory-source";

const root = mkdtempSync(path.join(tmpdir(), "parity-spread-emitters-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

const WIRE = `export const TSP_KINDS = ["row", "seg"] as const;
export type TspKind = (typeof TSP_KINDS)[number];
export interface TspCommonProps { role?: string }
export interface TspRowProps extends TspCommonProps { label: string; disabled?: boolean; min?: string }
export interface TspSegProps extends TspCommonProps { side?: "left" | "right"; min?: string; icon?: string }
export interface TspPropsByKind { row: TspRowProps; seg: TspSegProps }
export type TspProps<K extends TspKind = TspKind> = K extends TspKind ? TspPropsByKind[K] : never;
export type TspEvent = { ev: "go"; id: string };
export type TspQuery = { q: "hello" };
export type TspReply = { r: "hello"; v: number };
export type TspOp = readonly ["add", string];
export type TspVerb = "q" | "o";
`;

/** The emitters of each member after the schema inventory annotates `producer`'s typed uses. */
function emitters(producer: string): Record<string, string[]> {
	const tree = mkdtempSync(path.join(root, "tree-"));
	for (const name of ["wire", "tui"]) mkdirSync(path.join(tree, "packages", name, "src"), { recursive: true });
	writeFileSync(path.join(tree, "packages/wire/src/tsp.ts"), WIRE);
	writeFileSync(path.join(tree, "packages/tui/src/use.ts"), `import type { TspProps, TspRowProps, TspSegProps } from "../../wire/src/tsp";
declare function node<K extends "row" | "seg">(kind: K, props?: TspProps<K>): unknown;
${producer}
`);
	const result: Record<string, string[]> = {};
	for (const entry of schemaInventory(new SourceIndex(tree), [])) {
		if (entry.category !== "property") continue;
		const list = [...(Array.isArray(entry.details.emitters) ? entry.details.emitters : [])] as Array<{ role: string }>;
		result[entry.id.replace("property:", "")] = list.map(item => item.role);
	}
	return result;
}

describe("typed spread emitters", () => {
	test("a spread of the container's own members copies them and produces none", () => {
		const found = emitters(`export const seg = (key: string, props: TspProps<"seg">): void => { node("seg", { role: key, ...props }); };
export const relabel = (rows: TspRowProps[]) => rows.map(row => ({ ...row, label: "x" }) satisfies TspRowProps);
`);
		expect(found["TspSegProps.min"]).toEqual([]);
		expect(found["TspSegProps.icon"]).toEqual([]);
		expect(found["TspRowProps.disabled"]).toEqual([]);
		expect(found["TspRowProps.label"]).toEqual(["typed-property-emitter"]);
	});

	test("a spread of an ad-hoc object emits each member it carries into the typed literal", () => {
		const found = emitters(`export const seg = (extra: { min: string; icon?: string }): void => { node("seg", { role: "r", ...extra }); };`);
		expect(found["TspSegProps.min"]).toEqual(["typed-spread-emitter"]);
		expect(found["TspSegProps.icon"]).toEqual(["typed-spread-emitter"]);
		expect(found["TspSegProps.side"]).toEqual([]);
	});

	test("a spread of another kind's props does not emit this kind's same-named member", () => {
		const found = emitters(`export const seg = (row: TspRowProps): void => { node("seg", { ...row }); };`);
		expect(found["TspSegProps.min"]).toEqual(["typed-spread-emitter"]);
	});

	test("a shorthand member and a later assignment on the same typed binding both emit", () => {
		const found = emitters(`export function build(side: "left" | "right", wide: boolean): unknown {
	const props: TspProps<"seg"> = { role: "r", side };
	if (wide) props.min = "12ch";
	return node("seg", props);
}`);
		expect(found["TspSegProps.side"]).toEqual(["typed-property-emitter"]);
		expect(found["TspSegProps.min"]).toEqual(["typed-property-write"]);
		expect(found["TspSegProps.icon"]).toEqual([]);
	});
});
