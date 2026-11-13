import ts from "typescript";
import type { FlowHost } from "./inventory-native-flow";
import { derives } from "./inventory-native-callees";

/**
 * Where an object literal's function member runs: every place a property of
 * its name is read, narrowed by the type the literal is contextually given and
 * by whether the read property can hold a function at all. Both narrowings
 * only drop reads the checker proves are another property; an untyped read
 * (`x.name` on an unresolved receiver) stays, since it may be any implementation.
 */
export function literalMemberReach(host: FlowHost & { named(name: string): readonly ts.Node[] }, member: ts.MethodDeclaration | ts.PropertyAssignment, label: string): ts.Node[] {
	const checker = host.source.checker;
	const contextual = ts.isObjectLiteralExpression(member.parent) ? checker.getContextualType(member.parent) : undefined;
	const typedBy = (contextual ? contextual.isUnionOrIntersection() ? contextual.types : [contextual] : []).flatMap(part => (part.aliasSymbol ?? part.getSymbol())?.declarations ?? []);
	const mayHoldFunction = (declaration: ts.Declaration): boolean => {
		const type = checker.getTypeAtLocation(declaration);
		return (type.isUnionOrIntersection() ? type.types : [type]).some(part => part.getCallSignatures().length > 0 || (part.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown | ts.TypeFlags.TypeParameter)) !== 0);
	};
	return host.named(label).filter(identifier => {
		const declaration = host.source.declaration(identifier);
		if (!declaration) return ts.isPropertyAccessExpression(identifier.parent) && identifier.parent.name === identifier;
		if (declaration === member) return true;
		if (!ts.isMethodSignature(declaration) && !ts.isPropertySignature(declaration)) return false;
		const owner = declaration.parent;
		if (typedBy.length && !typedBy.some(typed => typed === owner || ts.isInterfaceDeclaration(owner) && ts.isInterfaceDeclaration(typed) && (derives(host, typed, owner) || derives(host, owner, typed)))) return false;
		return ts.isMethodSignature(declaration) || mayHoldFunction(declaration);
	});
}
