import ts from "typescript";
import type { SourceIndex } from "./inventory-source";

/**
 * Whether a hook's return type is built from native descriptor types
 * (`Native*` / `Tsp*`), so a `describe`-named member that only returns plain
 * data is told apart from a native hook. Called by `inventory-components.ts`.
 */
const DESCRIPTOR_TYPE = /^(?:Native|Tsp)[A-Z]/;
const MAX_DEPTH = 3;

function mentionsDescriptor(checker: ts.TypeChecker, type: ts.Type, depth: number, seen: Set<ts.Type>): boolean {
	if (seen.has(type)) return false;
	seen.add(type);
	const names = [type.aliasSymbol?.name, type.getSymbol()?.name];
	if (names.some(name => name !== undefined && DESCRIPTOR_TYPE.test(name))) return true;
	if (type.isUnionOrIntersection()) return type.types.some(member => mentionsDescriptor(checker, member, depth, seen));
	if (depth >= MAX_DEPTH) return false;
	const reference = (type.flags & ts.TypeFlags.Object) !== 0 && ((type as ts.ObjectType).objectFlags & ts.ObjectFlags.Reference) !== 0;
	const nested = reference ? [...checker.getTypeArguments(type as ts.TypeReference)] : [];
	for (const property of checker.getPropertiesOfType(type)) nested.push(checker.getTypeOfSymbol(property));
	return nested.some(member => mentionsDescriptor(checker, member, depth + 1, seen));
}

export function returnsNativeDescriptor(source: SourceIndex, member: ts.Node): boolean {
	const named = "name" in member && member.name && typeof member.name === "object" ? member.name as ts.Node : member;
	const signatures = source.checker.getTypeAtLocation(named).getCallSignatures();
	return signatures.some(signature => mentionsDescriptor(source.checker, source.checker.getReturnTypeOfSignature(signature), 0, new Set()));
}
