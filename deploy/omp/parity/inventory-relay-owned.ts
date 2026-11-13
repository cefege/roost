//! Replies the ACP relay answers itself. The browser never sees the question's
//! answer from its own code, so no browser case can produce one. Built from the
//! TSP frame encoders' call sites. Called by `inventory.ts`; read by
//! `coverage-lanes.ts` and `coverage.ts`.
import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";
import { record, type SourceIndex } from "./inventory-source";
import { sourceSpan, type SourceSpan } from "./inventory-setting-controls";
import { RUST_HOST, type RustHost } from "./inventory-host-audit";
import { ACP_RELAY } from "./coverage-relay";

const ENCODER_FILE = "packages/tui/src/native/encode.ts";
const ENCODERS = ["encodeTspMessage", "encodeTspJson"];
const REPLY_VERBS = ["r", "e"];

/** One call that can write a reply frame, with the tag it writes. */
interface RelayWrite { at: SourceSpan; tag: string }
interface RelayProof { encoders: string[]; calls: number; writes: RelayWrite[]; verbDeclaration: SourceSpan }

const stringLiterals = (type: ts.Type): string[] | undefined => {
	const literals = (type.isUnion() ? type.types : [type]).flatMap(part => part.isStringLiteral() ? [part.value] : []);
	return literals.length === (type.isUnion() ? type.types.length : 1) ? literals : undefined;
};

/**
 * Every call of a frame encoder whose verb can be `r` or `e`, or is not a literal union at all. Returns the
 * writes and the full call count when each such call sits in the ACP relay and spells its reply tag as
 * `{ r: "<tag>" }`; otherwise undefined, because another writer could exist.
 */
function relayWrites(source: SourceIndex): RelayProof | undefined {
	const encoder = source.files.find(file => source.relative(file) === ENCODER_FILE);
	if (!encoder) return undefined;
	const declarations = new Set<ts.FunctionDeclaration>();
	for (const statement of encoder.statements) if (ts.isFunctionDeclaration(statement) && statement.name && ENCODERS.includes(statement.name.text)) declarations.add(statement);
	const verbParameters = new Set([...declarations].flatMap(declaration => declaration.parameters.slice(0, 1).map(parameter => source.symbol(parameter.name))));
	const writes: RelayWrite[] = [];
	let calls = 0;
	let failed = false;
	for (const file of source.files) source.walk(file, node => {
		if (!ts.isCallExpression(node) || failed) return;
		const callee = source.checker.getResolvedSignature(node)?.declaration;
		if (!callee || !ts.isFunctionDeclaration(callee) || !declarations.has(callee)) return;
		const verb = node.arguments[0];
		if (!verb) { failed = true; return; }
		// An encoder forwarding its own verb parameter is accounted for at its callers.
		if (ts.isIdentifier(verb) && verbParameters.has(source.symbol(verb))) return;
		calls++;
		const verbs = stringLiterals(source.checker.getTypeAtLocation(verb));
		if (verbs && !verbs.some(item => REPLY_VERBS.includes(item))) return;
		const relative = source.relative(file);
		const body = node.arguments[1];
		const literal = body && ts.isObjectLiteralExpression(body) ? body : undefined;
		const tag = literal?.properties.flatMap(property => ts.isPropertyAssignment(property) && source.name(property.name) === "r" && ts.isStringLiteralLike(property.initializer) ? [property.initializer.text] : [])[0];
		if (!relative.startsWith(ACP_RELAY) || !verbs || !verbs.includes("r") || verbs.length !== 1 || !tag) failed = true;
		else writes.push({ at: sourceSpan(source, node), tag });
	});
	const verbDeclaration = verbSpan(source);
	return failed || !writes.length || !verbDeclaration ? undefined : { encoders: ENCODERS, calls, writes, verbDeclaration };
}

/** The declaration of the verbs OMP's own transport can send. */
function verbSpan(source: SourceIndex): SourceSpan | undefined {
	const backend = source.files.find(file => source.relative(file) === "packages/tui/src/native/backend.ts");
	let found: SourceSpan | undefined;
	if (backend) source.walk(backend, node => { if (ts.isInterfaceDeclaration(node) && node.name.text === "NativeTransportMessage") found = sourceSpan(source, node); });
	return found;
}

/**
 * Mark the replies only the ACP relay writes: the host's own source never spells the tag, and every encoder call
 * that could write a reply frame is in the relay. The tag's variant, its exclusive members and its value in the
 * shared discriminator are relay-owned, never browser-produced.
 */
export function classifyRelayOwnedReplies(entries: InventoryEntry[], source: SourceIndex, host: RustHost): void {
	const proof = relayWrites(source);
	if (!proof) return;
	for (const tag of new Set(proof.writes.map(write => write.tag))) {
		if (host.keys.has(tag)) continue;
		const reply = entries.find(entry => entry.category === "reply" && entry.id === `reply:${tag}`);
		if (!reply) continue;
		const owned = { ...proof, writes: proof.writes.filter(write => write.tag === tag), tag };
		reply.details.exposure = "unproduced";
		delete reply.details.uiEmitters;
		reply.details.unproducedProof = {
			key: tag, declarations: [reply.details.exposureProof], searchedFiles: host.files, proof: "relay-owned-reply", relay: owned,
			host: { rust: RUST_HOST, writeForms: ["string-literal"] },
		};
		for (const entry of entries) {
			if (entry.category !== "property" || entry.details.declaringType !== "TspReply" || entry.details.exposure !== "native-contract" || !Array.isArray(entry.details.declarations)) continue;
			const variants = entry.details.declarations.flatMap(declaration => record(declaration) && Array.isArray(declaration.variants) ? declaration.variants.map(String) : [undefined]);
			if (variants.length && variants.every(variant => variant === `r:${tag}`)) {
				entry.details.exposure = "unproduced";
				entry.details.unproducedProof = { key: String(entry.details.path), declarations: entry.details.declarations, searchedFiles: host.files, proof: "parent-never-written", parent: reply.id };
			} else if (entry.details.path === "r" && Array.isArray(entry.details.enumValues) && entry.details.enumValues.includes(tag)) {
				entry.details.relayOwnedValues = { values: [tag], proof: "relay-owned-reply", reply: reply.id, relay: owned };
			}
		}
	}
}
