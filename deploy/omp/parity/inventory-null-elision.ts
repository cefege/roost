import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";
import { record, SourceIndex } from "./inventory-source";
import { sourceSpan } from "./inventory-setting-controls";

const RECONCILE = "packages/tui/src/native/reconcile.ts";
const APPLY = "packages/tui/src/native/apply.ts";
const TOP_LEVEL_KEY = /^[A-Za-z_$][\w$]*$/;

/** The statement `if (v !== undefined && v !== null) …` and the object it writes `v` into. */
function nullDroppingGuards(source: SourceIndex, relative: string): Array<{ node: ts.IfStatement; target: string }> {
	const found: Array<{ node: ts.IfStatement; target: string }> = [];
	source.walk(source.file(relative), node => {
		if (!ts.isIfStatement(node) || !ts.isBinaryExpression(node.expression) || node.expression.operatorToken.kind !== ts.SyntaxKind.AmpersandAmpersandToken) return;
		const clauses = [node.expression.left, node.expression.right].filter(ts.isBinaryExpression);
		const kinds = clauses.map(clause => clause.operatorToken.kind === ts.SyntaxKind.ExclamationEqualsEqualsToken ? clause.right.getText() : "");
		if (clauses.length !== 2 || !kinds.includes("undefined") || !kinds.includes("null") || clauses[0]!.left.getText() !== clauses[1]!.left.getText()) return;
		const value = clauses[0]!.left.getText();
		source.walk(node.thenStatement, inner => {
			if (ts.isBinaryExpression(inner) && inner.operatorToken.kind === ts.SyntaxKind.EqualsToken && ts.isElementAccessExpression(inner.left) && inner.right.getText() === value) found.push({ node, target: inner.left.getText() });
		});
	});
	return found;
}

/** `if (v === null) delete node.p[key]`: a `set` op carrying null clears the key instead of storing it. */
function nullClearingGuards(source: SourceIndex, relative: string): ts.IfStatement[] {
	const found: ts.IfStatement[] = [];
	source.walk(source.file(relative), node => {
		if (!ts.isIfStatement(node) || !ts.isBinaryExpression(node.expression) || node.expression.operatorToken.kind !== ts.SyntaxKind.EqualsEqualsEqualsToken || node.expression.right.getText() !== "null") return;
		source.walk(node.thenStatement, inner => { if (ts.isDeleteExpression(inner) && ts.isElementAccessExpression(inner.expression)) found.push(node); });
	});
	return found;
}

/** The document is built and patched only through these three writes; each drops or deletes a top-level null prop. */
function nullAbsenceProof(source: SourceIndex): Record<string, unknown> | undefined {
	try {
		const wire = nullDroppingGuards(source, RECONCILE).filter(guard => /\.p\b|\bp\b/.test(guard.target));
		const build = nullDroppingGuards(source, APPLY).filter(guard => /\.p\b/.test(guard.target));
		const set = nullClearingGuards(source, APPLY).filter(guard => /\.p\b/.test(guard.thenStatement.getText()));
		if (wire.length !== 1 || build.length !== 1 || set.length !== 1) return undefined;
		return {
			producerWire: sourceSpan(source, wire[0]!.node),
			addBuild: sourceSpan(source, build[0]!.node),
			setClear: sourceSpan(source, set[0]!),
		};
	} catch { return undefined; }
}

/**
 * A top-level element prop typed `… | null` never holds null in the document:
 * the producer's node builder omits it, `add` skips it and `set` deletes the
 * key. Null is then the absence of the prop, not a value any adapter can
 * show, so it leaves the value domain only with that located source proof.
 * Nested positions (an array slot, an event value, a `set`'s own payload) keep it.
 */
export function elideUnstorableNull(source: SourceIndex, entries: InventoryEntry[], propsInterfaces: ReadonlySet<string>): void {
	let proof: Record<string, unknown> | undefined;
	for (const entry of entries) {
		if (entry.category !== "property" || entry.details.exposure !== "ui" || !Array.isArray(entry.details.enumValues) || !entry.details.enumValues.includes(null)) continue;
		const owner = entry.details.declaringType, key = entry.details.path;
		if (typeof owner !== "string" || !propsInterfaces.has(owner) || typeof key !== "string" || !TOP_LEVEL_KEY.test(key)) continue;
		proof ??= nullAbsenceProof(source) ?? {};
		if (!Object.keys(proof).length) return;
		entry.details.enumValues = entry.details.enumValues.filter(value => value !== null);
		const domain = record(entry.details.enumDomain) ? entry.details.enumDomain : {};
		entry.details.enumDomain = { ...domain, values: entry.details.enumValues, excludedValues: [{ value: null, reason: "top-level-null-is-absent-prop", proof }] };
	}
}
