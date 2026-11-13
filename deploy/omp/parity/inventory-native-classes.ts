import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";
import type { Origin, SourceIndex } from "./inventory-source";
import type { AbsenceRejection } from "./inventory-native-types";
import { INPUT_HOOK } from "./inventory-native-members";

/**
 * Proofs for components the browser never describes because source never asks:
 * composer styles are only rendered by native chrome, and gallery fixtures sit
 * behind a CLI subcommand the Roost and parity launchers never pass.
 */
const ROOST_ROOT = path.resolve(import.meta.dir, "../../..");
const COMPOSER_HOOKS: Record<string, true> = { renderTop: true, renderRow: true, renderBottom: true };
const LAUNCHERS = [
	{ source: "crates/roost-worker/src/omp/launcher.rs", needle: '.args(["acp"', role: "roost-worker" },
	{ source: "deploy/omp/parity/profile.ts", needle: "command: [launcher, ", role: "parity-terminal-leg" },
];

export interface RoostAnchor extends Origin { repository: "roost"; role: string; text: string }
export interface ChromeCall extends Origin { hook: string }
export interface ComposerStyleProof { class: "composer-style"; chrome: ChromeCall[] }
export interface ImportEdge extends Origin { importer: string; imported: string; dynamic: boolean }
export interface CliUnreachableProof {
	class: "cli-unreachable";
	edges: ImportEdge[];
	registration: Origin & { command: string; module: string };
	launchers: RoostAnchor[];
}

function terminalHookNames(entry: InventoryEntry): string[] {
	return Array.isArray(entry.details.terminalHooks) ? entry.details.terminalHooks.map(String) : [];
}

/** A style literal is described by native chrome iff `describeComposerChrome` calls each of its hooks on a `ComposerStyle`. */
export function composerStyleProof(source: SourceIndex, entry: InventoryEntry, node: ts.Node): ComposerStyleProof | AbsenceRejection {
	if (!ts.isObjectLiteralExpression(node)) return { reason: "not an object-literal composer style" };
	const hooks = terminalHookNames(entry);
	if (!hooks.length || !hooks.every(name => Object.hasOwn(COMPOSER_HOOKS, name)) || Array.isArray(entry.details.nativeHooks) && entry.details.nativeHooks.length) return { reason: "hooks are not exactly the composer-style render hooks" };
	const contextual = source.checker.getContextualType(node)?.getSymbol();
	if (!contextual) return { reason: "style literal has no contextual interface type", origin: source.origin(node) };
	const chrome: ChromeCall[] = [];
	for (const file of source.files) {
		if (!file.fileName.endsWith("/composer/native-chrome.ts")) continue;
		source.walk(file, candidate => {
			if (!ts.isCallExpression(candidate) || !ts.isPropertyAccessExpression(candidate.expression) || !Object.hasOwn(COMPOSER_HOOKS, candidate.expression.name.text)) return;
			if (source.checker.getTypeAtLocation(candidate.expression.expression).getSymbol() !== contextual) return;
			chrome.push({ hook: candidate.expression.name.text, ...source.origin(candidate) });
		});
	}
	const missing = hooks.filter(name => !chrome.some(call => call.hook === name));
	return missing.length ? { reason: `native chrome never calls style hook ${missing.join(", ")}`, origin: source.origin(node) } : { class: "composer-style", chrome };
}

interface Registration { command: string; module: string; origin: Origin; endLine: number }

class ImportGraph {
	private readonly importers = new Map<string, ImportEdge[]>();
	private readonly registrations = new Map<string, Registration[]>();

	constructor(private readonly source: SourceIndex) {
		const options = source.program.getCompilerOptions();
		for (const file of source.files) {
			source.walk(file, node => {
				const specifier = ts.isImportDeclaration(node) || ts.isExportDeclaration(node) ? node.moduleSpecifier
					: ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword ? node.arguments[0] : undefined;
				if (!specifier || !ts.isStringLiteralLike(specifier)) return;
				const resolved = ts.resolveModuleName(specifier.text, file.fileName, options, ts.sys).resolvedModule?.resolvedFileName;
				if (!resolved) return;
				const imported = path.relative(source.root, resolved).split(path.sep).join("/");
				const edge: ImportEdge = { importer: source.relative(file), imported, dynamic: ts.isCallExpression(node), ...source.origin(node) };
				this.importers.set(imported, [...this.importers.get(imported) ?? [], edge]);
				const registration = ts.isCallExpression(node) ? this.registrationOf(node, imported) : undefined;
				if (registration) this.registrations.set(imported, [...this.registrations.get(imported) ?? [], registration]);
			});
		}
	}

	/** `{ name: "gallery", load: () => import("./commands/gallery") }`: the lazy command-table row. */
	private registrationOf(call: ts.CallExpression, module: string): Registration | undefined {
		const literal = ts.findAncestor(call, ts.isObjectLiteralExpression);
		if (!literal) return undefined;
		const property = (key: string) => literal.properties.find((candidate): candidate is ts.PropertyAssignment => ts.isPropertyAssignment(candidate) && ts.isIdentifier(candidate.name) && candidate.name.text === key);
		const name = property("name");
		const loader = property("load");
		if (!name || !ts.isStringLiteralLike(name.initializer) || !loader || !ts.findAncestor(call, node => node === loader)) return undefined;
		const origin = this.source.origin(literal);
		return { command: name.initializer.text, module, origin, endLine: this.source.origin(literal.getLastToken() ?? literal).line };
	}

	/** Importers of `start`, transitively, stopping at command-table rows; fails when any file has no importer or a non-table root. */
	closure(start: string): { edges: ImportEdge[]; registration: Registration } | AbsenceRejection {
		const edges: ImportEdge[] = [];
		const seen = new Set<string>([start]);
		const queue = [start];
		const registrations: Registration[] = [];
		while (queue.length) {
			const current = queue.pop()!;
			const incoming = this.importers.get(current) ?? [];
			const registered = this.registrations.get(current) ?? [];
			if (!incoming.length) return { reason: `${current} has no importer, so it may itself be an entry point` };
			for (const edge of incoming) {
				if (registered.some(row => row.origin.source === edge.source && row.origin.line <= edge.line && edge.line <= row.endLine)) continue;
				edges.push(edge);
				if (!seen.has(edge.importer)) { seen.add(edge.importer); queue.push(edge.importer); }
			}
			registrations.push(...registered);
		}
		const commands = new Set(registrations.map(row => `${row.command}:${row.module}`));
		if (commands.size !== 1) return { reason: `fixture is reached through ${commands.size} command-table rows, expected exactly one` };
		return { edges, registration: registrations[0]! };
	}
}

const launcherAnchors = (): RoostAnchor[] | AbsenceRejection => {
	const anchors: RoostAnchor[] = [];
	for (const launcher of LAUNCHERS) {
		const filename = path.join(ROOST_ROOT, launcher.source);
		if (!existsSync(filename)) return { reason: `launcher source ${launcher.source} is missing` };
		const lines = readFileSync(filename, "utf8").split("\n");
		const index = lines.findIndex(line => line.includes(launcher.needle));
		if (index < 0) return { reason: `launcher ${launcher.source} no longer contains ${launcher.needle}` };
		anchors.push({ repository: "roost", source: launcher.source, line: index + 1, role: launcher.role, text: lines[index]!.trim() });
	}
	return anchors;
};

/** A fixture module is reachable only through one lazily loaded CLI subcommand that neither launcher passes. */
export function cliUnreachableProof(graph: ImportGraphHandle, entry: InventoryEntry): CliUnreachableProof | AbsenceRejection {
	const closure = graph.closure(entry.source);
	if ("reason" in closure) return closure;
	const launchers = launcherAnchors();
	if (!Array.isArray(launchers)) return launchers;
	const subcommand = closure.registration.command;
	for (const anchor of launchers) if (anchor.text.includes(`"${subcommand}"`)) return { reason: `a launcher passes the ${subcommand} subcommand`, origin: anchor };
	return { class: "cli-unreachable", edges: closure.edges, registration: { ...closure.registration.origin, command: subcommand, module: closure.registration.module }, launchers };
}

export type ImportGraphHandle = Pick<ImportGraph, "closure">;
export const importGraph = (source: SourceIndex): ImportGraphHandle => new ImportGraph(source);

export interface InputListenerProof { class: "input-listener"; hooks: (Origin & { name: string })[] }

/**
 * A component with input hooks and nothing to paint is a key listener the TUI feeds from stdin and
 * the ACP session feeds through `handleRemoteInput`; no render or describe hook exists to witness.
 */
export function inputListenerProof(entry: InventoryEntry): InputListenerProof | AbsenceRejection {
	const hooks = Array.isArray(entry.details.hooks) ? entry.details.hooks : [];
	const located = hooks.flatMap(hook => {
		const implementation = hook?.implementation;
		return typeof hook?.name === "string" && typeof implementation?.source === "string" && typeof implementation.line === "number" ? [{ name: hook.name, source: implementation.source, line: implementation.line }] : [];
	});
	if (!hooks.length || located.length !== hooks.length) return { reason: "a hook has no located implementation" };
	if (!located.every(hook => INPUT_HOOK.test(hook.name))) return { reason: "the component declares a hook that is not an input hook" };
	return { class: "input-listener", hooks: located };
}
