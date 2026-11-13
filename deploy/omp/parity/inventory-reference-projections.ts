import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";
import { SourceIndex } from "./inventory-source";
import { sourceSpan } from "./inventory-setting-controls";

/** Reference provenance describes the source resolver; it never resolves keys itself. */
export function annotateReferenceProjections(source: SourceIndex, entries: readonly InventoryEntry[]): void {
	const file = source.files.find(file => source.relative(file) === "packages/tui/src/native/reconcile.ts");
	if (!file) return;
	const registry = source.named("packages/wire/src/tsp.ts", "TspPropsByKind");
	if (!ts.isInterfaceDeclaration(registry)) return;
	source.walk(file, node => {
		if (!ts.isPropertyAssignment(node) || !ts.isPropertyAccessExpression(node.initializer) || !ts.isIdentifier(node.initializer.expression)) return;
		if (!ts.isObjectLiteralExpression(node.parent) || !ts.isReturnStatement(node.parent.parent)) return;
		const item = source.declaration(node.initializer.expression);
		if (!item || !ts.isVariableDeclaration(item) || !item.initializer || !ts.isCallExpression(item.initializer) || !ts.isPropertyAccessExpression(item.initializer.expression) || item.initializer.expression.name.text !== "find") return;
		const predicate = item.initializer.arguments.find(argument => ts.isArrowFunction(argument) || ts.isFunctionExpression(argument));
		if (!predicate || !(ts.isArrowFunction(predicate) || ts.isFunctionExpression(predicate))) return;
		const parameter = predicate.parameters[0];
		if (!parameter || !ts.isIdentifier(parameter.name)) return;
		let clause: ts.Node | undefined = node;
		while (clause && !ts.isCaseClause(clause)) clause = clause.parent;
		if (!clause || !ts.isCaseClause(clause) || !ts.isStringLiteralLike(clause.expression)) return;
		const kind = clause.expression.text;
		const kindEntry = registry.members.find(member => !!member.name && source.name(member.name) === kind);
		if (!kindEntry || !ts.isPropertySignature(kindEntry) || !kindEntry.type) return;
		const property = source.name(node.name);
		if (!property) return;
		const declarations = source.checker.getPropertyOfType(source.checker.getTypeFromTypeNode(kindEntry.type), property)?.declarations ?? [];
		const selected = entries.filter(entry => entry.category === "property" && declarations.some(declaration => entry.source === source.relative(declaration) && Array.isArray(entry.details.declarations) && entry.details.declarations.some(detail => typeof detail === "object" && detail !== null && "start" in detail && detail.start === declaration.getStart())));
		if (selected.length === 0) return;
		const entryId = node.initializer.name.text;
		source.walk(predicate.body, comparison => {
			if (!ts.isBinaryExpression(comparison) || comparison.operatorToken.kind !== ts.SyntaxKind.EqualsEqualsEqualsToken) return;
			for (const [key, value] of [[comparison.left, comparison.right], [comparison.right, comparison.left]] as const) {
				if (!ts.isPropertyAccessExpression(key) || !ts.isPropertyAccessExpression(key.expression) || !ts.isIdentifier(key.expression.expression) || source.declaration(key.expression.expression) !== parameter) continue;
				const input = source.declaration(value);
				if (!input || !ts.isVariableDeclaration(input) || !input.initializer || !ts.isPropertyAccessExpression(input.initializer) || input.initializer.name.text !== property) continue;
				let method: ts.Node | undefined = node;
				while (method && !ts.isMethodDeclaration(method)) method = method.parent;
				const projection = { role: "child-key-to-wire-node-id", sourceKind: kind, sourceProperty: property, producer: method ? sourceSpan(source, method) : undefined, case: sourceSpan(source, clause!), source: sourceSpan(source, input), match: sourceSpan(source, item.initializer!), predicate: sourceSpan(source, predicate.body), comparison: sourceSpan(source, comparison), entryKey: `${key.expression.name.text}.${key.name.text}`, entryId, output: sourceSpan(source, node.parent.parent), property: sourceSpan(source, node) };
				for (const entry of selected) entry.details.valueProjection = projection;
			}
		});
	});
}
