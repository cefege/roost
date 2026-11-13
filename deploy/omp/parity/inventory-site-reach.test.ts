import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import ts from "typescript";
import { hostAdvertisedKinds } from "./inventory-kind-gates";
import { SiteReach } from "./inventory-site-reach";
import { SourceIndex } from "./inventory-source";
import type { InventoryEntry } from "./inventory-types";
import { classifyUnproduced } from "./inventory-unproduced";

const root = mkdtempSync(path.join(tmpdir(), "parity-reach-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

const ACP = "packages/coding-agent/src/commands/acp.ts";
const WIRE = `export interface TspColProps { truncate?: "start" | "end"; align?: "left" | "center"; steps?: number }\n`;
const LAUNCHER = { source: "crates/roost-worker/src/omp/launcher.rs", line: 108 };
const HOST = { kinds: new Set(["chart"]), source: "crates/roost-web/host.rs", line: 19 };

function tree(files: Record<string, string>): SourceIndex {
	const directory = mkdtempSync(path.join(root, "tree-"));
	for (const [name, text] of Object.entries({ "packages/wire/src/tsp.ts": WIRE, ...files })) {
		mkdirSync(path.dirname(path.join(directory, name)), { recursive: true });
		writeFileSync(path.join(directory, name), text);
	}
	return new SourceIndex(directory);
}

/** The first node whose text is `needle`, in a file. */
function find(source: SourceIndex, file: string, needle: string): ts.Node {
	let found: ts.Node | undefined;
	source.walk(source.file(file), node => { if (!found && ts.isStringLiteral(node) && node.text === needle) found = node; });
	return found!;
}

describe("session import closure", () => {
	const files = {
		[ACP]: `import "../reached"; export default async function run() { const lazy = await import("../lazy"); return lazy; }`,
		"packages/coding-agent/src/reached.ts": `export const reached = "mark-reached";`,
		"packages/coding-agent/src/lazy.ts": `export const lazy = "mark-lazy";`,
		"packages/coding-agent/src/orphan.ts": `export const orphan = "mark-orphan";`,
	};

	test("a file no session command loads is excluded; static and dynamic imports stay in", () => {
		const source = tree(files);
		const reach = new SiteReach(source, HOST, [ACP], LAUNCHER);
		expect(reach.exclusion(find(source, "packages/coding-agent/src/reached.ts", "mark-reached"))).toBeUndefined();
		expect(reach.exclusion(find(source, "packages/coding-agent/src/lazy.ts", "mark-lazy"))).toBeUndefined();
		expect(reach.exclusion(find(source, "packages/coding-agent/src/orphan.ts", "mark-orphan"))).toMatchObject({ proof: "outside-session-import-closure", entries: [ACP] });
	});

	test("a command registry that lazily loads the file keeps it in the closure", () => {
		const source = tree({ ...files, [ACP]: `import "../registry"; export default function run() {}`, "packages/coding-agent/src/registry.ts": `export const commands = { orphan: () => import("./orphan") };` });
		const reach = new SiteReach(source, HOST, [ACP], LAUNCHER);
		expect(reach.exclusion(find(source, "packages/coding-agent/src/orphan.ts", "mark-orphan"))).toBeUndefined();
	});

	test("a computed load keeps the closure, but only excludes a file whose name no session string spells", () => {
		const computed = { ...files, [ACP]: `export default async function run(name: string) { return import(name); }` };
		const unspelledSource = tree(computed);
		expect(new SiteReach(unspelledSource, HOST, [ACP], LAUNCHER).exclusion(find(unspelledSource, "packages/coding-agent/src/orphan.ts", "mark-orphan"))).toMatchObject({ proof: "outside-session-import-closure", computedLoads: 1 });
		const spelledSource = tree({ ...files, [ACP]: `export default async function run(name: string) { return import(name) ?? "./src/orphan"; }` });
		expect(new SiteReach(spelledSource, HOST, [ACP], LAUNCHER).exclusion(find(spelledSource, "packages/coding-agent/src/orphan.ts", "mark-orphan"))).toBeUndefined();
	});

	test("without launcher evidence nothing is excluded", () => {
		const source = tree(files);
		const reach = new SiteReach(source, HOST, [ACP], null);
		expect(reach.exclusion(find(source, "packages/coding-agent/src/orphan.ts", "mark-orphan"))).toBeUndefined();
	});
});

describe("host-gated fallbacks", () => {
	const gated = `
export class View {
	render(cx: { supports(kind: string): boolean }) { return cx.supports("chart") ? this.#chart() : this.#table(); }
	#chart() { return "mark-chart"; }
	#table() { return "mark-table"; }
}`;
	const file = "packages/coding-agent/src/view.ts";

	test("a fallback only the rejected branch calls is excluded when the host advertises the kind", () => {
		const source = tree({ [file]: gated });
		const reach = new SiteReach(source, HOST, [file], LAUNCHER);
		expect(reach.exclusion(find(source, file, "mark-table"))).toMatchObject({ proof: "host-gated-fallback", kind: "chart" });
		expect(reach.exclusion(find(source, file, "mark-chart"))).toBeUndefined();
	});

	test("a kind the host does not advertise excludes nothing", () => {
		const source = tree({ [file]: gated });
		const reach = new SiteReach(source, { ...HOST, kinds: new Set(["table"]) }, [file], LAUNCHER);
		expect(reach.exclusion(find(source, file, "mark-table"))).toBeUndefined();
	});

	test("a fallback also called outside the gate is not excluded", () => {
		const source = tree({ [file]: gated.replace(`this.#table(); }`, `this.#table(); }\n\tagain() { return this.#table(); }`) });
		const reach = new SiteReach(source, HOST, [file], LAUNCHER);
		expect(reach.exclusion(find(source, file, "mark-table"))).toBeUndefined();
	});

	test("the real Roost host advertises the kinds the fork's fallbacks are gated on", () => {
		const { kinds, source } = hostAdvertisedKinds();
		expect(source).toBe("crates/roost-web/src/components/agent_chat/omp_ui_mount_transport.rs");
		for (const kind of ["chart", "tool", "effort"]) expect(kinds.has(kind)).toBe(true);
	});
});

describe("unproduced values behind unreachable sites", () => {
	const entry = (key: string, enumValues: string[]): InventoryEntry => ({
		id: `property:TspColProps.${key}`, category: "property", source: "packages/wire/src/tsp.ts", line: 1,
		details: { declaringType: "TspColProps", path: key, exposure: "ui", enumValues, declarations: [{ source: "packages/wire/src/tsp.ts", line: 1 }], uiEmitters: [{ source: "packages/coding-agent/src/table.ts", line: 2 }] },
	});
	const run = (files: Record<string, string>) => {
		const truncate = entry("truncate", ["start", "end"]);
		classifyUnproduced(tree(files), [truncate]);
		return truncate.details.unproducedValues as { values: string[]; excludedSites: unknown[] } | undefined;
	};
	const producer = (value: string) => `import type { TspColProps } from "../../wire/src/tsp";\nexport const column: TspColProps = { truncate: "${value}" };\n`;
	const loaded = { [ACP]: `import "../table"; export default function run() {}` };

	test("a value only an unloaded file writes is unproduced, with the excluded site in the proof", () => {
		const proof = run({ ...loaded, "packages/coding-agent/src/table.ts": producer("end"), "packages/coding-agent/src/predict.ts": producer("start") });
		expect(proof).toMatchObject({ values: ["start"] });
		expect(proof!.excludedSites).toHaveLength(1);
	});

	test("the same value written by a loaded file stays demanded", () => {
		expect(run({ ...loaded, "packages/coding-agent/src/table.ts": `${producer("end")}export const other: TspColProps = { truncate: "start" };\n` })).toBeUndefined();
	});

	test("an unloaded file cannot hide an open write of the key from a loaded one", () => {
		const open = `export const extra = (value: unknown): unknown => ({ truncate: value });\n`;
		expect(run({ ...loaded, "packages/coding-agent/src/table.ts": producer("end") + open, "packages/coding-agent/src/predict.ts": producer("start") })).toBeUndefined();
	});

	test("a key whose every producer is unreachable is unproduced, and one reachable producer keeps it demanded", () => {
		const meter = `import type { TspColProps } from "../../wire/src/tsp";\nexport const meter: TspColProps = { steps: 4 };\n`;
		const steps = (files: Record<string, string>): InventoryEntry => {
			const member = entry("steps", []);
			member.details.enumValues = undefined;
			member.details.uiEmitters = [{ source: "packages/coding-agent/src/predict.ts", line: 2, start: meter.indexOf("steps"), end: meter.indexOf("steps") + 8, role: "typed-property-emitter" }];
			classifyUnproduced(tree(files), [member]);
			return member;
		};
		const unreachable = steps({ ...loaded, "packages/coding-agent/src/table.ts": producer("end"), "packages/coding-agent/src/predict.ts": meter });
		expect(unreachable.details.exposure).toBe("unproduced");
		expect(unreachable.details.unproducedProof).toMatchObject({ proof: "every-reachable-typed-site-lacks-key", excludedSites: [{ exclusion: { proof: "outside-session-import-closure" } }] });
		const reachable = steps({ [ACP]: `import "../predict"; export default function run() {}`, "packages/coding-agent/src/predict.ts": meter });
		expect(reachable.details.exposure).toBe("ui");
	});
});
