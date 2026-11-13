import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";
import { record, SourceIndex } from "./inventory-source";
import { SourceFlowIndex } from "./inventory-flow";
import type { SourceEmitter } from "./inventory-flow";
import { nativeVariantEmitters } from "./inventory-native-emitters";
import { nativeKindEmitters } from "./inventory-native-kinds";
import { annotateReferenceProjections } from "./inventory-reference-projections";
import { sourceSpan } from "./inventory-setting-controls";
import { classifyUnproduced, nameOccurrences, unreferencedProducers } from "./inventory-unproduced";
import { elideUnstorableNull } from "./inventory-null-elision";

const WIRE = "packages/wire/src/tsp.ts";
type SchemaDeclaration = ts.InterfaceDeclaration | ts.TypeAliasDeclaration;
interface Reference { source: string; line: number; expression: string; declaringType: string; role: string }
interface Path { root: string; declarations: Array<{ name: string; source: string; line: number }> }

function finiteDomain(source: SourceIndex, types: readonly ts.TypeNode[]): Record<string, unknown> {
	const values: Array<string | number | boolean | null> = [];
	const descriptions: string[] = [];
	let finite = types.length > 0;
	let unresolved = types.length === 0;
	const visit = (type: ts.Type): void => {
		if (type.isUnion()) { for (const item of type.types) visit(item); return; }
		descriptions.push(source.checker.typeToString(type));
		let value: string | number | boolean | null | undefined;
		if (type.flags & ts.TypeFlags.StringLiteral) value = (type as ts.StringLiteralType).value;
		else if (type.flags & ts.TypeFlags.NumberLiteral) value = (type as ts.NumberLiteralType).value;
		else if (type.flags & ts.TypeFlags.BooleanLiteral) value = source.checker.typeToString(type) === "true";
		else if (type.flags & ts.TypeFlags.Null) value = null;
		else if (type.flags & (ts.TypeFlags.Undefined | ts.TypeFlags.Never)) return;
		else {
			finite = false;
			if (type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown | ts.TypeFlags.TypeParameter | ts.TypeFlags.IndexedAccess | ts.TypeFlags.Conditional | ts.TypeFlags.Substitution)) unresolved = true;
			return;
		}
		if (!values.some(previous => Object.is(previous, value))) values.push(value);
	};
	for (const type of types) visit(source.checker.getTypeFromTypeNode(type));
	return { values, finite, unresolved, types: [...new Set(descriptions)] };
}

function declaredFieldType(node: ts.Node): ts.TypeNode | undefined {
	if (ts.isPropertySignature(node) || ts.isIndexSignatureDeclaration(node) || ts.isMappedTypeNode(node) || ts.isNamedTupleMember(node)) return node.type;
	if (ts.isTypeReferenceNode(node) && ["Record", "ReadonlyMap", "Map"].includes(node.typeName.getText())) return node.typeArguments?.[1];
	return ts.isTypeNode(node) ? node : undefined;
}

/** Separate element props from transport/schema fields using their real type graph and production uses. */
export function annotateSchemaExposure(source: SourceIndex, entries: InventoryEntry[], components: readonly InventoryEntry[]): void {
	const file = source.file(WIRE);
	const declarations = new Map<string, SchemaDeclaration>();
	for (const statement of file.statements) if (ts.isInterfaceDeclaration(statement) || ts.isTypeAliasDeclaration(statement)) declarations.set(statement.name.text, statement);
	const graph = new Map<string, Set<string>>();
	const declarationName = (node: ts.Node): string | undefined => {
		const target = source.declaration(node);
		return target && source.relative(target) === WIRE && (ts.isInterfaceDeclaration(target) || ts.isTypeAliasDeclaration(target)) ? target.name.text : undefined;
	};
	for (const [name, declaration] of declarations) {
		const edges = new Set<string>();
		source.walk(declaration, node => {
			const target = ts.isTypeReferenceNode(node) ? declarationName(node.typeName) : ts.isExpressionWithTypeArguments(node) ? declarationName(node.expression) : undefined;
			if (target && target !== name) edges.add(target);
		});
		graph.set(name, edges);
	}
	const references = new Map<string, Reference[]>();
	const recordReference = (name: string, node: ts.Node, role: string): void => {
		const list = references.get(name) ?? [];
		const reference = { ...source.origin(node), expression: node.getText(), declaringType: name, role };
		if (!list.some(previous => previous.source === reference.source && previous.line === reference.line && previous.expression === reference.expression && previous.role === role)) list.push(reference);
		references.set(name, list);
	};
	for (const candidate of source.files) if (source.relative(candidate) !== WIRE) source.walk(candidate, node => {
		if (!ts.isTypeReferenceNode(node) && !ts.isExpressionWithTypeArguments(node)) return;
		const name = declarationName(ts.isTypeReferenceNode(node) ? node.typeName : node.expression);
		if (name) recordReference(name, node, "production-type-reference");
	});
	const paths = (roots: Iterable<string>): Map<string, Path> => {
		const result = new Map<string, Path>();
		const pending = [...roots].map(root => ({ root, name: root, path: [] as Path["declarations"] }));
		while (pending.length) {
			const current = pending.shift()!;
			if (result.has(current.name)) continue;
			const declaration = declarations.get(current.name);
			if (!declaration) continue;
			const path = [...current.path, { name: current.name, ...source.origin(declaration) }];
			result.set(current.name, { root: current.root, declarations: path });
			for (const next of graph.get(current.name) ?? []) pending.push({ root: current.root, name: next, path });
		}
		return result;
	};
	const propsByKind = source.named(WIRE, "TspPropsByKind");
	if (!ts.isInterfaceDeclaration(propsByKind)) throw new Error("UI props registry is not an interface");
	const uiRoots = new Set<string>(["TspCommonProps"]);
	for (const member of propsByKind.members) if (ts.isPropertySignature(member) && member.type) source.walk(member.type, node => {
		if (ts.isTypeReferenceNode(node)) { const name = declarationName(node.typeName); if (name) uiRoots.add(name); }
	});
	const ui = paths(uiRoots);
	const native = paths(references.keys());
	const registryUses: Reference[] = [];
	source.walk(file, node => {
		if (!ts.isTypeReferenceNode(node) || declarationName(node.typeName) !== propsByKind.name.text) return;
		if (ts.isIndexedAccessTypeNode(node.parent) && node.parent.objectType === node || ts.isTypeOperatorNode(node.parent) && node.parent.operator === ts.SyntaxKind.KeyOfKeyword) registryUses.push({ ...source.origin(node.parent), expression: node.parent.getText(), declaringType: propsByKind.name.text, role: "indexed-kind-props-registry" });
	});
	const nodesByStart = new Map<number, ts.Node[]>();
	source.walk(file, node => { const nodes = nodesByStart.get(node.getStart(file)) ?? []; nodes.push(node); nodesByStart.set(node.getStart(file), nodes); });
	const propertiesByDeclaration = new Map<ts.Node, InventoryEntry[]>();
	const declarationsByEntry = new Map<string, ts.Node[]>();
	for (const entry of entries) if (entry.category === "property") {
		const types: ts.TypeNode[] = [];
		for (const detail of Array.isArray(entry.details.declarations) ? entry.details.declarations : []) {
			if (!record(detail) || typeof detail.start !== "number") continue;
			const declaration = (nodesByStart.get(detail.start) ?? []).find(node => declaredFieldType(node)?.getText() === detail.type);
			if (!declaration) continue;
			const type = declaredFieldType(declaration);
			if (type) types.push(type);
			const owners = propertiesByDeclaration.get(declaration) ?? []; owners.push(entry); propertiesByDeclaration.set(declaration, owners);
			const declared = declarationsByEntry.get(entry.id) ?? []; declared.push(declaration); declarationsByEntry.set(entry.id, declared);
		}
		const domain = finiteDomain(source, types);
		entry.details.enumValues = domain.values;
		entry.details.enumDomain = { ...domain, declarations: entry.details.declarations };
	}
	const emitters = new Map<string, Reference[]>();
	const emitterNodes = new Map<string, SourceEmitter[]>();
	const emitted = (entry: InventoryEntry, node: ts.Node, role: string): void => {
		const list = emitters.get(entry.id) ?? [];
		const reference = { ...sourceSpan(source, node), declaringType: String(entry.details.declaringType ?? ""), role };
		if (!list.some(previous => previous.source === reference.source && previous.line === reference.line && previous.expression === reference.expression)) list.push(reference);
		emitters.set(entry.id, list);
		const nodes = emitterNodes.get(entry.id) ?? [];
		if (!nodes.some(emitter => emitter.node === node && emitter.role === role)) nodes.push({ node, role });
		emitterNodes.set(entry.id, nodes);
	};
	const contextualDeclarations = (type: ts.Type, key: string): ts.Declaration[] => (type.isUnion() ? type.types : [type]).flatMap(item => source.checker.getPropertyOfType(item, key)?.declarations ?? []);
	// An unannotated `const props = {…}` has no contextual type of its own; the
	// typed parameters it is later passed to are where the source checks it, and
	// a literal nested inside it is checked against the matching member type.
	const literalContexts = new Map<ts.Expression, ts.Type[]>();
	const members = (types: readonly ts.Type[], at: ts.Node, member: (type: ts.Type) => ts.Type | undefined): ts.Type[] =>
		types.flatMap(type => type.isUnion() ? type.types : [type]).flatMap(type => { const found = member(type); return found ? [source.checker.getNonNullableType(found)] : []; });
	const contextsOf = (expression: ts.Expression): ts.Type[] => {
		const cached = literalContexts.get(expression);
		if (cached) return cached;
		literalContexts.set(expression, []);
		const own = source.checker.getContextualType(expression);
		const contexts = own ? [own] : [];
		const parent = expression.parent;
		if (!own && ts.isVariableDeclaration(parent) && parent.initializer === expression && !parent.type && ts.isIdentifier(parent.name)) {
			const binding = parent.name;
			const symbol = source.checker.getSymbolAtLocation(binding);
			const scope = ts.findAncestor(parent, ancestor => ts.isBlock(ancestor) || ts.isSourceFile(ancestor));
			if (symbol && scope) source.walk(scope, use => {
				if (!ts.isIdentifier(use) || use === binding || source.checker.getSymbolAtLocation(use) !== symbol) return;
				const context = source.checker.getContextualType(use);
				if (context) contexts.push(context);
			});
		} else if (!own && ts.isPropertyAssignment(parent) && parent.initializer === expression && ts.isObjectLiteralExpression(parent.parent)) {
			const name = source.name(parent.name);
			if (name) contexts.push(...members(contextsOf(parent.parent), expression, type => { const property = source.checker.getPropertyOfType(type, name); return property && source.checker.getTypeOfSymbolAtLocation(property, expression); }));
		} else if (!own && ts.isArrayLiteralExpression(parent)) {
			contexts.push(...members(contextsOf(parent), expression, type => source.checker.getIndexTypeOfType(type, ts.IndexKind.Number)));
		} else if (!own && (ts.isParenthesizedExpression(parent) || ts.isSpreadAssignment(parent) || ts.isConditionalExpression(parent) && parent.condition !== expression || ts.isBinaryExpression(parent) && [ts.SyntaxKind.QuestionQuestionToken, ts.SyntaxKind.BarBarToken].includes(parent.operatorToken.kind))) {
			contexts.push(...contextsOf(ts.isSpreadAssignment(parent) ? parent.parent as ts.ObjectLiteralExpression : parent));
		}
		literalContexts.set(expression, contexts);
		return contexts;
	};
	for (const candidate of source.files) if (source.relative(candidate) !== WIRE) source.walk(candidate, node => {
		if ((ts.isPropertyAssignment(node) || ts.isShorthandPropertyAssignment(node)) && ts.isObjectLiteralExpression(node.parent)) {
			const name = source.name(node.name);
			if (name) for (const context of contextsOf(node.parent)) for (const declaration of contextualDeclarations(context, name)) for (const entry of propertiesByDeclaration.get(declaration) ?? []) emitted(entry, node, "typed-property-emitter");
		} else if (ts.isSpreadAssignment(node) && ts.isObjectLiteralExpression(node.parent) && !ts.isObjectLiteralExpression(node.expression)) {
			// `{ ...props, hunks }` emits the members of an operand typed by some other declaration. A member the operand
			// shares with the literal's own container (`{ ...row, label }` over a row of the same type) is a copy of a value
			// built where that container was built, so it is no producer here.
			const operand = source.checker.getTypeAtLocation(node.expression);
			for (const context of contextsOf(node.parent)) for (const property of operand.getProperties()) {
				const own = [...contextualDeclarations(context, property.name)];
				if (property.declarations?.some(declaration => own.includes(declaration))) continue;
				for (const declaration of own) for (const entry of propertiesByDeclaration.get(declaration) ?? []) emitted(entry, node, "typed-spread-emitter");
			}
		} else if (ts.isPropertyAccessExpression(node)) {
			for (const declaration of source.symbol(node.name)?.declarations ?? []) for (const entry of propertiesByDeclaration.get(declaration) ?? []) emitted(entry, node, ts.isBinaryExpression(node.parent) && node.parent.left === node && node.parent.operatorToken.kind === ts.SyntaxKind.EqualsToken ? "typed-property-write" : "typed-property-read");
		}
	});
	for (const [id, nodes] of nativeVariantEmitters(source, entries)) {
		const entry = entries.find(entry => entry.id === id)!;
		for (const emitter of nodes) emitted(entry, emitter.node, emitter.role);
	}
	const kinds = nativeKindEmitters(source);
	const flow = new SourceFlowIndex(source, components);
	annotateReferenceProjections(source, entries);
	const schemaName = (entry: InventoryEntry): string | undefined => {
		if (typeof entry.details.declaringType === "string") return entry.details.declaringType;
		if (entry.category === "operation") return "TspOp";
		return undefined;
	};
	for (const entry of entries) {
		if (entry.category === "kind") {
			const producers = kinds.get(entry.id.slice("kind:".length)) ?? [];
			entry.details.exposure = "ui";
			entry.details.exposureProof = { registry: source.origin(propsByKind), declaringType: propsByKind.name.text };
			entry.details.emitters = producers.map(emitter => ({ ...sourceSpan(source, emitter.node), role: emitter.role }));
			entry.details.uiEmitters = flow.uiEmitters(producers);
			continue;
		}
		const name = schemaName(entry);
		if (!name) continue;
		const uiPath = ui.get(name);
		const nativePath = native.get(name);
		const schemaRegistry = name === propsByKind.name.text && registryUses.length > 0;
		const path = schemaRegistry ? undefined : uiPath ?? nativePath;
		const sourceReferences = schemaRegistry ? registryUses : [...new Set([uiPath?.root, nativePath?.root].filter((root): root is string => !!root))].flatMap(root => references.get(root) ?? []);
		const tagDeclarations = declarationsByEntry.get(entry.id) ?? [];
		const tagged = entry.category === "property" ? flow.nativeContract(tagDeclarations, emitterNodes.get(entry.id) ?? []) ?? flow.browserOnly(tagDeclarations, emitterNodes.get(entry.id) ?? []) : undefined;
		entry.details.exposure = tagged ? tagged.proven === true ? "native-contract" : "unresolved" : schemaRegistry || !uiPath && !!nativePath ? "native-contract" : uiPath ? "ui" : "unresolved";
		entry.details.exposureProof = {
			declaration: declarations.has(name) ? { ...source.origin(declarations.get(name)!), name } : undefined,
			role: tagged ? tagged.proven === true ? String(tagged.role) : "unproven-native-only-field" : schemaRegistry ? "indexed-kind-props-registry" : uiPath ? "element-props-type-graph" : nativePath ? "production-native-type-graph" : "unresolved-source-use",
			typePath: path, sourceReferences,
		};
		entry.details.sourceReferences = sourceReferences;
		entry.details.emitters = emitters.get(entry.id) ?? [];
		entry.details.nativeContractProof = tagged;
		entry.details.uiEmitters = flow.uiEmitters(emitterNodes.get(entry.id) ?? []);
	}
	inheritPassThroughEmitters(entries);
	classifyUnreferencedProducers(source, entries, entry => entry.category === "kind" ? kinds.get(entry.id.slice("kind:".length)) : emitterNodes.get(entry.id));
	elideUnstorableNull(source, entries, uiRoots);
	classifyUnproduced(source, entries);
}

/** A UI kind or property no native root reaches is unproduced when every producer's function is never referenced. */
function classifyUnreferencedProducers(source: SourceIndex, entries: InventoryEntry[], producersOf: (entry: InventoryEntry) => readonly SourceEmitter[] | undefined): void {
	let names: Map<string, ts.Node[]> | undefined;
	for (const entry of entries) {
		if (entry.category !== "kind" && entry.category !== "property" || entry.details.exposure !== "ui") continue;
		if (!Array.isArray(entry.details.uiEmitters) || entry.details.uiEmitters.length) continue;
		const producers = (producersOf(entry) ?? []).filter(emitter => emitter.role !== "typed-property-read").map(emitter => emitter.node);
		if (!producers.length) continue;
		names ??= nameOccurrences(source);
		const proof = unreferencedProducers(source, producers, names);
		if (!proof) continue;
		const key = entry.category === "kind" ? entry.id.slice("kind:".length) : entry.id.slice(entry.id.lastIndexOf(".") + 1);
		const declarations = entry.category === "kind" && record(entry.details.exposureProof) ? [entry.details.exposureProof.registry] : entry.details.declarations;
		entry.details.exposure = "unproduced";
		delete entry.details.uiEmitters;
		entry.details.unproducedProof = { key, declarations, searchedFiles: source.files.length, proof: "producer-unreferenced", producers: proof };
	}
}

const PATH_SEGMENT = /^(.+?)(?:\.[A-Za-z_$][\w$]*|\.\[[A-Za-z_$][\w$]*\]|\[key\]|\[\d+\]|\[\])$/;

/**
 * A nested field nothing writes literally is produced by the expression that
 * emits its enclosing property: `margin: options?.margin` carries the margin
 * object's fields, and `badges: list` carries each badge's. The enclosing
 * property is the parent path, or for a named type every property declared
 * with that type.
 */
function inheritPassThroughEmitters(entries: InventoryEntry[]): void {
	const properties = new Map(entries.filter(entry => entry.category === "property").map(entry => [entry.id.slice("property:".length), entry]));
	const resolved = new Map<InventoryEntry, Record<string, unknown>[]>();
	const emittersOf = (entry: InventoryEntry, visiting: Set<InventoryEntry>): Record<string, unknown>[] => {
		const known = resolved.get(entry);
		if (known) return known;
		const own = Array.isArray(entry.details.uiEmitters) ? entry.details.uiEmitters.filter(record) : [];
		if (own.length || visiting.has(entry)) return own;
		visiting.add(entry);
		const parents: InventoryEntry[] = [];
		let path = entry.id.slice("property:".length);
		for (let match = PATH_SEGMENT.exec(path); match && !parents.length; match = PATH_SEGMENT.exec(path)) {
			path = match[1]!;
			const parent = properties.get(path);
			if (parent) parents.push(parent);
			else if (!/[.[]/.test(path)) {
				const named = new RegExp(`\\b${path}\\b`);
				for (const candidate of properties.values()) if (candidate !== entry && (Array.isArray(candidate.details.declarations) ? candidate.details.declarations : []).some(declaration => record(declaration) && typeof declaration.type === "string" && named.test(declaration.type))) parents.push(candidate);
			}
		}
		const inherited = parents.flatMap(parent => emittersOf(parent, visiting).map(emitter => ({ ...emitter, inheritedFrom: parent.id })));
		visiting.delete(entry);
		// An empty answer may be an artifact of cutting a cycle; only a found producer is final.
		if (inherited.length) {
			resolved.set(entry, inherited);
			entry.details.uiEmitters = inherited;
		}
		return inherited;
	};
	for (const entry of properties.values()) if (entry.details.exposure === "ui") emittersOf(entry, new Set());
}
