import { readFileSync } from "node:fs";
import path from "node:path";
import ts from "typescript";
import { gatedByUnsupportedKind, hostAdvertisedKinds, type HostKinds } from "./inventory-kind-gates";
import { isRegistryThunk, registryDispatchProof, type RegistryDispatchProof } from "./inventory-registry-dispatch";
import type { SourceIndex } from "./inventory-source";

/** The commands Roost starts: the browser leg runs `omp acp`, the terminal leg runs the default launch. */
const SESSION_ENTRIES = ["packages/coding-agent/src/commands/acp.ts", "packages/coding-agent/src/commands/launch.ts"];
const LAUNCHER = "crates/roost-worker/src/omp/launcher.rs";
const ROOST_ROOT = path.resolve(import.meta.dir, "../../..");

/** Why a producer site can never run for the host: its file is never loaded, or only a kind the host lacks reaches it. */
export type SiteExclusion =
	| { proof: "outside-session-import-closure"; entries: string[]; closureFiles: number; launcher: { source: string; line: number }; computedLoads: number; nameSpelledInClosure: false; registryDispatch?: RegistryDispatchProof & { deferredThunks: number } }
	| { proof: "host-gated-fallback"; kind: string; gate: { source: string; line: number; expression: string }; host: { source: string; line: number } };

interface Closure { files: Set<string>; entries: string[]; closureFiles: number; launcher: { source: string; line: number }; opaque: Array<{ source: string; line: number }>; spelled: Set<string>; registry?: RegistryDispatchProof & { deferredThunks: number }; /** Why every registry thunk was followed. */ registryUnproven?: string }

/** The first `.args(["acp"` of the Roost launcher: the evidence that no other subcommand starts a session. */
function launcherEvidence(root: string): { source: string; line: number } | undefined {
	try {
		const text = readFileSync(path.join(root, LAUNCHER), "utf8");
		const at = text.search(/\.args\(\s*\[\s*"acp"/);
		return at < 0 ? undefined : { source: LAUNCHER, line: text.slice(0, at).split("\n").length };
	} catch { return undefined; }
}

/**
 * Files the session commands can load: static imports, re-exports, `import("x")`
 * and `require("x")`. A load whose specifier is not a literal (a plugin or
 * user-supplied path) is recorded; it can reach a file only by spelling its
 * name, so every string in the closure is kept to check that.
 *
 * A registry thunk (`{ name, load: () => import("./x") }`) runs only when
 * something calls `load`. It is deferred while `registryDispatchProof` shows
 * every `load()` call in the closure sits behind an exported dispatcher no
 * session file imports; without that proof every thunk is followed.
 */
function sessionClosure(source: SourceIndex, entries: readonly string[], launcher: { source: string; line: number }): Closure | undefined {
	const attempt = (followThunks: boolean) => {
		const options = source.program.getCompilerOptions();
		const files = new Set<string>();
		const pending: ts.SourceFile[] = [];
		const visit = (file: ts.SourceFile | undefined): void => { if (file && !files.has(source.relative(file))) { files.add(source.relative(file)); pending.push(file); } };
		for (const entry of entries) visit(source.program.getSourceFile(path.join(source.root, entry)));
		if (!pending.length) return undefined;
		const opaque: Array<{ source: string; line: number }> = [];
		const spelled = new Set<string>();
		let deferredThunks = 0;
		const load = (specifier: ts.Expression, containing: ts.SourceFile, thunk: boolean): void => {
			if (!ts.isStringLiteralLike(specifier)) { opaque.push(source.origin(specifier)); return; }
			if (thunk && !followThunks) { deferredThunks++; return; }
			const resolved = ts.resolveModuleName(specifier.text, containing.fileName, options, ts.sys).resolvedModule?.resolvedFileName;
			visit(resolved ? source.program.getSourceFile(resolved) : undefined);
		};
		for (let file = pending.pop(); file; file = pending.pop()) {
			const containing = file;
			source.walk(containing, node => {
				if (ts.isStringLiteralLike(node) || ts.isTemplateHead(node) || ts.isTemplateMiddle(node) || ts.isTemplateTail(node)) spelled.add(node.text.toLowerCase());
				if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier) load(node.moduleSpecifier, containing, false);
				else if (ts.isImportEqualsDeclaration(node) && ts.isExternalModuleReference(node.moduleReference)) load(node.moduleReference.expression, containing, false);
				else if (ts.isCallExpression(node) && (node.expression.kind === ts.SyntaxKind.ImportKeyword || ts.isIdentifier(node.expression) && node.expression.text === "require") && node.arguments[0]) load(node.arguments[0], containing, isRegistryThunk(node));
			});
		}
		return { files, entries: [...entries], closureFiles: files.size, launcher, opaque, spelled, deferredThunks };
	};
	const deferred = attempt(false);
	if (!deferred) return undefined;
	if (!deferred.deferredThunks) return deferred;
	const proof = registryDispatchProof(source, deferred.files);
	if ("proven" in proof) return { ...deferred, registry: { ...proof.proven, deferredThunks: deferred.deferredThunks } };
	const followed = attempt(true);
	return followed && { ...followed, registryUnproven: proof.unproven };
}

export class SiteReach {
	readonly closure: Closure | undefined;
	private readonly spelledName = new Map<string, boolean>();

	constructor(private readonly source: SourceIndex, private readonly host: HostKinds = hostAdvertisedKinds(), entries: readonly string[] = SESSION_ENTRIES, launcher: { source: string; line: number } | null = launcherEvidence(ROOST_ROOT) ?? null) {
		this.closure = launcher ? sessionClosure(source, entries, launcher) : undefined;
	}

	/** Whether any string the session carries contains the module's name, so a computed load could assemble it. */
	private named(closure: Closure, file: string): boolean {
		const base = path.basename(file).replace(/\.[cm]?[jt]sx?$/, "").toLowerCase();
		let spelled = this.spelledName.get(file);
		if (spelled === undefined) {
			spelled = !closure.opaque.length ? false : [...closure.spelled].some(text => text.includes(base));
			this.spelledName.set(file, spelled);
		}
		return spelled;
	}

	exclusion(node: ts.Node): SiteExclusion | undefined {
		const closure = this.closure;
		const file = this.source.relative(node);
		if (closure && !closure.files.has(file) && !this.named(closure, file)) return { proof: "outside-session-import-closure", entries: closure.entries, closureFiles: closure.closureFiles, launcher: closure.launcher, computedLoads: closure.opaque.length, nameSpelledInClosure: false, ...closure.registry ? { registryDispatch: closure.registry } : {} };
		const gate = gatedByUnsupportedKind(this.source, node);
		return gate && this.host.kinds.has(gate.kind) ? { proof: "host-gated-fallback", kind: gate.kind, gate: { source: gate.source, line: gate.line, expression: gate.expression }, host: { source: this.host.source, line: this.host.line } } : undefined;
	}

	/** The exclusion of the object-literal member an emitter record points at, by its file and start offset. */
	exclusionAt(file: string, start: number): SiteExclusion | undefined {
		const sourceFile = this.source.files.find(candidate => this.source.relative(candidate) === file);
		let member: ts.Node | undefined;
		if (sourceFile) this.source.walk(sourceFile, node => { if (!member && (ts.isPropertyAssignment(node) || ts.isShorthandPropertyAssignment(node)) && node.getStart(sourceFile) === start) member = node; });
		return member && this.exclusion(member);
	}
}
