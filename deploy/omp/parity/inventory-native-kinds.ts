import ts from "typescript";
import { SourceIndex } from "./inventory-source";
import type { SourceEmitter } from "./inventory-flow";

/**
 * A canonical node factory exposes kinds only through its actual typed
 * descriptor constructor. A kind typed by the factory's own type parameter is
 * produced at each call site with that call's argument; a factory that escapes
 * as a value keeps every kind its constraint admits.
 */
export function nativeKindEmitters(source: SourceIndex): Map<string, SourceEmitter[]> {
	const result = new Map<string, SourceEmitter[]>();
	const file = source.files.find(file => source.relative(file) === "packages/tui/src/native/node.ts");
	const declaration = file && source.declarations(file, "NativeNode")[0];
	if (!declaration || !ts.isTypeAliasDeclaration(declaration)) return result;
	const checker = source.checker;
	const descriptor = checker.getTypeFromTypeNode(declaration.type);
	const parts = descriptor.isUnion() ? descriptor.types : [descriptor];
	const native = (type: ts.Type | undefined): boolean => {
		if (!type) return false;
		const value = checker.getNonNullableType(type);
		if (value === descriptor || parts.includes(value) || value.aliasSymbol?.declarations?.includes(declaration)) return true;
		// A slot like `NativeChild` admits a node beside strings; extra producers only keep obligations.
		return value.isUnion() && value.types.some(part => parts.includes(part));
	};
	const literals = (type: ts.Type, visited = new Set<ts.Type>()): string[] => {
		if (visited.has(type)) return [];
		visited.add(type);
		if (type.flags & ts.TypeFlags.StringLiteral) return [(type as ts.StringLiteralType).value];
		if (type.isUnion()) return type.types.flatMap(part => literals(part, visited));
		const constraint = checker.getBaseConstraintOfType(type);
		return constraint && constraint !== type ? literals(constraint, visited) : [];
	};
	const references = new Map<string, Map<ts.Symbol, ts.Identifier[]>>();
	const symbolOf = (identifier: ts.Identifier): ts.Symbol | undefined => {
		const target = ts.isShorthandPropertyAssignment(identifier.parent) && identifier.parent.name === identifier
			? checker.getShorthandAssignmentValueSymbol(identifier.parent) : checker.getSymbolAtLocation(identifier);
		return target && target.flags & ts.SymbolFlags.Alias ? checker.getAliasedSymbol(target) : target;
	};
	const referencesOf = (symbol: ts.Symbol, name: string): ts.Identifier[] => {
		let named = references.get(name);
		if (!named) {
			named = new Map();
			for (const candidate of source.files) source.walk(candidate, node => {
				if (!ts.isIdentifier(node) || node.text !== name) return;
				const target = symbolOf(node);
				if (target) named!.set(target, [...(named!.get(target) ?? []), node]);
			});
			references.set(name, named);
		}
		return named.get(symbol) ?? [];
	};
	const assigned = (identifier: ts.Identifier): boolean => ts.isBinaryExpression(identifier.parent) && identifier.parent.left === identifier
		&& identifier.parent.operatorToken.kind >= ts.SyntaxKind.FirstAssignment && identifier.parent.operatorToken.kind <= ts.SyntaxKind.LastAssignment;
	/** The call arguments a never-reassigned parameter receives, or undefined when its function escapes as a value. */
	const callArguments = (parameter: ts.ParameterDeclaration): ts.Expression[] | undefined => {
		const owner = parameter.parent;
		const index = owner.parameters.indexOf(parameter);
		const name = ts.isFunctionDeclaration(owner) ? owner.name : ts.isVariableDeclaration(owner.parent) && ts.isIdentifier(owner.parent.name) ? owner.parent.name : undefined;
		const symbol = name && checker.getSymbolAtLocation(name);
		const own = ts.isIdentifier(parameter.name) ? checker.getSymbolAtLocation(parameter.name) : undefined;
		if (!name || !symbol || !own || parameter.dotDotDotToken || referencesOf(own, own.name).some(assigned)) return undefined;
		const found: ts.Expression[] = [];
		for (const reference of referencesOf(symbol, name.text)) {
			// A renamed import or re-export calls the factory under a name this search does not follow.
			if ((ts.isImportSpecifier(reference.parent) || ts.isExportSpecifier(reference.parent)) && reference.parent.propertyName === reference) return undefined;
			if (reference === name || ts.isImportSpecifier(reference.parent) || ts.isImportClause(reference.parent) || ts.isExportSpecifier(reference.parent)) continue;
			const callee = ts.isPropertyAccessExpression(reference.parent) && reference.parent.name === reference ? reference.parent : reference;
			if (!ts.isCallExpression(callee.parent) || callee.parent.expression !== callee || callee.parent.arguments.some(ts.isSpreadElement)) return undefined;
			const argument = callee.parent.arguments[index];
			if (!argument) return undefined;
			found.push(argument);
		}
		return found;
	};
	const producers = (expression: ts.Expression, visited: Set<ts.Node>): Array<{ node: ts.Node; kinds: string[] }> => {
		// `described.k` re-emits the kind of a node some other producer already constructed;
		// only a value that can be nothing but a constructed node carries.
		if (ts.isPropertyAccessExpression(expression) && expression.name.text === "k") {
			const carrier = checker.getNonNullableType(checker.getTypeAtLocation(expression.expression));
			if (carrier === descriptor || parts.includes(carrier) || carrier.isUnion() && carrier.types.every(part => parts.includes(part))) return [];
		}
		const type = checker.getTypeAtLocation(expression);
		// A shorthand member reports the property's type; the parameter's own declaration carries the type parameter.
		const declared = ts.isIdentifier(expression) ? symbolOf(expression)?.valueDeclaration : undefined;
		const parameter = declared && ts.isParameter(declared) && checker.getTypeAtLocation(declared.name).flags & ts.TypeFlags.TypeParameter ? declared : undefined;
		const passed = parameter && !visited.has(parameter) ? callArguments(parameter) : undefined;
		if (!passed || !parameter) return [{ node: expression, kinds: literals(type) }];
		visited.add(parameter);
		return passed.flatMap(argument => producers(argument, visited));
	};
	for (const candidate of source.files) source.walk(candidate, node => {
		if (!ts.isObjectLiteralExpression(node) || !native(checker.getContextualType(node))) return;
		for (const member of node.properties) {
			if (!member.name || source.name(member.name) !== "k") continue;
			const expression = ts.isPropertyAssignment(member) ? member.initializer : ts.isShorthandPropertyAssignment(member) ? member.name : undefined;
			if (!expression) continue;
			for (const producer of producers(expression, new Set())) for (const kind of producer.kinds) {
				const emitters = result.get(kind) ?? [];
				emitters.push({ node: producer.node === expression ? member : producer.node, role: "typed-kind-producer" });
				result.set(kind, emitters);
			}
		}
	});
	return result;
}
