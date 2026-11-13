import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";
import { record, type SourceIndex } from "./inventory-source";
import { sourceSpan, type SourceSpan } from "./inventory-setting-controls";

/**
 * Inbound wire members (`TspEvent`, `TspReply`) are written by the host, not by
 * OMP. In Roost the host is the browser crates plus OMP's ACP bridge, which turns
 * the browser's open parameters into the hello reply. A member neither writes
 * cannot reach OMP from Roost, so no browser case can produce it.
 */
const INBOUND = new Set(["TspEvent", "TspReply"]);
export const RUST_HOST = ["crates/roost-web/src", "crates/roost-client-core/src"];
const FORK_BRIDGE = "packages/coding-agent/src/modes/acp/";
export const ROOST_ROOT = path.resolve(import.meta.dir, "../../..");

export interface RustHost { keys: Set<string>; files: number }

export function rustFiles(directory: string): string[] {
	return readdirSync(directory, { withFileTypes: true }).flatMap(item => {
		const full = path.join(directory, item.name);
		return item.isDirectory() ? rustFiles(full) : item.name.endsWith(".rs") ? [full] : [];
	});
}

const camel = (name: string) => name.replace(/_([a-z0-9])/g, (_, next: string) => next.toUpperCase());

/** The braced body that starts at `open`, nested braces included. */
function body(text: string, open: number): string {
	let depth = 0;
	for (let index = open; index < text.length; index++) {
		if (text[index] === "{") depth++;
		else if (text[index] === "}" && --depth === 0) return text.slice(open + 1, index);
	}
	return text.slice(open + 1);
}

/**
 * Every JSON key the Rust host can emit: any string literal (`json!` keys,
 * serde renames, map inserts) and every field of a `Serialize` type under both
 * its own and its camelCase name, since `rename_all` may apply.
 */
export function rustHostKeys(root = ROOST_ROOT): RustHost {
	const keys = new Set<string>();
	const files = RUST_HOST.flatMap(directory => rustFiles(path.join(root, directory)));
	for (const file of files) {
		const text = readFileSync(file, "utf8");
		for (const match of text.matchAll(/"((?:[^"\\]|\\.)*)"/g)) keys.add(match[1]!);
		for (const match of text.matchAll(/#\[derive\(([^)]*)\)\]((?:\s*#\[[^\]]*\])*)\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum)\s+\w+[^{;]*\{/g)) {
			if (!/\bSerialize\b/.test(match[1]!)) continue;
			for (const field of body(text, match.index! + match[0].length - 1).matchAll(/\b([a-z_][a-z0-9_]*)\s*:(?!:)/g)) {
				keys.add(field[1]!);
				keys.add(camel(field[1]!));
			}
		}
	}
	return { keys, files: files.length };
}

const location = (emitter: unknown) => record(emitter) && typeof emitter.source === "string" ? `${emitter.source}:${String(emitter.line)}` : undefined;

/** Mark inbound members, and their nested fields, that no Roost host source writes. */
export function classifyHostUnproduced(entries: InventoryEntry[], host: RustHost = rustHostKeys()): void {
	const unproduced = new Map<string, InventoryEntry>();
	const candidates = entries
		.filter(entry => entry.category === "property" && entry.details.exposure === "native-contract"
			&& typeof entry.details.declaringType === "string" && INBOUND.has(entry.details.declaringType) && typeof entry.details.path === "string")
		.sort((left, right) => String(left.details.path).length - String(right.details.path).length);
	for (const entry of candidates) {
		const [top, ...rest] = String(entry.details.path).split(".");
		const leaf = rest.at(-1) ?? top!;
		const parent = rest.length ? unproduced.get(`property:${entry.details.declaringType}.${String(entry.details.path).split(".").slice(0, -1).join(".")}`) : undefined;
		const emitters = Array.isArray(entry.details.emitters) ? entry.details.emitters : [];
		const writes = emitters.filter(emitter => record(emitter) && emitter.role !== "typed-property-read");
		if (parent) {
			entry.details.exposure = "unproduced";
			entry.details.unproducedProof = { key: leaf, declarations: entry.details.declarations, searchedFiles: Number((parent.details.unproducedProof as Record<string, unknown>).searchedFiles), proof: "parent-never-written", parent: parent.id };
			unproduced.set(entry.id, entry);
			continue;
		}
		if (rest.length || host.keys.has(top!) || writes.some(emitter => String((emitter as Record<string, unknown>).source).startsWith(FORK_BRIDGE))) continue;
		entry.details.exposure = "unproduced";
		entry.details.unproducedProof = {
			key: leaf, declarations: entry.details.declarations, searchedFiles: host.files, proof: "host-never-writes",
			host: { rust: RUST_HOST, forkBridge: FORK_BRIDGE, writeForms: ["string-literal", "serialize-field", "serialize-field-camel-case"] },
			// Fork writes outside the bridge serve OMP's own terminal transport, which Roost does not run.
			nonHostWrites: writes.flatMap(emitter => location(emitter) ?? []),
		};
		unproduced.set(entry.id, entry);
	}
	for (const entry of entries) {
		const tag = record(entry.details.discriminator) ? entry.details.discriminator.value : undefined;
		if (entry.category !== "event" || entry.details.exposure !== "native-contract" || typeof tag !== "string" || host.keys.has(tag)) continue;
		const emitters = Array.isArray(entry.details.emitters) ? entry.details.emitters : [];
		if (emitters.some(emitter => record(emitter) && emitter.role !== "typed-variant-consumer" && String(emitter.source).startsWith(FORK_BRIDGE))) continue;
		entry.details.exposure = "unproduced";
		entry.details.unproducedProof = {
			key: tag, declarations: [entry.details.exposureProof], searchedFiles: host.files, proof: "host-never-writes",
			host: { rust: RUST_HOST, forkBridge: FORK_BRIDGE, writeForms: ["string-literal"] },
		};
	}
}

/**
 * Whether a tag string written as a value could be (the head of) an operation tuple.
 * Only an expression nothing types (no contextual type, `any`, `unknown` or a type
 * parameter), one whose contextual type is exactly the tag outside an object property,
 * or the first element of a tuple-typed array can become one without a cast the
 * producer scan already sees; a string typed as `string` or as another union cannot,
 * and a JSON resource's strings are always `string`.
 */
function couldBuildOperation(source: SourceIndex, node: ts.StringLiteralLike): boolean {
	if (node.getSourceFile().fileName.endsWith(".json")) return false;
	const open = (type: ts.Type | undefined): boolean => !type || (type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown | ts.TypeFlags.TypeParameter)) !== 0 || type.isUnion() && type.types.some(open);
	const contextual = source.checker.getContextualType(node);
	if (open(contextual)) return true;
	// A generic call's inferred parameter is exactly the tag, and returns it unchanged; only an object property of that literal type is a different record.
	if (contextual && contextual.isStringLiteral() && contextual.value === node.text && !ts.isPropertyAssignment(node.parent)) return true;
	const array = ts.isArrayLiteralExpression(node.parent) && node.parent.elements[0] === node ? source.checker.getContextualType(node.parent) : undefined;
	return array !== undefined && (array.isUnion() ? array.types : [array]).some(member => source.checker.isTupleType(member));
}

/**
 * An outbound operation OMP only consumes is unemitted when the contextual-type
 * producer scan found no producer and its tag string occurs nowhere outside the
 * schema except as a `case` label, an equality operand or a value typed as
 * something that cannot be the tuple (`typedElsewhere`, listed in the proof).
 */
export function classifyUnemittedOperations(source: SourceIndex, entries: InventoryEntry[]): void {
	const candidates = entries.filter(entry => entry.category === "operation" && entry.details.exposure === "native-contract"
		&& record(entry.details.discriminator) && typeof entry.details.discriminator.value === "string"
		&& (Array.isArray(entry.details.emitters) ? entry.details.emitters : []).every(emitter => record(emitter) && emitter.role === "typed-variant-consumer"));
	if (!candidates.length) return;
	const tags = new Map(candidates.map(entry => [String((entry.details.discriminator as Record<string, unknown>).value), { entry, uses: [] as SourceSpan[], typedElsewhere: [] as SourceSpan[], blocked: false }]));
	let searched = 0;
	for (const file of source.files) {
		if (source.relative(file) === "packages/wire/src/tsp.ts") continue;
		searched++;
		source.walk(file, node => {
			if (!ts.isStringLiteralLike(node)) return;
			const tag = tags.get(node.text);
			if (!tag) return;
			const parent = node.parent;
			const compared = ts.isCaseClause(parent) && parent.expression === node
				|| ts.isBinaryExpression(parent) && [ts.SyntaxKind.EqualsEqualsToken, ts.SyntaxKind.EqualsEqualsEqualsToken, ts.SyntaxKind.ExclamationEqualsToken, ts.SyntaxKind.ExclamationEqualsEqualsToken].includes(parent.operatorToken.kind);
			if (compared) tag.uses.push(sourceSpan(source, node));
			else if (ts.isLiteralTypeNode(parent)) return;
			else if (couldBuildOperation(source, node)) tag.blocked = true;
			else tag.typedElsewhere.push(sourceSpan(source, node));
		});
	}
	for (const [value, tag] of tags) {
		if (tag.blocked) continue;
		tag.entry.details.exposure = "unproduced";
		tag.entry.details.unproducedProof = { key: value, declarations: [tag.entry.details.exposureProof], searchedFiles: searched, excluded: "packages/wire/src/tsp.ts", proof: "tag-only-compared", comparisons: tag.uses, typedElsewhere: tag.typedElsewhere };
	}
}
