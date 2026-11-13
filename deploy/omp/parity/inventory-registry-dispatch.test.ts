import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import ts from "typescript";
import { SiteReach } from "./inventory-site-reach";
import { SourceIndex } from "./inventory-source";

const root = mkdtempSync(path.join(tmpdir(), "parity-registry-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

const ACP = "packages/coding-agent/src/commands/acp.ts";
const LAUNCHER = { source: "crates/roost-worker/src/omp/launcher.rs", line: 108 };
const HOST = { kinds: new Set<string>(), source: "crates/roost-web/host.rs", line: 19 };
const CLI = `export class Command {}
export interface CommandEntry { name: string; load(): Promise<unknown> }
async function loadEntry(entry: CommandEntry) { return entry.load(); }
export async function run(commands: CommandEntry[], id: string) { const entry = commands.find(item => item.name === id)!; return loadEntry(entry); }
`;
const REGISTRY = `import type { CommandEntry } from "../../utils/src/cli";
export const commands: CommandEntry[] = [
	{ name: "acp", load: () => import("./commands/acp") },
	{ name: "predict", load: () => import("./predict") },
];
export function loadAll(entries: CommandEntry[]) { return Promise.all(entries.map(entry => entry.load())); }
`;
const files = {
	"packages/utils/src/cli.ts": CLI,
	"packages/coding-agent/src/registry.ts": REGISTRY,
	"packages/coding-agent/src/predict.ts": `export const predict = "mark-predict";`,
};

function reach(acp: string, extra: Record<string, string> = {}): { source: SourceIndex; reach: SiteReach } {
	const directory = mkdtempSync(path.join(root, "tree-"));
	for (const [name, text] of Object.entries({ "packages/wire/src/tsp.ts": "export interface TspProps { a?: 1 }\n", ...files, [ACP]: acp, ...extra })) {
		mkdirSync(path.dirname(path.join(directory, name)), { recursive: true });
		writeFileSync(path.join(directory, name), text);
	}
	const source = new SourceIndex(directory);
	return { source, reach: new SiteReach(source, HOST, [ACP], LAUNCHER) };
}

function predictLiteral(source: SourceIndex): ts.Node {
	let found: ts.Node | undefined;
	source.walk(source.file("packages/coding-agent/src/predict.ts"), node => { if (ts.isStringLiteral(node)) found = node; });
	return found!;
}

describe("registry dispatch proof", () => {
	test("thunks stay deferred when load() is reached only through a dispatcher no session file imports", () => {
		const { source, reach: sessions } = reach(`import { Command } from "../../../utils/src/cli";\nimport { commands } from "../registry";\nexport default class Acp extends Command { names = commands.length }`);
		const exclusion = sessions.exclusion(predictLiteral(source));
		expect(exclusion).toMatchObject({
			proof: "outside-session-import-closure",
			registryDispatch: {
				deferredThunks: 2,
				invokers: [expect.objectContaining({ function: expect.stringMatching(/^load(All|Entry)$/) }), expect.objectContaining({ function: expect.stringMatching(/^load(All|Entry)$/) })],
				dispatchers: expect.arrayContaining([expect.objectContaining({ name: "run" }), expect.objectContaining({ name: "loadAll" })]),
			},
		});
	});

	const withLoader = (loader: string) => reach(`import { Command } from "../../../utils/src/cli";\nimport "../loader";\nimport { commands } from "../registry";\nexport default class Acp extends Command { names = commands.length }`, { "packages/coding-agent/src/loader.ts": loader });

	test("a static class load() is not a registry entry", () => {
		const { source, reach: sessions } = withLoader(`export class Manager { static async load() { return 1; } }\nexport const use = () => Manager.load();\n`);
		expect(sessions.exclusion(predictLiteral(source))).toMatchObject({ registryDispatch: { deferredThunks: 2 } });
	});

	test("an untyped load() receiver might be a registry entry, so it is not excluded", () => {
		const { source, reach: sessions } = withLoader(`export const use = (entry: any) => entry.load();\n`);
		expect(sessions.exclusion(predictLiteral(source))).toBeUndefined();
	});

	const sdk = (body: string, imports = `import "../entry";\nimport "../sdk";\n`) => reach(`import { Command } from "../../../utils/src/cli";\n${imports}import { commands } from "../registry";\nexport default class Acp extends Command { names = commands.length }`, {
		...chain,
		"packages/coding-agent/src/index.ts": `export * from "./entry";\n`,
		"packages/coding-agent/src/sdk.ts": body,
	});

	test("returning the barrel namespace to extension code is recorded, not a call", () => {
		const { source, reach: sessions } = sdk(`export function getApi() { return require("./index"); }\n`);
		expect(sessions.exclusion(predictLiteral(source))).toMatchObject({ registryDispatch: { namespaceHandoffs: [expect.objectContaining({ source: "packages/coding-agent/src/sdk.ts" })] } });
	});

	test("calling the process entry through a namespace from a session method is not excluded", () => {
		const { source, reach: sessions } = sdk(`import * as api from "./index";\nexport class Host { restart() { return api.main([]); } }\n`);
		expect(sessions.exclusion(predictLiteral(source))).toBeUndefined();
	});

	test("a namespace stored for later use is not traceable", () => {
		const { source, reach: sessions } = sdk(`import * as api from "./index";\nexport const hold = [api];\n`);
		expect(sessions.exclusion(predictLiteral(source))).toBeUndefined();
	});

	test("a then callback receiving the namespace is not traceable", () => {
		const { source, reach: sessions } = sdk(`export const later = () => import("./index").then(api => api.main([]));\n`);
		expect(sessions.exclusion(predictLiteral(source))).toBeUndefined();
	});

	test("a property read off an awaited import is traced; other members are not the dispatcher", () => {
		const other = sdk(`export async function pick() { return (await import("./index")).unrelated; }\n`);
		expect(other.reach.exclusion(predictLiteral(other.source))).toMatchObject({ registryDispatch: { deferredThunks: 2 } });
		const taken = sdk(`export async function pick() { return (await import("./index")).main; }\n`);
		expect(taken.reach.exclusion(predictLiteral(taken.source))).toBeUndefined();
	});

	const chain = {
		"packages/coding-agent/src/runner.ts": `export async function runCli(argv: string[]) { const { run } = await import("../../utils/src/cli"); const { commands } = await import("./registry"); return run(commands, argv[0]!); }\n`,
		"packages/coding-agent/src/entry.ts": `export async function main(args: string[]) { const [{ runCli }] = await Promise.all([import("./runner")]); await runCli(args); }\n`,
	};

	test("the proof climbs through process-entry functions nothing in the session calls", () => {
		const { source, reach: sessions } = reach(`import { Command } from "../../../utils/src/cli";\nimport "../entry";\nimport { commands } from "../registry";\nexport default class Acp extends Command { names = commands.length }`, chain);
		expect(sessions.exclusion(predictLiteral(source))).toMatchObject({ registryDispatch: { dispatchers: expect.arrayContaining([expect.objectContaining({ name: "runCli" }), expect.objectContaining({ name: "main" })]) } });
	});

	test("a session method that calls the process entry makes every thunk a session edge", () => {
		const { source, reach: sessions } = reach(`import { Command } from "../../../utils/src/cli";\nimport { main } from "../entry";\nimport { commands } from "../registry";\nexport default class Acp extends Command { names = commands.length; restart() { return main([]); } }`, chain);
		expect(sessions.exclusion(predictLiteral(source))).toBeUndefined();
	});

	test("a session file that imports the argv dispatcher makes every thunk a session edge", () => {
		const { source, reach: sessions } = reach(`import { Command, run } from "../../../utils/src/cli";\nimport { commands } from "../registry";\nexport default class Acp extends Command { go = () => run(commands, "x") }`);
		expect(sessions.exclusion(predictLiteral(source))).toBeUndefined();
	});

	test("a session file that loads every registry entry makes every thunk a session edge", () => {
		const { source, reach: sessions } = reach(`import { Command } from "../../../utils/src/cli";\nimport { commands, loadAll } from "../registry";\nexport default class Acp extends Command { go = () => loadAll(commands) }`);
		expect(sessions.exclusion(predictLiteral(source))).toBeUndefined();
	});

	test("a load() call outside any named function cannot be shown to be argv-keyed", () => {
		const { source, reach: sessions } = reach(`import { Command } from "../../../utils/src/cli";\nimport { commands } from "../registry";\nexport default class Acp extends Command { names = commands.length }`, {
			"packages/coding-agent/src/everything.ts": `import type { CommandEntry } from "../../utils/src/cli";\nexport const eager = (entry: CommandEntry) => entry.load();\n`,
			[ACP]: `import { Command } from "../../../utils/src/cli";\nimport "../everything";\nimport { commands } from "../registry";\nexport default class Acp extends Command { names = commands.length }`,
		});
		expect(sessions.exclusion(predictLiteral(source))).toBeUndefined();
	});

	test("an import() that is not a registry thunk is always followed", () => {
		const { source, reach: sessions } = reach(`import { Command } from "../../../utils/src/cli";\nexport default class Acp extends Command { go = () => import("../predict") }`);
		expect(sessions.exclusion(predictLiteral(source))).toBeUndefined();
	});
});
