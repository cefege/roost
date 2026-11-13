import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";
import { SourceIndex, type Origin } from "./inventory-source";

export interface RuntimeSchemaField extends Origin {
	name: string;
	optional: boolean;
	readonly: boolean;
	propertyIds: string[];
	type: RuntimeSchemaNode;
}
export type RuntimeSchemaNode = Origin & (
	| { kind: "ref"; name: string; arguments: RuntimeSchemaNode[] }
	| { kind: "object"; fields: RuntimeSchemaField[]; indexes: Array<{ key: RuntimeSchemaNode; value: RuntimeSchemaNode; propertyIds: string[] }> }
	| { kind: "union" | "intersection"; types: RuntimeSchemaNode[] }
	| { kind: "array"; item: RuntimeSchemaNode }
	| { kind: "tuple"; items: Array<{ name?: string; optional: boolean; rest: boolean; propertyIds: string[]; type: RuntimeSchemaNode }> }
	| { kind: "literal"; value: unknown }
	| { kind: "scalar"; name: string }
	| { kind: "operator"; operator: string; type: RuntimeSchemaNode }
	| { kind: "indexed"; object: RuntimeSchemaNode; index: RuntimeSchemaNode }
	| { kind: "mapped"; parameter: string; constraint: RuntimeSchemaNode; nameType?: RuntimeSchemaNode; value: RuntimeSchemaNode; optional: string; readonly: string; propertyIds: string[] }
	| { kind: "conditional"; check: RuntimeSchemaNode; extends: RuntimeSchemaNode; whenTrue: RuntimeSchemaNode; whenFalse: RuntimeSchemaNode }
	| { kind: "template"; head: string; spans: Array<{ type: RuntimeSchemaNode; text: string }> }
	| { kind: "value"; name: string; value: unknown }
	| { kind: "unknown"; expression: string }
);
export interface RuntimeSchemaDeclaration extends Origin {
	kind: "interface" | "alias";
	parameters: Array<{ name: string; constraint?: RuntimeSchemaNode; default?: RuntimeSchemaNode }>;
	extends: RuntimeSchemaNode[];
	definition: RuntimeSchemaNode;
}
export type RuntimeSchema = Record<string, RuntimeSchemaDeclaration>;

/** Retain TypeScript's type graph; field links come from declaration offsets, not a mirrored schema. */
export function runtimeSchemaInventory(source: SourceIndex, entries: InventoryEntry[]): RuntimeSchema {
	const propertyIds = new Map<string, string[]>();
	for (const entry of entries) {
		if (entry.category !== "property" || !Array.isArray(entry.details.declarations)) continue;
		for (const declaration of entry.details.declarations as Array<{ start?: number }>) {
			if (declaration.start === undefined) continue;
			const key = `${entry.details.declaringType}:${declaration.start}`;
			const previous = propertyIds.get(key) ?? [];
			if (!previous.includes(entry.id)) previous.push(entry.id);
			propertyIds.set(key, previous);
		}
	}
	const links = (owner: string, node: ts.Node): string[] => propertyIds.get(`${owner}:${node.getStart()}`) ?? [];
	const object = (owner: string, node: ts.InterfaceDeclaration | ts.TypeLiteralNode): RuntimeSchemaNode => {
		const fields: RuntimeSchemaField[] = [];
		const indexes: Array<{ key: RuntimeSchemaNode; value: RuntimeSchemaNode; propertyIds: string[] }> = [];
		for (const member of node.members) {
			if (ts.isPropertySignature(member)) {
				const name = source.name(member.name);
				if (name === undefined) throw new Error(`Unresolved runtime schema field: ${member.getText()}`);
				fields.push({ name, ...source.origin(member), optional: !!member.questionToken, readonly: member.modifiers?.some(modifier => modifier.kind === ts.SyntaxKind.ReadonlyKeyword) ?? false, propertyIds: links(owner, member), type: type(owner, member.type ?? member) });
			} else if (ts.isIndexSignatureDeclaration(member)) indexes.push({ key: type(owner, member.parameters[0]?.type ?? member), value: type(owner, member.type ?? member), propertyIds: links(owner, member) });
		}
		return { kind: "object", ...source.origin(node), fields, indexes };
	};
	const type = (owner: string, node: ts.Node): RuntimeSchemaNode => {
		const origin = source.origin(node);
		if (ts.isParenthesizedTypeNode(node)) return type(owner, node.type);
		if (ts.isTypeLiteralNode(node)) return object(owner, node);
		if (ts.isUnionTypeNode(node) || ts.isIntersectionTypeNode(node)) return { kind: ts.isUnionTypeNode(node) ? "union" : "intersection", ...origin, types: node.types.map(member => type(owner, member)) };
		if (ts.isTypeReferenceNode(node)) return { kind: "ref", ...origin, name: node.typeName.getText(), arguments: node.typeArguments?.map(argument => type(owner, argument)) ?? [] };
		if (ts.isArrayTypeNode(node)) return { kind: "array", ...origin, item: type(owner, node.elementType) };
		if (ts.isTypeOperatorNode(node)) return { kind: "operator", ...origin, operator: ts.tokenToString(node.operator) ?? ts.SyntaxKind[node.operator], type: type(owner, node.type) };
		if (ts.isOptionalTypeNode(node) || ts.isRestTypeNode(node)) return { kind: "operator", ...origin, operator: ts.isOptionalTypeNode(node) ? "optional" : "rest", type: type(owner, node.type) };
		if (ts.isTupleTypeNode(node)) return { kind: "tuple", ...origin, items: node.elements.map(element => ({ name: ts.isNamedTupleMember(element) ? element.name.getText() : undefined, optional: ts.isNamedTupleMember(element) ? !!element.questionToken : ts.isOptionalTypeNode(element), rest: ts.isNamedTupleMember(element) ? !!element.dotDotDotToken : ts.isRestTypeNode(element), propertyIds: links(owner, element), type: type(owner, ts.isNamedTupleMember(element) ? element.type : element) })) };
		if (ts.isLiteralTypeNode(node)) return { kind: "literal", ...origin, value: source.serialize(source.value(node.literal)) };
		if (ts.isIndexedAccessTypeNode(node)) return { kind: "indexed", ...origin, object: type(owner, node.objectType), index: type(owner, node.indexType) };
		if (ts.isMappedTypeNode(node)) return { kind: "mapped", ...origin, parameter: node.typeParameter.name.text, constraint: type(owner, node.typeParameter.constraint ?? node.typeParameter), nameType: node.nameType ? type(owner, node.nameType) : undefined, value: node.type ? type(owner, node.type) : { kind: "unknown", ...origin, expression: node.getText() }, optional: node.questionToken?.getText() ?? "preserve", readonly: node.readonlyToken?.getText() ?? "preserve", propertyIds: links(owner, node) };
		if (ts.isConditionalTypeNode(node)) return { kind: "conditional", ...origin, check: type(owner, node.checkType), extends: type(owner, node.extendsType), whenTrue: type(owner, node.trueType), whenFalse: type(owner, node.falseType) };
		if (ts.isTemplateLiteralTypeNode(node)) return { kind: "template", ...origin, head: node.head.text, spans: node.templateSpans.map(span => ({ type: type(owner, span.type), text: span.literal.text })) };
		if (ts.isTypeQueryNode(node)) return { kind: "value", ...origin, name: node.exprName.getText(), value: source.serialize(source.value(node.exprName)) };
		if ([ts.SyntaxKind.StringKeyword, ts.SyntaxKind.NumberKeyword, ts.SyntaxKind.BooleanKeyword, ts.SyntaxKind.UnknownKeyword, ts.SyntaxKind.AnyKeyword, ts.SyntaxKind.NeverKeyword, ts.SyntaxKind.UndefinedKeyword, ts.SyntaxKind.ObjectKeyword, ts.SyntaxKind.VoidKeyword].includes(node.kind)) return { kind: "scalar", ...origin, name: node.getText() };
		return { kind: "unknown", ...origin, expression: node.getText() };
	};
	const graph: RuntimeSchema = {};
	for (const statement of source.file("packages/wire/src/tsp.ts").statements) {
		if (!ts.isInterfaceDeclaration(statement) && !ts.isTypeAliasDeclaration(statement)) continue;
		const owner = statement.name.text;
		graph[owner] = {
			kind: ts.isInterfaceDeclaration(statement) ? "interface" : "alias", ...source.origin(statement),
			parameters: statement.typeParameters?.map(parameter => ({ name: parameter.name.text, constraint: parameter.constraint ? type(owner, parameter.constraint) : undefined, default: parameter.default ? type(owner, parameter.default) : undefined })) ?? [],
			extends: ts.isInterfaceDeclaration(statement) ? statement.heritageClauses?.flatMap(heritage => heritage.types.map(parent => ({ kind: "ref" as const, ...source.origin(parent), name: parent.expression.getText(), arguments: parent.typeArguments?.map(argument => type(owner, argument)) ?? [] }))) ?? [] : [],
			definition: ts.isInterfaceDeclaration(statement) ? object(owner, statement) : type(owner, statement.type),
		};
	}
	return graph;
}
