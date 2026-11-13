import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";
import { SourceIndex } from "./inventory-source";
import { annotateSchemaExposure } from "./inventory-exposure";

const WIRE = "packages/wire/src/tsp.ts";

function union(node: ts.TypeNode): ts.TypeNode[] {
	if (ts.isParenthesizedTypeNode(node) || ts.isTypeOperatorNode(node)) return union(node.type);
	return ts.isUnionTypeNode(node) ? node.types.flatMap(union) : [node];
}
function literalStrings(node: ts.TypeNode | undefined): string[] {
	return node ? union(node).flatMap(type => ts.isLiteralTypeNode(type) && ts.isStringLiteralLike(type.literal) ? [type.literal.text] : []) : [];
}
function discriminator(node: ts.TypeNode, name: string): string[] {
	if (!ts.isTypeLiteralNode(node)) return [];
	return node.members.flatMap(member => ts.isPropertySignature(member) && member.name.getText() === name ? literalStrings(member.type) : []);
}

/** Field identity belongs to its declaring type, not each kind which inherits it. */
export function schemaInventory(source: SourceIndex, components: readonly InventoryEntry[]): InventoryEntry[] {
	const file = source.file(WIRE);
	const entries: InventoryEntry[] = [];
	const kinds = source.valueNamed(WIRE, "TSP_KINDS");
	if (!Array.isArray(kinds) || !kinds.every(kind => typeof kind === "string")) throw new Error("TSP_KINDS is not a statically enumerable canonical registry");
	const propsByKind = source.named(WIRE, "TspPropsByKind");
	if (!ts.isInterfaceDeclaration(propsByKind)) throw new Error("TspPropsByKind is not an interface");
	for (const kind of kinds) {
		const declaration = propsByKind.members.find(member => member.name && source.name(member.name) === kind);
		entries.push(source.entry(`kind:${kind}`, "kind", declaration ?? source.named(WIRE, "TSP_KINDS"), { props: declaration && ts.isPropertySignature(declaration) ? declaration.type?.getText() : undefined, commonProps: "TspCommonProps" }));
	}
	for (const [name, key, category] of [["TspEvent", "ev", "event"], ["TspQuery", "q", "query"], ["TspReply", "r", "reply"]] as const) {
		const declaration = source.named(WIRE, name);
		if (!ts.isTypeAliasDeclaration(declaration)) throw new Error(`${name} is not a type alias`);
		for (const variant of union(declaration.type)) for (const tag of discriminator(variant, key)) entries.push(source.entry(`${category}:${tag}`, category, variant, { declaringType: name, schema: variant.getText(), discriminator: { key, value: tag } }));
	}
	const operations = source.named(WIRE, "TspOp");
	if (!ts.isTypeAliasDeclaration(operations)) throw new Error("TspOp is not a type alias");
	for (const variant of union(operations.type)) {
		if (!ts.isTupleTypeNode(variant)) throw new Error(`Unenumerated TspOp variant: ${variant.getText()}`);
		const first = variant.elements[0];
		const tags = first ? literalStrings(ts.isNamedTupleMember(first) ? first.type : first) : [];
		if (tags.length !== 1) throw new Error(`Unenumerated TspOp discriminant: ${variant.getText()}`);
		entries.push(source.entry(`operation:${tags[0]}`, "operation", variant, { declaringType: "TspOp", discriminator: { index: 0, value: tags[0] }, fields: variant.elements.map(element => ts.isNamedTupleMember(element) ? { name: element.name.getText(), optional: !!element.questionToken, type: element.type.getText() } : { type: element.getText() }) }));
	}
	const verb = source.named(WIRE, "TspVerb");
	if (ts.isTypeAliasDeclaration(verb)) for (const value of literalStrings(verb.type)) entries.push(source.entry(`verb:${value}`, "verb", verb, { declaringType: "TspVerb", discriminator: { value } }));

	const fields = new Map<string, InventoryEntry>();
	const field = (owner: string, path: string, declaration: ts.Node, type: ts.TypeNode | undefined, optional: boolean, variants: string[]): void => {
		const id = `property:${owner}.${path}`;
		const references: Array<{ name: string; source: string; line: number; schema: string }> = [];
		const visited = new Set<ts.Node>();
		const referencedAliases = (node: ts.Node): void => {
			if (ts.isTypeReferenceNode(node)) {
				const alias = source.declaration(node.typeName);
				if (alias && ts.isTypeAliasDeclaration(alias) && source.relative(alias) === WIRE && !visited.has(alias)) {
					visited.add(alias);
					references.push({ name: alias.name.text, ...source.origin(alias), schema: alias.type.getText() });
					referencedAliases(alias.type);
				}
			}
			node.forEachChild(referencedAliases);
		};
		if (type) referencedAliases(type);
		const detail = { ...source.origin(declaration), start: declaration.getStart(), optional, type: type?.getText(), variants, references };
		const previous = fields.get(id);
		if (previous) (previous.details.declarations as unknown[]).push(detail);
		else fields.set(id, source.entry(id, "property", declaration, { declaringType: owner, path, declarations: [detail] }));
	};
	const properties = (owner: string, node: ts.TypeNode | ts.InterfaceDeclaration, prefix = "", variants: string[] = []): void => {
		if (ts.isInterfaceDeclaration(node) || ts.isTypeLiteralNode(node)) {
			for (const member of node.members) {
				if (ts.isPropertySignature(member)) {
					const name = source.name(member.name);
					if (name === undefined) throw new Error(`Unenumerated schema property: ${member.getText()}`);
					const path = `${prefix}${name}`;
					field(owner, path, member, member.type, !!member.questionToken, variants);
					if (member.type) properties(owner, member.type, `${path}.`, variants);
				} else if (ts.isIndexSignatureDeclaration(member)) {
					const path = `${prefix.replace(/\.$/, "")}[${member.parameters.map(parameter => parameter.name.getText()).join(",")}]`;
					field(owner, path, member, member.type, false, variants);
					if (member.type) properties(owner, member.type, `${path}.`, variants);
				}
			}
		} else if (ts.isUnionTypeNode(node) || ts.isIntersectionTypeNode(node)) {
			for (const child of node.types) {
				const tags = ["ev", "q", "r", "type", "kind"].flatMap(key => discriminator(child, key).map(value => `${key}:${value}`));
				const shape = union(child)[0];
				if (shape && ts.isTupleTypeNode(shape) && shape.elements[0]) {
					const first = shape.elements[0];
					tags.push(...literalStrings(ts.isNamedTupleMember(first) ? first.type : first).map(value => `op:${value}`));
				}
				properties(owner, child, prefix, [...variants, ...tags]);
			}
		} else if (ts.isArrayTypeNode(node)) properties(owner, node.elementType, `${prefix.replace(/\.$/, "")}[].`, variants);
		else if (ts.isParenthesizedTypeNode(node) || ts.isTypeOperatorNode(node) || ts.isOptionalTypeNode(node)) properties(owner, node.type, prefix, variants);
		else if (ts.isTypeReferenceNode(node)) {
			const name = node.typeName.getText();
			if (name === "Record" || name === "ReadonlyMap" || name === "Map") {
				const path = `${prefix.replace(/\.$/, "")}[key]`;
				field(owner, path, node, node.typeArguments?.[1], false, variants);
				if (node.typeArguments?.[1]) properties(owner, node.typeArguments[1], `${path}.`, variants);
			} else if (name === "Array" || name === "ReadonlyArray") {
				if (node.typeArguments?.[0]) properties(owner, node.typeArguments[0], `${prefix.replace(/\.$/, "")}[].`, variants);
			} else if (name === "Readonly" && node.typeArguments?.[0]) properties(owner, node.typeArguments[0], prefix, variants);
		} else if (ts.isMappedTypeNode(node)) {
			const path = `${prefix.replace(/\.$/, "")}[${node.typeParameter.name.text}]`;
			field(owner, path, node, node.type, !!node.questionToken, variants);
			if (node.type) properties(owner, node.type, `${path}.`, variants);
		} else if (ts.isIndexedAccessTypeNode(node)) properties(owner, node.objectType, prefix, variants);
		else if (ts.isTupleTypeNode(node)) {
			for (const [index, element] of node.elements.entries()) {
				const type = ts.isNamedTupleMember(element) ? element.type : element;
				const path = `${prefix.replace(/\.$/, "")}[${ts.isNamedTupleMember(element) ? element.name.getText() : index}]`;
				field(owner, path, element, type, ts.isNamedTupleMember(element) && !!element.questionToken, variants);
				properties(owner, type, `${path}.`, variants);
			}
		}
	};
	for (const statement of file.statements) {
		if (ts.isInterfaceDeclaration(statement)) properties(statement.name.text, statement);
		else if (ts.isTypeAliasDeclaration(statement)) properties(statement.name.text, statement.type);
	}
	entries.push(...fields.values());
	annotateSchemaExposure(source, entries, components);
	return entries;
}
