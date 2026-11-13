import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";
import { record, SourceIndex } from "./inventory-source";
import { resolveHook } from "./inventory-hooks";
import { sourceSpan } from "./inventory-setting-controls";
import type { SourceSpan } from "./inventory-setting-controls";

type BodyFunction = ts.FunctionDeclaration | ts.FunctionExpression | ts.ArrowFunction | ts.MethodDeclaration | ts.GetAccessorDeclaration | ts.SetAccessorDeclaration | ts.ConstructorDeclaration;
interface CallEdge { target: BodyFunction; call: Omit<SourceSpan, "expression">; role: string }
interface Root { component: InventoryEntry; name: string; implementation: BodyFunction }
export interface SourceEmitter { node: ts.Node; role: string }
const functionBody = (node: ts.Node): node is BodyFunction => (ts.isFunctionDeclaration(node) || ts.isFunctionExpression(node) || ts.isArrowFunction(node) || ts.isMethodDeclaration(node) || ts.isGetAccessorDeclaration(node) || ts.isSetAccessorDeclaration(node) || ts.isConstructorDeclaration(node)) && !!node.body;
const terminal = (name: string): boolean => name === "render" || /^render[A-Z]/.test(name);
const ttyInput = (name: string): boolean => /^(?:handleInput|handleMouse|handleKey|onKey|onMouse)/.test(name);

/** Static possible paths are provenance only; the coverage gate still requires the actual runtime visits. */
export class SourceFlowIndex {
	private readonly functions = new Map<string, BodyFunction>();
	private readonly edges = new Map<BodyFunction, CallEdge[]>();
	/** Calls through a base member to the overriding or implementing members a receiver may supply. */
	private readonly virtualEdges = new Map<BodyFunction, CallEdge[]>();
	private readonly overriders = new Map<ts.Node, BodyFunction[]>();
	private readonly native: Root[] = [];
	private readonly nativeEvents: Root[] = [];
	private readonly tty: Root[] = [];
	private readonly paths = new Map<BodyFunction, Map<BodyFunction, CallEdge[]>>();
	private readonly virtualPaths = new Map<BodyFunction, Map<BodyFunction, CallEdge[]>>();
	constructor(private readonly source: SourceIndex, components: readonly InventoryEntry[]) {
		for (const file of source.files) source.walk(file, node => {
			if (functionBody(node)) this.functions.set(`${source.relative(node)}:${node.getStart()}`, node);
			if (ts.isClassLike(node)) this.indexClassOverrides(node);
			else if (ts.isObjectLiteralExpression(node)) this.indexLiteralImplementations(node);
		});
		for (const file of source.files) source.walk(file, node => {
			const caller = this.enclosing(node.parent);
			if (!caller) return;
			if (ts.isCallExpression(node) || ts.isNewExpression(node)) {
				const expression = node.expression;
				const declaration = source.declaration(ts.isPropertyAccessExpression(expression) ? expression.name : expression);
				const resolved = declaration ? resolveHook(source, declaration) : undefined;
				if (resolved && functionBody(resolved)) this.edge(this.edges, caller, resolved, node, "source-call");
				if (resolved && (ts.isClassDeclaration(resolved) || ts.isClassExpression(resolved))) for (const member of resolved.members) if (ts.isConstructorDeclaration(member) && functionBody(member)) this.edge(this.edges, caller, member, node, "source-construction");
				for (const argument of node.arguments ?? []) if (functionBody(argument)) this.edge(this.edges, caller, argument, node, "callback-argument");
				if (declaration && ts.isPropertyAccessExpression(expression)) for (const target of this.overriders.get(declaration) ?? []) if (target !== resolved) this.edge(this.virtualEdges, caller, target, node, "virtual-dispatch");
			} else if (ts.isPropertyAccessExpression(node) && !this.write(node)) {
				const declaration = source.declaration(node.name);
				if (declaration && ts.isGetAccessorDeclaration(declaration) && functionBody(declaration)) this.edge(this.edges, caller, declaration, node, "source-getter");
			}
		});
		for (const component of components) if (component.category === "component") for (const hook of Array.isArray(component.details.hooks) ? component.details.hooks : []) {
			if (!record(hook) || typeof hook.name !== "string" || !record(hook.implementation)) continue;
			const implementation = this.functions.get(`${hook.implementation.source}:${hook.implementation.start}`);
			if (!implementation) continue;
			const root = { component, name: hook.name, implementation };
			if (["describe", "describeSurface", "describeScreen"].includes(hook.name)) this.native.push(root);
			if (hook.name === "handleNativeEvent") this.nativeEvents.push(root);
			if (terminal(hook.name) || ttyInput(hook.name)) this.tty.push(root);
		}
	}
	private location(node: ts.Node): Omit<SourceSpan, "expression"> { return { ...this.source.origin(node), start: node.getStart(), end: node.getEnd() }; }
	private edge(edges: Map<BodyFunction, CallEdge[]>, caller: BodyFunction, target: BodyFunction, call: ts.Node, role: string): void {
		const list = edges.get(caller) ?? [];
		if (!list.some(edge => edge.target === target && edge.call.start === call.getStart() && edge.call.source === this.source.relative(call))) list.push({ target, call: this.location(call), role });
		edges.set(caller, list);
	}
	private overrides(base: ts.Node, member: ts.Node): void {
		const implementation = resolveHook(this.source, member);
		if (base === member || !functionBody(implementation)) return;
		const list = this.overriders.get(base) ?? [];
		if (!list.includes(implementation)) list.push(implementation);
		this.overriders.set(base, list);
	}
	/** A class member overrides every same-named member of its base classes and implemented interfaces. */
	private indexClassOverrides(node: ts.ClassLikeDeclaration): void {
		const bases: ts.Type[] = [];
		const pending: ts.Type[] = (node.heritageClauses ?? []).flatMap(clause => clause.types.map(type => this.source.checker.getTypeAtLocation(type)));
		while (pending.length) {
			const base = pending.pop()!;
			if (bases.includes(base)) continue;
			bases.push(base);
			if (base.isClassOrInterface()) pending.push(...this.source.checker.getBaseTypes(base));
		}
		for (const member of node.members) {
			const name = this.source.name(member.name);
			if (!name) continue;
			for (const base of bases) for (const declaration of this.source.checker.getPropertyOfType(base, name)?.declarations ?? []) this.overrides(declaration, member);
		}
	}
	/** An object literal typed as an interface implements that interface's members. */
	private indexLiteralImplementations(node: ts.ObjectLiteralExpression): void {
		const context = this.source.checker.getContextualType(node);
		if (!context) return;
		for (const property of node.properties) {
			if (!ts.isPropertyAssignment(property) && !ts.isMethodDeclaration(property) && !ts.isShorthandPropertyAssignment(property)) continue;
			const name = this.source.name(property.name);
			if (!name) continue;
			for (const type of context.isUnion() ? context.types : [context]) for (const declaration of this.source.checker.getPropertyOfType(type, name)?.declarations ?? []) this.overrides(declaration, property);
		}
	}
	private write(node: ts.Node): boolean {
		return ts.isBinaryExpression(node.parent) && node.parent.left === node && node.parent.operatorToken.kind >= ts.SyntaxKind.FirstAssignment && node.parent.operatorToken.kind <= ts.SyntaxKind.LastAssignment;
	}
	private enclosing(node: ts.Node | undefined): BodyFunction | undefined {
		for (let current = node; current; current = current.parent) if (functionBody(current)) return current;
		return undefined;
	}
	private reach(root: BodyFunction, virtual = false): Map<BodyFunction, CallEdge[]> {
		const cache = virtual ? this.virtualPaths : this.paths;
		const previous = cache.get(root);
		if (previous) return previous;
		const found = new Map<BodyFunction, CallEdge[]>([[root, []]]);
		const pending = [root];
		for (let index = 0; index < pending.length; index++) {
			const current = pending[index]!;
			const edges = virtual ? [...this.edges.get(current) ?? [], ...this.virtualEdges.get(current) ?? []] : this.edges.get(current) ?? [];
			for (const edge of edges) if (!found.has(edge.target)) { found.set(edge.target, [...found.get(current)!, edge]); pending.push(edge.target); }
		}
		cache.set(root, found);
		return found;
	}
	/** What `root` reaches without entering a native event handler. */
	private reachOutsideNativeEvents(root: BodyFunction): Set<BodyFunction> {
		const barriers = new Set(this.nativeEvents.map(event => event.implementation));
		const found = new Set<BodyFunction>([root]);
		const pending = [root];
		for (let index = 0; index < pending.length; index++) {
			for (const edge of this.edges.get(pending[index]!) ?? []) if (!found.has(edge.target) && !barriers.has(edge.target)) { found.add(edge.target); pending.push(edge.target); }
		}
		return found;
	}
	private eventOrigins: { proven: boolean; translated: SourceSpan[]; ttyOrigins: SourceSpan[] } | undefined;
	/**
	 * Every `NativeUiEvent` value a source creates (a literal of that type, a
	 * cast, an untyped argument). Each must translate a wire `TspEvent` its
	 * enclosing function receives as a parameter — so it exists only because the
	 * native transport delivered an event — or sit outside what a TTY hook
	 * reaches. Then a TTY path into a native event handler only replays a
	 * delivered event (a queue drained after a paste), so a read there is a
	 * native consumer, not a TTY read.
	 */
	private nativeEventOrigins(): { proven: boolean; translated: SourceSpan[]; ttyOrigins: SourceSpan[] } {
		if (this.eventOrigins) return this.eventOrigins;
		const checker = this.source.checker;
		const unionOf = (file: string, name: string): { alias: ts.Type; members: Set<ts.Type> } => {
			const declaration = this.source.named(file, name);
			const alias = checker.getTypeAtLocation(ts.isTypeAliasDeclaration(declaration) ? declaration.name : declaration);
			return { alias, members: new Set(alias.isUnion() ? alias.types : [alias]) };
		};
		const event = unionOf("packages/tui/src/native/node.ts", "NativeUiEvent");
		const wire = unionOf("packages/wire/src/tsp.ts", "TspEvent");
		const within = (union: { alias: ts.Type; members: Set<ts.Type> }, type: ts.Type | undefined): boolean => !!type && (type === union.alias || (type.isUnion() ? type.types : [type]).some(part => union.members.has(part)));
		const untyped = (type: ts.Type): boolean => !!(type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown));
		const origins: ts.Node[] = [];
		for (const file of this.source.files) this.source.walk(file, node => {
			if (ts.isObjectLiteralExpression(node)) {
				if (within(event, checker.getContextualType(node)) && checker.isTypeAssignableTo(checker.getTypeAtLocation(node), event.alias)) origins.push(node);
			} else if (ts.isAsExpression(node) || ts.isTypeAssertionExpression(node)) {
				if (within(event, checker.getTypeFromTypeNode(node.type))) origins.push(node);
			} else if (ts.isExpression(node) && (ts.isCallExpression(node.parent) || ts.isNewExpression(node.parent)) && node.parent.arguments?.includes(node)
				&& within(event, checker.getContextualType(node)) && untyped(checker.getTypeAtLocation(node))) origins.push(node);
		});
		const wireEvent = (parameter: ts.ParameterDeclaration): boolean => {
			const type = checker.getTypeAtLocation(parameter);
			return ts.isIdentifier(parameter.name) && !untyped(type) && (type.isUnion() ? type.types : [type]).every(part => wire.members.has(part));
		};
		const translates = (node: ts.Node): boolean => {
			for (let owner = this.enclosing(node); owner; owner = this.enclosing(owner.parent)) if (owner.parameters.some(wireEvent)) return true;
			return false;
		};
		const ttyReached = this.tty.map(root => this.reachOutsideNativeEvents(root.implementation));
		const translated = origins.filter(translates);
		const ttyOrigins = origins.filter(node => { if (translated.includes(node)) return false; const owner = this.enclosing(node); return !owner || ttyReached.some(reached => reached.has(owner)); });
		this.eventOrigins = { proven: origins.length > 0 && ttyOrigins.length === 0, translated: translated.map(node => sourceSpan(this.source, node)), ttyOrigins: ttyOrigins.map(node => sourceSpan(this.source, node)) };
		return this.eventOrigins;
	}
	private flow(root: Root, target: BodyFunction, virtual = false): Record<string, unknown>[] | undefined {
		return this.reach(root.implementation, virtual).get(target)?.map(edge => ({ ...edge.call, role: edge.role, target: this.location(edge.target) }));
	}
	private referenceIndex: Map<string, Array<ts.Identifier | ts.PrivateIdentifier>> | undefined;
	private readonly referenceCache = new Map<ts.Node, Array<ts.Identifier | ts.PrivateIdentifier>>();
	/** Identifier uses of a constant or field, excluding its own declaration name. */
	private references(declaration: ts.VariableDeclaration | ts.PropertyDeclaration): Array<ts.Identifier | ts.PrivateIdentifier> {
		const cached = this.referenceCache.get(declaration);
		if (cached) return cached;
		const name = declaration.name;
		if (!ts.isIdentifier(name) && !ts.isPrivateIdentifier(name)) return [];
		if (!this.referenceIndex) {
			this.referenceIndex = new Map();
			for (const file of this.source.files) this.source.walk(file, node => {
				if (ts.isIdentifier(node) || ts.isPrivateIdentifier(node)) { const list = this.referenceIndex!.get(node.text) ?? []; list.push(node); this.referenceIndex!.set(node.text, list); }
			});
		}
		const uses = (this.referenceIndex.get(name.text) ?? []).filter(use => use !== name && this.source.declaration(use) === declaration);
		this.referenceCache.set(declaration, uses);
		return uses;
	}
	/**
	 * The functions that run an emitter. An emitter in a function body runs in that function. One in a
	 * module constant or class field initializer runs wherever the constant is used, so it follows the
	 * constant's references (through further constants) to the functions that use it. An initializer
	 * nothing references has no producer.
	 */
	private producers(node: ts.Node, seen = new Set<ts.Node>()): BodyFunction[] {
		const own = this.enclosing(node);
		if (own) return [own];
		for (let holder = node.parent; holder; holder = holder.parent) {
			if (!ts.isVariableDeclaration(holder) && !ts.isPropertyDeclaration(holder)) continue;
			if (seen.has(holder)) return [];
			seen.add(holder);
			return [...new Set(this.references(holder).flatMap(use => this.producers(use, seen)))];
		}
		return [];
	}
	uiEmitters(emitters: readonly SourceEmitter[]): Record<string, unknown>[] {
		const result: Record<string, unknown>[] = [];
		for (const emitter of emitters) {
			if (!["typed-property-emitter", "typed-spread-emitter", "typed-property-write", "typed-kind-producer"].includes(emitter.role)) continue;
			for (const producer of this.producers(emitter.node)) {
				const direct = this.native.flatMap(root => { const path = this.flow(root, producer); return path ? [{ root, path }] : []; });
				// Without a static call path, the nearest roots through dispatch on a base member are the
				// candidates; every container's describe reaches the producer too, only by longer paths.
				const dispatched = direct.length ? [] : this.native.flatMap(root => { const path = this.flow(root, producer, true); return path ? [{ root, path }] : []; });
				const nearest = Math.min(...dispatched.map(candidate => candidate.path.length));
				for (const { root, path } of direct.length ? direct : dispatched.filter(candidate => candidate.path.length === nearest)) {
					const terminalHooks = this.tty.filter(hook => hook.component.id === root.component.id && terminal(hook.name)).map(hook => ({ ...this.location(hook.implementation), component: hook.component.id, method: hook.name }));
					result.push({ ...sourceSpan(this.source, emitter.node), role: emitter.role, component: root.component.id, nativeHook: { ...this.location(root.implementation), method: root.name }, terminalHooks, path });
				}
			}
		}
		return result;
	}
	nativeContract(declarations: readonly ts.Node[], emitters: readonly SourceEmitter[]): Record<string, unknown> | undefined {
		const tags = declarations.flatMap(declaration => ts.getJSDocTags(declaration).filter(tag => tag.tagName.text === "nativeContract").map(tag => ({ declaration, tag })));
		if (tags.length === 0) return undefined;
		const producers = emitters.filter(emitter => ["typed-property-emitter", "typed-spread-emitter", "typed-property-write"].includes(emitter.role));
		const fields = new Set<ts.Declaration>();
		const visited = new Set<ts.Node>();
		const dependencies = (node: ts.Node): void => {
			if (visited.has(node)) return;
			visited.add(node);
			this.source.walk(node, expression => {
				const declaration = ts.isPropertyAccessExpression(expression) ? this.source.declaration(expression.name) : ts.isIdentifier(expression) ? this.source.declaration(expression) : undefined;
				if (!declaration || this.source.relative(declaration) === "packages/wire/src/tsp.ts") return;
				if (ts.isPropertyDeclaration(declaration) || ts.isGetAccessorDeclaration(declaration)) fields.add(declaration);
				if (ts.isVariableDeclaration(declaration) && declaration.initializer) dependencies(declaration.initializer);
				// A getter helper (`getNativeEditAck() { return this.#nativeAck; }`) hands out its backing field: its returned expression is the dependency.
				else if ((ts.isMethodDeclaration(declaration) || ts.isFunctionDeclaration(declaration)) && declaration.body?.statements.length === 1) {
					const [only] = declaration.body.statements;
					if (only && ts.isReturnStatement(only) && only.expression) dependencies(only.expression);
				}
			});
		};
		for (const producer of producers) {
			if (ts.isPropertyAssignment(producer.node)) dependencies(producer.node.initializer);
			else if (ts.isShorthandPropertyAssignment(producer.node)) {
				const target = this.source.checker.getShorthandAssignmentValueSymbol(producer.node)?.valueDeclaration;
				if (target && ts.isVariableDeclaration(target) && target.initializer) dependencies(target.initializer);
			} else dependencies(producer.node);
		}
		const inherited = (type: ts.Type, owner: ts.Node, visited = new Set<ts.Type>()): boolean => {
			if (visited.has(type)) return false;
			visited.add(type);
			if (type.getSymbol()?.declarations?.includes(owner as ts.Declaration)) return true;
			return type.isClassOrInterface() && this.source.checker.getBaseTypes(type).some(base => inherited(base, owner, visited));
		};
		const accessors = [...fields].filter(ts.isGetAccessorDeclaration);
		for (const file of this.source.files) this.source.walk(file, node => {
			if (!ts.isGetAccessorDeclaration(node) || !(ts.isClassDeclaration(node.parent) || ts.isClassExpression(node.parent))) return;
			const type = this.source.checker.getTypeAtLocation(node.parent);
			if (accessors.some(base => this.source.name(base.name) === this.source.name(node.name) && inherited(type, base.parent))) fields.add(node);
		});
		const consumers: Record<string, unknown>[] = [];
		const ttyReads: Record<string, unknown>[] = [];
		const replayedReads: Record<string, unknown>[] = [];
		const unknownReads: SourceSpan[] = [];
		for (const file of this.source.files) this.source.walk(file, node => {
			if (!ts.isPropertyAccessExpression(node)) return;
			const declaration = this.source.declaration(node.name);
			if (!declaration || !fields.has(declaration)) return;
			const owner = this.enclosing(node);
			if (!owner) return;
			const role = this.write(node) ? "write" : "read";
			const nativeConsumers = this.nativeEvents.flatMap(root => { const path = this.flow(root, owner); return path ? [{ component: root.component.id, eventHandler: sourceSpan(this.source, root.implementation), access: sourceSpan(this.source, node), role, path }] : []; });
			consumers.push(...nativeConsumers);
			if (role === "read") {
				const terminals = this.tty.flatMap(root => { const path = this.flow(root, owner); return path ? [{ component: root.component.id, hook: sourceSpan(this.source, root.implementation), access: sourceSpan(this.source, node), path, direct: this.reachOutsideNativeEvents(root.implementation).has(owner) }] : []; });
				const replayed = this.nativeEventOrigins().proven ? terminals.filter(read => !read.direct) : [];
				ttyReads.push(...terminals.filter(read => !replayed.includes(read)));
				replayedReads.push(...replayed);
				if (terminals.length === 0 && nativeConsumers.length === 0 && !this.native.some(root => this.reach(root.implementation).has(owner))) unknownReads.push(sourceSpan(this.source, node));
			}
		});
		const proven = producers.length > 0 && fields.size > 0 && consumers.length > 0 && ttyReads.length === 0 && unknownReads.length === 0;
		const eventOrigins = replayedReads.length ? this.nativeEventOrigins() : undefined;
		return { proven, role: "source-validated-native-only-field", tags: tags.map(({ declaration, tag }) => ({ declaration: sourceSpan(this.source, declaration), tag: sourceSpan(this.source, tag), rationale: typeof tag.comment === "string" ? tag.comment : tag.comment?.map(part => part.text).join("") })), producers: producers.map(producer => sourceSpan(this.source, producer.node)), backingFields: [...fields].map(field => sourceSpan(this.source, field)), consumers, ttyReads, replayedReads, nativeEventTranslations: eventOrigins?.translated, unknownReads };
	}

	/**
	 * A member tagged `@browserOnly`: written for the browser and read by no terminal code. Proven when some
	 * native producer writes it, every typed read of it sits in a function a native root reaches, and no
	 * terminal root reaches any read (nor is any read unreachable from both, which would leave it unplaced).
	 */
	browserOnly(declarations: readonly ts.Node[], emitters: readonly SourceEmitter[]): Record<string, unknown> | undefined {
		const tags = declarations.flatMap(declaration => ts.getJSDocTags(declaration).filter(tag => tag.tagName.text === "browserOnly").map(tag => ({ declaration, tag })));
		if (tags.length === 0) return undefined;
		const producers = emitters.filter(emitter => ["typed-property-emitter", "typed-spread-emitter", "typed-property-write"].includes(emitter.role));
		const nativeReads: Record<string, unknown>[] = [];
		const ttyReads: Record<string, unknown>[] = [];
		const unplacedReads: SourceSpan[] = [];
		for (const read of emitters.filter(emitter => emitter.role === "typed-property-read")) {
			const owner = this.enclosing(read.node);
			const terminals = owner ? this.tty.flatMap(root => { const path = this.flow(root, owner); return path ? [{ component: root.component.id, hook: sourceSpan(this.source, root.implementation), access: sourceSpan(this.source, read.node), path }] : []; }) : [];
			ttyReads.push(...terminals);
			if (terminals.length) continue;
			const natives = owner ? this.native.flatMap(root => { const path = this.flow(root, owner); return path ? [{ component: root.component.id, hook: sourceSpan(this.source, root.implementation), access: sourceSpan(this.source, read.node), path }] : []; }) : [];
			if (natives.length) nativeReads.push(...natives); else unplacedReads.push(sourceSpan(this.source, read.node));
		}
		const proven = producers.length > 0 && ttyReads.length === 0 && unplacedReads.length === 0;
		return { proven, role: "source-validated-browser-only-field", tags: tags.map(({ declaration, tag }) => ({ declaration: sourceSpan(this.source, declaration), tag: sourceSpan(this.source, tag), rationale: typeof tag.comment === "string" ? tag.comment : tag.comment?.map(part => part.text).join("") })), producers: producers.map(producer => sourceSpan(this.source, producer.node)), nativeReads, ttyReads, unplacedReads };
	}
}
