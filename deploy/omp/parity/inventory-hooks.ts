import ts from "typescript";
import { SourceIndex } from "./inventory-source";

export function resolveHook(source: SourceIndex, member: ts.Node): ts.Node {
	let implementation = member;
	const visited = new Set<ts.Node>();
	while (!visited.has(implementation)) {
		visited.add(implementation);
		if ((ts.isPropertyAssignment(implementation) || ts.isPropertyDeclaration(implementation) || ts.isVariableDeclaration(implementation)) && implementation.initializer) implementation = implementation.initializer;
		else if (ts.isShorthandPropertyAssignment(implementation)) {
			const declaration = source.checker.getShorthandAssignmentValueSymbol(implementation)?.valueDeclaration;
			if (!declaration) break;
			implementation = declaration;
		} else if (ts.isAsExpression(implementation) || ts.isSatisfiesExpression(implementation) || ts.isParenthesizedExpression(implementation) || ts.isTypeAssertionExpression(implementation) || ts.isNonNullExpression(implementation)) implementation = implementation.expression;
		else if (ts.isIdentifier(implementation) || ts.isPropertyAccessExpression(implementation)) {
			const declaration = source.declaration(ts.isPropertyAccessExpression(implementation) ? implementation.name : implementation);
			if (!declaration) break;
			implementation = declaration;
		} else break;
	}
	return implementation;
}

export interface HookShape {
	callable: boolean;
	returns: Array<{ type: string; shape: "rows" | "string" | "value" | "empty" | "unknown" }>;
}

export function hookShape(source: SourceIndex, member: ts.Node): HookShape {
	const implementation = resolveHook(source, member);
	const named = "name" in member && member.name && typeof member.name === "object" ? member.name as ts.Node : member;
	let signatures = source.checker.getTypeAtLocation(named).getCallSignatures();
	if (signatures.length === 0 && (ts.isMethodDeclaration(implementation) || ts.isFunctionDeclaration(implementation) || ts.isFunctionExpression(implementation) || ts.isArrowFunction(implementation))) {
		const signature = source.checker.getSignatureFromDeclaration(implementation);
		if (signature) signatures = [signature];
	}
	const returns: HookShape["returns"] = [];
	for (const signature of signatures) {
		const returned = source.checker.getReturnTypeOfSignature(signature);
		for (const type of returned.isUnion() ? returned.types : [returned]) {
			let shape: HookShape["returns"][number]["shape"] = "value";
			if (type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown)) shape = "unknown";
			else if (type.flags & (ts.TypeFlags.Null | ts.TypeFlags.Undefined | ts.TypeFlags.Void | ts.TypeFlags.Never)) shape = "empty";
			else if (type.flags & ts.TypeFlags.StringLike) shape = "string";
			else if (source.checker.isArrayType(type) || source.checker.isTupleType(type)) {
				const element = source.checker.getIndexTypeOfType(type, ts.IndexKind.Number);
				if (element && (element.flags & ts.TypeFlags.StringLike || element.isUnion() && element.types.every(item => !!(item.flags & ts.TypeFlags.StringLike)))) shape = "rows";
				else if (element && element.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown)) shape = "unknown";
			}
			returns.push({ type: source.checker.typeToString(type), shape });
		}
	}
	return { callable: signatures.length > 0, returns };
}

export function componentContract(source: SourceIndex, node: ts.ClassLikeDeclaration | ts.ObjectLiteralExpression): { declared: boolean; structural: boolean; declaration?: { source: string; line: number } } {
	const file = source.files.find(file => source.relative(file) === "packages/tui/src/tui.ts");
	const declaration = file?.statements.find(statement => ts.isInterfaceDeclaration(statement) && statement.name.text === "Component");
	if (!declaration || !ts.isInterfaceDeclaration(declaration)) return { declared: false, structural: false };
	const contract = source.checker.getTypeAtLocation(declaration.name);
	const visited = new Set<ts.Node>();
	const declares = (candidate: ts.Node): boolean => {
		if (candidate === declaration) return true;
		if (visited.has(candidate)) return false;
		visited.add(candidate);
		if (ts.isTypeAliasDeclaration(candidate)) {
			let found = false;
			source.walk(candidate.type, reference => {
				if (ts.isTypeReferenceNode(reference)) {
					const target = source.declaration(reference.typeName);
					if (target && declares(target)) found = true;
				}
			});
			return found;
		}
		const resolved = resolveHook(source, candidate);
		if (resolved !== candidate) return declares(resolved);
		if (!(ts.isClassDeclaration(candidate) || ts.isClassExpression(candidate) || ts.isInterfaceDeclaration(candidate))) return false;
		return (candidate.heritageClauses ?? []).some(clause => clause.types.some(type => {
			const target = source.declaration(type.expression);
			return !!target && declares(target);
		}));
	};
	const valueType = source.checker.getTypeAtLocation(node);
	const constructor = valueType.getConstructSignatures()[0];
	const type = constructor ? source.checker.getReturnTypeOfSignature(constructor) : valueType;
	const structural = !(type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown)) && source.checker.isTypeAssignableTo(type, contract);
	return { declared: declares(node), structural, declaration: source.origin(declaration) };
}

interface FallbackGuard {
	describe: ts.Node;
	render: ts.Node;
	source: string;
	line: number;
	expression: string;
}

/** Match the resolver's actual prototype-identity guard, not a class name. */
export function nativeFallbackGuard(source: SourceIndex): FallbackGuard | undefined {
	const file = source.files.find(file => source.relative(file) === "packages/tui/src/native/reconcile.ts");
	if (!file) return undefined;
	let result: FallbackGuard | undefined;
	const prototypeMember = (node: ts.Expression, name: string): ts.Node | undefined => {
		if (!ts.isPropertyAccessExpression(node) || node.name.text !== name || !ts.isPropertyAccessExpression(node.expression) || node.expression.name.text !== "prototype") return undefined;
		return source.declaration(node.name);
	};
	source.walk(file, node => {
		if (!ts.isVariableDeclaration(node) || !node.initializer || !ts.isBinaryExpression(node.initializer) || node.initializer.operatorToken.kind !== ts.SyntaxKind.AmpersandAmpersandToken) return;
		const { left, right } = node.initializer;
		if (!ts.isBinaryExpression(left) || left.operatorToken.kind !== ts.SyntaxKind.EqualsEqualsEqualsToken || !ts.isBinaryExpression(right) || right.operatorToken.kind !== ts.SyntaxKind.ExclamationEqualsEqualsToken) return;
		const describe = prototypeMember(left.right, "describe");
		const render = prototypeMember(right.right, "render");
		if (!describe || !render || describe.parent !== render.parent || !ts.isPropertyAccessExpression(right.left) || right.left.name.text !== "render") return;
		const describeVariable = source.declaration(left.left);
		if (!describeVariable || !ts.isVariableDeclaration(describeVariable) || !describeVariable.initializer || !ts.isPropertyAccessExpression(describeVariable.initializer) || describeVariable.initializer.name.text !== "describe") return;
		if (source.symbol(describeVariable.initializer.expression) !== source.symbol(right.left.expression)) return;
		let scope: ts.Node = node;
		while (scope.parent && !ts.isFunctionLike(scope)) scope = scope.parent;
		let gatesDescription = false;
		let emitsRows = false;
		source.walk(scope, child => {
			if (ts.isIfStatement(child)) source.walk(child.expression, expression => {
				if (ts.isPrefixUnaryExpression(expression) && expression.operator === ts.SyntaxKind.ExclamationToken && source.declaration(expression.operand) === node) gatesDescription = true;
			});
			if (ts.isPropertyAssignment(child) && source.name(child.name) === "k" && ts.isStringLiteralLike(child.initializer) && child.initializer.text === "rows") emitsRows = true;
		});
		if (gatesDescription && emitsRows) result = { describe, render, ...source.origin(node), expression: node.initializer.getText() };
	});
	return result;
}
