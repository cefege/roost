import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";
import { record, SourceIndex } from "./inventory-source";
import type { SourceEmitter } from "./inventory-flow";

/** Resolve each transport variant against actual contextual wire types, not matching string literals alone. */
export function nativeVariantEmitters(source: SourceIndex, entries: readonly InventoryEntry[]): Map<string, SourceEmitter[]> {
	const wire = source.file("packages/wire/src/tsp.ts");
	const variants = entries.flatMap(entry => {
		if (!["event", "query", "reply", "operation", "verb"].includes(entry.category) || !record(entry.details.discriminator) || typeof entry.details.declaringType !== "string") return [];
		const declaration = wire.statements.find(statement => ts.isTypeAliasDeclaration(statement) && statement.name.text === entry.details.declaringType);
		if (!declaration || !ts.isTypeAliasDeclaration(declaration)) return [];
		const type = source.checker.getTypeFromTypeNode(declaration.type);
		return [{ entry, declaration, type, parts: type.isUnion() ? type.types : [type], discriminator: entry.details.discriminator }];
	});
	const result = new Map<string, SourceEmitter[]>();
	const accepts = (type: ts.Type | undefined, variant: typeof variants[number]): boolean => {
		if (!type || type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown)) return false;
		if (type === variant.type || variant.parts.includes(type) || type.aliasSymbol?.declarations?.includes(variant.declaration)) return true;
		return type.isUnionOrIntersection() && type.types.some(part => accepts(part, variant));
	};
	const values = (expression: ts.Expression): string[] => {
		const value = source.value(expression);
		if (typeof value === "string") return [value];
		const type = source.checker.getTypeAtLocation(expression);
		return (type.isUnion() ? type.types : [type]).flatMap(part => part.flags & ts.TypeFlags.StringLiteral ? [(part as ts.StringLiteralType).value] : []);
	};
	const add = (entry: InventoryEntry, node: ts.Node, role: string): void => {
		const emitters = result.get(entry.id) ?? [];
		if (!emitters.some(emitter => emitter.node === node && emitter.role === role)) emitters.push({ node, role });
		result.set(entry.id, emitters);
	};
	const receiver = (expression: ts.Expression, variant: typeof variants[number]): ts.Expression | undefined => ts.isPropertyAccessExpression(expression) && expression.name.text === variant.discriminator.key ? expression.expression : ts.isElementAccessExpression(expression) && ts.isNumericLiteral(expression.argumentExpression) && Number(expression.argumentExpression.text) === variant.discriminator.index ? expression.expression : variant.entry.category === "verb" ? expression : undefined;
	for (const file of source.files) if (source.relative(file) !== "packages/wire/src/tsp.ts") source.walk(file, node => {
		if (ts.isObjectLiteralExpression(node) || ts.isArrayLiteralExpression(node) || ts.isStringLiteralLike(node)) {
			const context = source.checker.getContextualType(node);
			for (const variant of variants) {
				if (!accepts(context, variant)) continue;
				let tags: string[] = [];
				if (ts.isStringLiteralLike(node) && variant.entry.category === "verb") tags = [node.text];
				if (ts.isArrayLiteralExpression(node) && typeof variant.discriminator.index === "number") {
					const element = node.elements[variant.discriminator.index];
					if (element) tags = values(element);
				}
				if (ts.isObjectLiteralExpression(node) && typeof variant.discriminator.key === "string") {
					const property = node.properties.find(property => !!property.name && source.name(property.name) === variant.discriminator.key);
					if (property && ts.isPropertyAssignment(property)) tags = values(property.initializer);
					else if (property && ts.isShorthandPropertyAssignment(property)) tags = values(property.name);
				}
				if (tags.includes(String(variant.discriminator.value))) add(variant.entry, node, "typed-variant-producer");
			}
		}
		if (ts.isCaseClause(node) && ts.isStringLiteralLike(node.expression)) {
			const switchNode = node.parent.parent;
			if (!ts.isSwitchStatement(switchNode)) return;
			const expression = switchNode.expression;
			for (const variant of variants) {
				if (node.expression.text !== variant.discriminator.value) continue;
				const input = receiver(expression, variant);
				if (input && accepts(source.checker.getTypeAtLocation(input), variant)) add(variant.entry, node, "typed-variant-consumer");
			}
		}
		if (ts.isBinaryExpression(node) && [ts.SyntaxKind.EqualsEqualsToken, ts.SyntaxKind.EqualsEqualsEqualsToken, ts.SyntaxKind.ExclamationEqualsToken, ts.SyntaxKind.ExclamationEqualsEqualsToken].includes(node.operatorToken.kind)) {
			for (const [expression, tag] of [[node.left, node.right], [node.right, node.left]] as const) if (ts.isStringLiteralLike(tag)) for (const variant of variants) {
				const input = receiver(expression, variant);
				if (tag.text === variant.discriminator.value && input && accepts(source.checker.getTypeAtLocation(input), variant)) add(variant.entry, node, "typed-variant-consumer");
			}
		}
	});
	return result;
}
