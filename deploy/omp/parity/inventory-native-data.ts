import ts from "typescript";

/**
 * Whether a value of this static type can hold a component: it cannot when it
 * is built only of primitives, arrays, promises, maps, sets and plain records
 * of such, with no function and no `any`/`unknown`/type-parameter slot that a
 * component could sit in. Called on the type of a node the flow reaches, so a
 * request's `Promise<string | undefined>` ends the trace there.
 */
const PRIMITIVE_FLAGS = ts.TypeFlags.StringLike | ts.TypeFlags.NumberLike | ts.TypeFlags.BooleanLike | ts.TypeFlags.BigIntLike | ts.TypeFlags.Void | ts.TypeFlags.Undefined | ts.TypeFlags.Null | ts.TypeFlags.Never | ts.TypeFlags.EnumLike;
const CONTAINERS: Record<string, true> = { Array: true, ReadonlyArray: true, Promise: true, PromiseLike: true, Map: true, ReadonlyMap: true, Set: true, ReadonlySet: true };
const MAX_DEPTH = 6;

export function carriesOnlyData(checker: ts.TypeChecker, type: ts.Type, visiting: Set<ts.Type> = new Set(), depth = 0): boolean {
	if (type.flags & PRIMITIVE_FLAGS) return true;
	if (type.isUnion()) return type.types.every(member => carriesOnlyData(checker, member, visiting, depth));
	if (!(type.flags & ts.TypeFlags.Object) || depth > MAX_DEPTH) return false;
	// A type already being examined adds nothing: its members are checked by the outer visit.
	if (visiting.has(type)) return true;
	visiting.add(type);
	const object = type as ts.ObjectType;
	if (type.getCallSignatures().length || type.getConstructSignatures().length) return false;
	const reference = object.objectFlags & ts.ObjectFlags.Reference ? (type as ts.TypeReference) : undefined;
	if (reference) {
		const name = reference.target.symbol?.name;
		if (name && CONTAINERS[name] || checker.isTupleType(type) || checker.isArrayType(type)) return checker.getTypeArguments(reference).every(argument => carriesOnlyData(checker, argument, visiting, depth + 1));
	}
	// A class instance carries behaviour and hidden state a plain record does not; only structural records qualify.
	if (type.symbol && type.symbol.flags & ts.SymbolFlags.Class) return false;
	if (checker.getIndexInfosOfType(type).some(info => !carriesOnlyData(checker, info.type, visiting, depth + 1))) return false;
	return checker.getPropertiesOfType(type).every(property => {
		const declaration = property.valueDeclaration;
		if (!declaration) return false;
		return carriesOnlyData(checker, checker.getTypeOfSymbolAtLocation(property, declaration), visiting, depth + 1);
	});
}

const FUNCTION_MEMBERS = ["apply", "call", "bind", "toString", "prototype", "length", "arguments", "caller", "name"];

/**
 * A type no function value can inhabit: a primitive, or an object whose required
 * members a function does not have. Only a program that type-checks makes this
 * sound, so `any`, `unknown`, `object`, type parameters and weak types never qualify.
 */
export function cannotHoldFunction(checker: ts.TypeChecker, type: ts.Type): boolean {
	if (type.flags & PRIMITIVE_FLAGS) return true;
	if (type.isUnion()) return type.types.every(member => cannotHoldFunction(checker, member));
	if (type.isIntersection()) return type.types.some(member => cannotHoldFunction(checker, member));
	if (!(type.flags & ts.TypeFlags.Object) || type.getCallSignatures().length || type.getConstructSignatures().length) return false;
	return checker.getPropertiesOfType(type).some(property => !(property.flags & ts.SymbolFlags.Optional) && !FUNCTION_MEMBERS.includes(property.name));
}
