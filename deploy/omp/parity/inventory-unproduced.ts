import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";
import { record, type SourceIndex } from "./inventory-source";
import { sourceSpan, type SourceSpan } from "./inventory-setting-controls";
import { SiteReach, type SiteExclusion } from "./inventory-site-reach";

/**
 * Typed-site audit of UI members OMP never produces. Every value the fork
 * hands to a slot typed as a member's container (the declaring type, or the
 * type a nested path reaches inside it), and every assignment into such a
 * value, is a site. A key or enum value no site can hold is unproduced; any
 * untyped or generic write of the key keeps the obligation.
 */

const WIRE = "packages/wire/src/tsp.ts";

interface Member { typeName: string; key: string; topLevel: boolean; containers: Set<ts.Type> }
interface Site { names: Set<string>; parts: Set<ts.Type>; node: ts.Node; type: ts.Type; assignedKey?: string; at: SourceSpan }
/** Literal values a site can put under a key; `open` when its type does not bound them. */
type Values = Set<unknown> | "open";

const parts = (type: ts.Type): ts.Type[] => type.isUnionOrIntersection() ? type.types.flatMap(parts) : [type];
const named = (type: ts.Type): string | undefined => (type.aliasSymbol ?? type.getSymbol())?.getName();
// A write whose target type is generic or untyped could land in any props type. A class's own `this`
// is a class instance, never a props document, so writing a key of it is not a write into one.
const open = (type: ts.Type): boolean => !!(type.flags & (ts.TypeFlags.Instantiable | ts.TypeFlags.Any | ts.TypeFlags.Unknown))
	&& !(type.flags & ts.TypeFlags.TypeParameter && type.symbol && type.symbol.flags & ts.SymbolFlags.Class);

function valueSlot(node: ts.Expression): boolean {
	const parent = node.parent;
	return ts.isCallExpression(parent) && parent.arguments.includes(node) || ts.isNewExpression(parent) && !!parent.arguments?.includes(node)
		|| (ts.isPropertyAssignment(parent) || ts.isVariableDeclaration(parent) || ts.isPropertyDeclaration(parent)) && parent.initializer === node
		|| ts.isReturnStatement(parent) || ts.isArrowFunction(parent) && parent.body === node || ts.isArrayLiteralExpression(parent)
		|| ts.isAsExpression(parent) || ts.isSatisfiesExpression(parent) || ts.isTypeAssertionExpression(parent)
		|| ts.isConditionalExpression(parent) && parent.condition !== node || ts.isBinaryExpression(parent) && parent.right === node;
}

const assignment = (node: ts.Node): node is ts.BinaryExpression => ts.isBinaryExpression(node)
	&& node.operatorToken.kind >= ts.SyntaxKind.FirstAssignment && node.operatorToken.kind <= ts.SyntaxKind.LastAssignment;

class TypedSites {
	readonly written = new Set<string>();
	readonly openWrites = new Set<string>();
	readonly sites: Site[] = [];
	/** Sites and open writes no session can run: kept with their proof, never counted as producers. */
	readonly excludedSites: Array<{ site: Site; exclusion: SiteExclusion }> = [];
	readonly excludedWrites: Array<{ key: string; at: SourceSpan; exclusion: SiteExclusion }> = [];
	searched = 0;
	private readonly checker: ts.TypeChecker;

	constructor(private readonly source: SourceIndex, members: Iterable<Member>, private readonly reach?: SiteReach) {
		this.checker = source.checker;
		const list = [...members];
		const typeNames = new Set(list.map(member => member.typeName));
		const containers = new Set(list.flatMap(member => [...member.containers]));
		const keys = new Set(list.map(member => member.key));
		const matching = (types: ts.Type[]) => {
			const all = types.flatMap(type => [type, ...parts(type)]);
			// A union alias (`TspPrefsControl`) names only the whole; its members are anonymous.
			return { all, names: new Set(all.flatMap(part => { const name = named(part); return name && typeNames.has(name) ? [name] : []; })), parts: new Set(all.filter(part => containers.has(part))) };
		};
		for (const file of source.files) {
			if (source.relative(file) === WIRE) continue;
			this.searched++;
			source.walk(file, node => {
				if ((ts.isPropertyAssignment(node) || ts.isShorthandPropertyAssignment(node) || ts.isMethodDeclaration(node) || ts.isGetAccessorDeclaration(node)) && ts.isObjectLiteralExpression(node.parent)) {
					const name = source.name(node.name);
					if (name) this.written.add(name);
					const context = name && keys.has(name) ? this.checker.getContextualType(node.parent) : undefined;
					if (name && context && parts(context).some(open) && !this.serializedByLibrary(node.parent)) this.openWrite(name, node);
				} else if ((ts.isPropertyAccessExpression(node) || ts.isElementAccessExpression(node)) && assignment(node.parent) && node.parent.left === node) {
					const key = ts.isPropertyAccessExpression(node) ? node.name.text : ts.isStringLiteralLike(node.argumentExpression) ? node.argumentExpression.text : undefined;
					if (key) this.written.add(key);
					const target = this.checker.getTypeAtLocation(node.expression);
					if (key && keys.has(key) && parts(target).some(open)) this.openWrite(key, node);
					const match = key && keys.has(key) ? matching([this.checker.getNonNullableType(target)]) : undefined;
					if (match && (match.names.size || match.parts.size)) this.addSite({ names: match.names, parts: match.parts, node, type: this.checker.getTypeAtLocation(node.parent.right), assignedKey: key, at: sourceSpan(source, node.parent) });
				}
				const slot = this.typedSlot(node);
				if (!slot) return;
				const match = matching(slot.contexts);
				if (match.names.size || match.parts.size) this.addSite({ names: match.names, parts: match.parts, node, type: slot.type, at: sourceSpan(source, node) });
				// An array handed to a `T[]` slot carries its elements into `T`: the element type is the site.
				// An array literal's elements are slots of their own, so its flattened element type adds nothing.
				if (ts.isArrayLiteralExpression(node)) return;
				const elements = new Set(match.all.flatMap(part => { const element = part.getNumberIndexType(); return element ? parts(this.checker.getNonNullableType(element)).filter(item => containers.has(item)) : []; }));
				if (elements.size) {
					// A slot typed `T[] | undefined` carries its elements through each array part.
					const carriers = parts(this.checker.getNonNullableType(slot.type)).flatMap(part => { const element = part.getNumberIndexType(); return element ? [element] : []; });
					for (const element of carriers.length ? carriers : [this.checker.getAnyType()]) this.addSite({ names: new Set(), parts: elements, node, type: element, at: sourceSpan(source, node) });
				}
			});
		}
	}

	private openWrite(key: string, node: ts.Node): void {
		const exclusion = this.reach?.exclusion(node);
		if (exclusion) this.excludedWrites.push({ key, at: sourceSpan(this.source, node), exclusion });
		else this.openWrites.add(key);
	}

	private addSite(site: Site): void {
		const exclusion = this.reach?.exclusion(site.node);
		if (exclusion) this.excludedSites.push({ site, exclusion });
		else this.sites.push(site);
	}

	private holds(site: Site, member: Member): boolean {
		return (site.assignedKey === undefined || site.assignedKey === member.key)
			&& ([...member.containers].some(container => site.parts.has(container)) || member.topLevel && site.names.has(member.typeName));
	}

	/** The sites holding a member's container. */
	sitesOf(member: Member): Site[] {
		return this.sites.filter(site => this.holds(site, member));
	}

	/** What the session can never run and would otherwise count against the member's values. */
	excludedFor(member: Member): Array<Record<string, unknown>> {
		return [
			...this.excludedSites.filter(({ site }) => this.holds(site, member)).map(({ site, exclusion }) => ({ at: site.at, kind: "typed-site", exclusion })),
			...this.excludedWrites.filter(({ key }) => key === member.key).map(({ at, exclusion }) => ({ at, kind: "open-write", exclusion })),
		];
	}

	/** The values a site can put under the member's key: an empty set when it cannot hold the key at all. */
	valuesAt(site: Site, member: Member): Values {
		if (site.assignedKey !== undefined) return this.valuesOf(site.type);
		const literal = ts.isParenthesizedExpression(site.node) ? site.node.expression : site.node;
		if (!ts.isObjectLiteralExpression(literal)) return this.carried(site.type, member);
		return this.literalValues(literal, member, 0);
	}

	/** A literal's flattened type merges what its spreads carried; only its own members and the spread operands decide. */
	private literalValues(literal: ts.ObjectLiteralExpression, member: Member, depth: number): Values {
		return merge(literal.properties.map(property => {
			if (ts.isSpreadAssignment(property)) return this.spreadValues(property.expression, member, depth);
			if (!property.name || this.source.name(property.name) !== member.key) return new Set();
			if (ts.isPropertyAssignment(property)) return this.scalarValues(property.initializer, depth);
			// The shorthand's own name carries the flow-narrowed type: `...(busy ? { busy } : {})` holds only a truthy value.
			return ts.isShorthandPropertyAssignment(property) ? this.scalarValues(property.name, depth) : "open";
		}));
	}

	/** The values an expression placed under the key can hold, following a plain parameter to every call site's argument. */
	private scalarValues(expression: ts.Expression, depth: number): Values {
		const narrowed = this.valuesOf(this.checker.getTypeAtLocation(expression));
		const parameter = this.parameterOf(expression);
		if (!parameter || narrowed !== "open" && narrowed.size === 0) return narrowed;
		const supplied = this.argumentValues(parameter, argument => argument ? this.scalarValues(argument, depth + 1) : new Set(), depth);
		// The body may narrow the parameter (`busy ? { busy } : {}`); only values both sides allow can arrive.
		return supplied === "open" ? narrowed : narrowed === "open" ? supplied : new Set([...supplied].filter(value => narrowed.has(value)));
	}

	/** What spreading an expression into the literal carries under the key. */
	private spreadValues(expression: ts.Expression, member: Member, depth: number): Values {
		if (ts.isParenthesizedExpression(expression)) return this.spreadValues(expression.expression, member, depth);
		if (ts.isObjectLiteralExpression(expression)) return this.literalValues(expression, member, depth);
		if (ts.isConditionalExpression(expression)) return merge([this.spreadValues(expression.whenTrue, member, depth), this.spreadValues(expression.whenFalse, member, depth)]);
		if (ts.isBinaryExpression(expression) && [ts.SyntaxKind.QuestionQuestionToken, ts.SyntaxKind.BarBarToken, ts.SyntaxKind.AmpersandAmpersandToken].includes(expression.operatorToken.kind)) {
			return merge([this.spreadValues(expression.left, member, depth), this.spreadValues(expression.right, member, depth)]);
		}
		const declared = this.carried(this.checker.getTypeAtLocation(expression), member);
		if (declared !== "open" && declared.size === 0) return declared;
		const followed = this.followedValues(expression, member, depth);
		if (followed) return followed;
		const parameter = this.parameterOf(expression);
		if (!parameter) return declared;
		const supplied = this.argumentValues(parameter, argument => argument ? this.spreadValues(argument, member, depth + 1) : new Set(), depth);
		return supplied === "open" ? declared : declared === "open" ? supplied : new Set([...supplied].filter(value => declared.has(value)));
	}

	/**
	 * What a spread operand holds when it is a call of a function that cannot be overridden (a function, an
	 * arrow constant or a `#private` method), or a `const` with an initializer: the returned or initial
	 * expressions decide, not the declared type. Anything else is not followed.
	 */
	private followedValues(expression: ts.Expression, member: Member, depth: number): Values | undefined {
		if (depth > 4) return undefined;
		if (ts.isIdentifier(expression)) {
			const declaration = this.checker.getSymbolAtLocation(expression)?.valueDeclaration;
			const constant = declaration && ts.isVariableDeclaration(declaration) && ts.isVariableDeclarationList(declaration.parent) && declaration.parent.flags & ts.NodeFlags.Const;
			return constant && declaration.initializer ? this.spreadValues(declaration.initializer, member, depth + 1) : undefined;
		}
		if (!ts.isCallExpression(expression)) return undefined;
		const declaration = this.checker.getResolvedSignature(expression)?.getDeclaration();
		const fn = declaration && (ts.isFunctionDeclaration(declaration) || ts.isArrowFunction(declaration) || ts.isMethodDeclaration(declaration) && ts.isPrivateIdentifier(declaration.name)) ? declaration : undefined;
		if (!fn?.body) return undefined;
		const returned: ts.Expression[] = [];
		const collect = (node: ts.Node): void => {
			if (ts.isReturnStatement(node)) { if (node.expression) returned.push(node.expression); return; }
			if (node !== fn.body && (ts.isFunctionLike(node) || ts.isClassLike(node))) return;
			ts.forEachChild(node, collect);
		};
		if (ts.isBlock(fn.body)) collect(fn.body); else returned.push(fn.body);
		return merge(returned.map(item => this.spreadValues(item, member, depth + 1)));
	}

	private parameterOf(expression: ts.Expression): ts.ParameterDeclaration | undefined {
		if (!ts.isIdentifier(expression)) return undefined;
		const symbol = ts.isShorthandPropertyAssignment(expression.parent) && expression.parent.name === expression ? this.checker.getShorthandAssignmentValueSymbol(expression.parent) : this.checker.getSymbolAtLocation(expression);
		const declaration = symbol?.valueDeclaration;
		return declaration && ts.isParameter(declaration) && ts.isIdentifier(declaration.name) && !declaration.dotDotDotToken ? declaration : undefined;
	}

	/**
	 * The union of what every call passes for a parameter. A function that is called only by name, has the
	 * parameter in a fixed position and never reassigns it is bounded by its arguments; any other use of the
	 * function (passed as a value, spread arguments, a callback) leaves the parameter open.
	 */
	private argumentValues(parameter: ts.ParameterDeclaration, project: (argument: ts.Expression | undefined) => Values, depth: number): Values {
		const fn = parameter.parent;
		if (depth > 4 || !(ts.isFunctionDeclaration(fn) || ts.isMethodDeclaration(fn) || ts.isArrowFunction(fn) || ts.isFunctionExpression(fn))) return "open";
		const holder = ts.isFunctionDeclaration(fn) || ts.isMethodDeclaration(fn) ? fn : ts.isVariableDeclaration(fn.parent) && fn.parent.initializer === fn ? fn.parent : undefined;
		const name = holder?.name;
		if (!holder || !name || !ts.isIdentifier(name) || this.reassigned(parameter)) return "open";
		const index = fn.parameters.indexOf(parameter);
		const uses = this.identifiers(name.text).filter(use => use !== name && !ts.isImportSpecifier(use.parent) && !ts.isExportSpecifier(use.parent) && this.source.declaration(use) === holder);
		if (!uses.length) return "open";
		const projected: Values[] = [];
		for (const use of uses) {
			const callee = ts.isPropertyAccessExpression(use.parent) && use.parent.name === use ? use.parent : use;
			const call = callee.parent;
			if (!ts.isCallExpression(call) || call.expression !== callee || call.arguments.slice(0, index + 1).some(ts.isSpreadElement)) return "open";
			projected.push(call.arguments[index] ? project(call.arguments[index]) : parameter.initializer ? project(parameter.initializer) : project(undefined));
		}
		return merge(projected);
	}

	private reassigned(parameter: ts.ParameterDeclaration): boolean {
		const symbol = this.checker.getSymbolAtLocation(parameter.name);
		let found = false;
		this.source.walk(parameter.parent, node => { if (assignment(node) && ts.isIdentifier(node.left) && this.checker.getSymbolAtLocation(node.left) === symbol) found = true; });
		return found;
	}

	private identifierIndex: Map<string, ts.Identifier[]> | undefined;
	private identifiers(text: string): ts.Identifier[] {
		if (!this.identifierIndex) {
			this.identifierIndex = new Map();
			for (const file of this.source.files) this.source.walk(file, node => {
				if (ts.isIdentifier(node)) { const list = this.identifierIndex!.get(node.text) ?? []; list.push(node); this.identifierIndex!.set(node.text, list); }
			});
		}
		return this.identifierIndex.get(text) ?? [];
	}

	/** What a value of `type` carries under the key, except as a pass-through of an already-typed container. */
	private carried(type: ts.Type, member: Member): Values {
		if (named(type) === member.typeName || member.containers.has(type) || this.wireUnionHolding(type, member)) return new Set();
		return merge((type.isUnion() ? type.types : [type]).map(item => {
			if (parts(item).some(part => named(part) === member.typeName || member.containers.has(part))) return new Set();
			return merge(parts(item).map(part => {
				// An array's number index holds its elements, which are sites of their own; only a string index can carry the key.
				if (part.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown) || this.checker.getIndexInfosOfType(part).some(info => !(info.keyType.flags & ts.TypeFlags.NumberLike))) return "open";
				const property = this.checker.getPropertyOfType(part, member.key);
				return property ? this.valuesOf(this.checker.getTypeOfSymbol(property)) : new Set();
			}));
		}));
	}

	/**
	 * A union of the wire's own props types that includes the member's container is any node's props, handed
	 * back to a props slot (`{ ...described.p, hidden: true }`). Each kind's values were typed where it was built;
	 * a sibling kind's same-named key does not reach this container. A member of the union that is not a wire
	 * type (an ad-hoc object, `any`) leaves the spread open.
	 */
	private wireUnionHolding(type: ts.Type, member: Member): boolean {
		if (!type.isUnion()) return false;
		const items = type.types.filter(item => !(item.flags & (ts.TypeFlags.Undefined | ts.TypeFlags.Null)));
		const declaredInWire = (part: ts.Type) => (part.aliasSymbol ?? part.getSymbol())?.declarations?.some(declaration => this.source.relative(declaration.getSourceFile()) === WIRE);
		return items.some(item => parts(item).some(part => named(part) === member.typeName || member.containers.has(part)))
			&& items.every(item => parts(item).some(declaredInWire));
	}

	private valuesOf(type: ts.Type): Values {
		const values = new Set<unknown>();
		for (const part of parts(type)) {
			if (part.flags & (ts.TypeFlags.Undefined | ts.TypeFlags.Void | ts.TypeFlags.Never)) continue;
			if (part.flags & ts.TypeFlags.Null) values.add(null);
			else if (part.flags & ts.TypeFlags.BooleanLiteral) values.add(this.checker.typeToString(part) === "true");
			else if (part.isStringLiteral() || part.isNumberLiteral()) values.add(part.value);
			else return "open";
		}
		return values;
	}

	/** The type a value slot expects and the type of the value placed in it; a `{ name }` shorthand is a slot too. */
	private typedSlot(node: ts.Node): { contexts: ts.Type[]; type: ts.Type } | undefined {
		if (ts.isShorthandPropertyAssignment(node) && ts.isObjectLiteralExpression(node.parent)) {
			const outer = this.checker.getContextualType(node.parent);
			const value = this.checker.getShorthandAssignmentValueSymbol(node);
			const contexts = outer ? parts(outer).flatMap(part => { const property = this.checker.getPropertyOfType(part, node.name.text); return property ? [this.checker.getTypeOfSymbolAtLocation(property, node)] : []; }) : [];
			return contexts.length && value ? { contexts, type: this.checker.getTypeOfSymbolAtLocation(value, node) } : undefined;
		}
		if (!ts.isExpression(node) || !valueSlot(node)) return undefined;
		const context = this.checker.getContextualType(node);
		return context ? { contexts: [context], type: this.checker.getTypeAtLocation(node) } : undefined;
	}

	// `JSON.stringify({ changed })`: a standard-library call returning a primitive keeps nothing of its argument.
	// A literal nested in that argument (a spread's branch, a member's object or array) is serialized with it.
	private serializedByLibrary(literal: ts.Expression): boolean {
		let outer: ts.Node = literal;
		while (ts.isParenthesizedExpression(outer.parent) || ts.isSpreadAssignment(outer.parent) || ts.isConditionalExpression(outer.parent) && outer.parent.condition !== outer
			|| ts.isBinaryExpression(outer.parent) && outer.parent.right === outer || ts.isPropertyAssignment(outer.parent) && outer.parent.initializer === outer
			|| ts.isObjectLiteralExpression(outer.parent) || ts.isArrayLiteralExpression(outer.parent)) outer = outer.parent;
		const call = outer.parent;
		if (!ts.isCallExpression(call) || !call.arguments.includes(outer as ts.Expression)) return false;
		const declaration = this.checker.getResolvedSignature(call)?.getDeclaration();
		const library = declaration && /[\\/]typescript[\\/]lib[\\/]lib\.[^\\/]*\.d\.ts$/.test(declaration.getSourceFile().fileName);
		return !!library && !!(this.checker.getTypeAtLocation(call).flags & (ts.TypeFlags.StringLike | ts.TypeFlags.NumberLike | ts.TypeFlags.BooleanLike | ts.TypeFlags.Void));
	}
}

function merge(values: Values[]): Values {
	if (values.includes("open")) return "open";
	return new Set(values.flatMap(set => [...(set as Set<unknown>)]));
}

/** The member's key and the types that contain it; only named members and `[]` steps resolve. */
function memberResolver(source: SourceIndex): (entry: InventoryEntry) => Member | undefined {
	const checker = source.checker;
	const wire = source.files.find(file => source.relative(file) === WIRE);
	const declared = new Map<string, ts.Type>();
	if (wire) for (const statement of wire.statements) {
		if ((ts.isInterfaceDeclaration(statement) || ts.isTypeAliasDeclaration(statement)) && !statement.typeParameters) declared.set(statement.name.text, checker.getTypeAtLocation(statement.name));
	}
	return entry => {
		const { declaringType: typeName, path } = entry.details;
		if (typeof typeName !== "string" || typeof path !== "string" || !/^[A-Za-z_$][\w$]*(?:\[\]|\.[A-Za-z_$][\w$]*)*$/.test(path)) return undefined;
		const steps = path.match(/[A-Za-z_$][\w$]*|\[\]/g) ?? [];
		const key = steps.pop();
		const root = declared.get(typeName);
		if (!key || key === "[]" || !root || !wire) return undefined;
		let containers = parts(checker.getNonNullableType(root));
		for (const step of steps) containers = containers.flatMap(type => {
			const property = step === "[]" ? undefined : checker.getPropertyOfType(type, step);
			const next = step === "[]" ? type.getNumberIndexType() : property ? checker.getTypeOfSymbolAtLocation(property, wire) : undefined;
			return next ? parts(checker.getNonNullableType(next)) : [];
		});
		return containers.length ? { typeName, key, topLevel: path === key, containers: new Set(containers) } : undefined;
	};
}

/**
 * Mark UI members no producer writes, then UI enum values no typed site can
 * hold. A key is unproduced when no source writes it, or when every typed
 * site lacks it; a value is unproduced when every typed site bounds the key
 * to other literals.
 */
export function classifyUnproduced(source: SourceIndex, entries: InventoryEntry[]): void {
	const resolve = memberResolver(source);
	const reach = new SiteReach(source);
	// An inherited emitter carries the enclosing value, not proof that this key is in it;
	// the typed-site proof decides whether a carried value can hold it. An emitter in code no
	// session runs is not a producer either.
	const ownEmitter = (emitter: unknown) => record(emitter) && emitter.inheritedFrom === undefined
		&& !(typeof emitter.source === "string" && typeof emitter.start === "number" && reach.exclusionAt(emitter.source, emitter.start));
	const unemitted = (entry: InventoryEntry) => !(Array.isArray(entry.details.uiEmitters) && entry.details.uiEmitters.some(ownEmitter))
		&& !(Array.isArray(entry.details.emitters) && entry.details.emitters.some(emitter => ownEmitter(emitter) && record(emitter) && emitter.role !== "typed-property-read"));
	const members = new Map(entries.flatMap(entry => {
		if (entry.category !== "property" || entry.details.exposure !== "ui") return [];
		if (!unemitted(entry) && !(Array.isArray(entry.details.enumValues) && entry.details.enumValues.length)) return [];
		const member = resolve(entry);
		return member ? [[entry, member] as const] : [];
	}));
	if (!members.size) return;
	const index = new TypedSites(source, members.values(), reach);
	const proofBase = (entry: InventoryEntry, key: string, member: Member) => {
		const excludedSites = index.excludedFor(member);
		return { key, declarations: entry.details.declarations, searchedFiles: index.searched, excluded: WIRE, ...excludedSites.length ? { excludedSites } : {} };
	};
	for (const [entry, member] of members) {
		const sites = index.sitesOf(member);
		const excludedHere = index.excludedFor(member).length > 0;
		const values = index.openWrites.has(member.key) || !sites.length && !excludedHere ? "open" : merge(sites.map(site => index.valuesAt(site, member)));
		if (unemitted(entry) && (!index.written.has(member.key) || values !== "open" && values.size === 0)) {
			entry.details.exposure = "unproduced";
			delete entry.details.uiEmitters;
			entry.details.unproducedProof = !index.written.has(member.key)
				? { ...proofBase(entry, member.key, member), proof: "key-never-written", writeForms: ["object-literal-member", "assignment"] }
				: { ...proofBase(entry, member.key, member), proof: excludedHere ? "every-reachable-typed-site-lacks-key" : "every-typed-site-lacks-key", typedSites: sites.map(site => site.at) };
			continue;
		}
		if (values === "open" || !Array.isArray(entry.details.enumValues)) continue;
		const missing = entry.details.enumValues.filter(value => !values.has(value));
		if (missing.length) entry.details.unproducedValues = { ...proofBase(entry, member.key, member), values: missing, proof: "every-typed-site-bounds-value", typedSites: sites.map(site => site.at) };
	}
}

/** Every identifier and string literal by text: a call, a value use, a re-export and a computed key all spell the name. */
export function nameOccurrences(source: SourceIndex): Map<string, ts.Node[]> {
	const index = new Map<string, ts.Node[]>();
	for (const file of source.files) source.walk(file, node => {
		if (!ts.isIdentifier(node) && !ts.isPrivateIdentifier(node) && !ts.isStringLiteral(node) && !ts.isNoSubstitutionTemplateLiteral(node)) return;
		const list = index.get(node.text);
		if (list) list.push(node); else index.set(node.text, [node]);
	});
	return index;
}

/**
 * Producers nothing can run: each emitter sits inside a named function whose
 * name occurs nowhere in the source except its own declaration. Returns the
 * located proof, or undefined when any emitter's function is spelled elsewhere.
 */
export function unreferencedProducers(source: SourceIndex, producers: readonly ts.Node[], index: ReadonlyMap<string, readonly ts.Node[]>): Record<string, unknown>[] | undefined {
	if (!producers.length) return undefined;
	const proof: Record<string, unknown>[] = [];
	for (const producer of producers) {
		let dead: ts.Identifier | undefined;
		for (let node: ts.Node | undefined = producer; node && !dead; node = node.parent) {
			const name = ts.isFunctionDeclaration(node) || ts.isMethodDeclaration(node) ? node.name
				: (ts.isArrowFunction(node) || ts.isFunctionExpression(node)) && ts.isVariableDeclaration(node.parent) ? node.parent.name : undefined;
			if (name && ts.isIdentifier(name) && (index.get(name.text) ?? []).every(occurrence => occurrence === name)) dead = name;
		}
		if (!dead) return undefined;
		proof.push({ producer: sourceSpan(source, producer), function: { ...source.origin(dead), name: dead.text }, occurrences: 1 });
	}
	return proof;
}
