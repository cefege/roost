import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";
import { record, type SourceIndex } from "./inventory-source";
import { appendTo, callOf, INPUT_HOOK, kindOf, memberName, membersOf, NATIVE_HOOK, ownNativeHooks, TERMINAL_HOOK, usesSuper, within } from "./inventory-native-members";
import { entryArguments } from "./inventory-native-entry";
import { literalMemberReach } from "./inventory-native-literals";
import { hookInertUse, type InertUseHost } from "./inventory-native-inert";
import type { PartOwner, PartUse, TerminalPartResult, AbsenceRejection } from "./inventory-native-types";
import { derives, inertReference, readOf } from "./inventory-native-callees";
import { traceValue, type FlowHost } from "./inventory-native-flow";

/**
 * Terminal-only components, resolved against the components that carry them.
 * A part's value flow ends in members of owners whose native hooks never reach
 * it, so the browser shows the owner's description and never the part. Every
 * step is recorded with its source location for the coverage gate to demand
 * owner execution against.
 */

interface Landing { owner: ts.Node; member: ts.Node; site: ts.Node; hops: ts.Node[] }
type Climb = Landing | { rejected: string; at: ts.Node };
type Parents = Map<ts.Node, Set<ts.Node>>;
type Rejected = { rejected: string; at: ts.Node };
type ValueInvocations = { calls: ts.Node[]; handoffs: ts.Node[]; parents: Parents } | Rejected;
/** A call made through a stored function value, with the flow that delivered the function so the climb follows only the callers that passed it. */
interface FlowInvocation { call: ts.Node; callee: ts.Node; parents: Parents }
/** What one climb has already expanded, so a recursive or shared function is entered once. */
interface Climbed { nodes: Set<ts.Node>; functions: Set<ts.Node> }
export class ComponentGraph implements FlowHost {
	readonly nodes = new Map<ts.Node, InventoryEntry>();
	private readonly nodeByEntry = new Map<InventoryEntry, ts.Node>();
	private nameIndex: Map<string, ts.Node[]> | undefined;
	private readonly tracing = new Set<ts.Node>();
	private readonly spellingCache = new Map<string, Map<ts.Node | undefined, ts.Node[]>>();
	private readonly invocationCache = new Map<ts.Node, ValueInvocations>();
	private aliasIndex: Map<ts.Node, string[]> | undefined;
	private importIndex: Map<ts.SourceFile, ts.Node[]> | undefined;
	private readonly useCache = new Map<string, ts.Node[]>();
	private readonly reachCache = new Map<ts.Node, Map<ts.Node, ts.Node[]>>();
	private readonly terminalReachCache = new Map<ts.Node, Set<ts.Node>>();
	private readonly resolving = new Set<string>();
	private readonly resolved = new Map<string, TerminalPartResult>();

	constructor(readonly source: SourceIndex, components: readonly InventoryEntry[]) {
		const byFile = new Map<string, InventoryEntry[]>();
		for (const entry of components) {
			if (entry.category !== "component" && entry.category !== "render-helper") continue;
			byFile.set(entry.source, [...byFile.get(entry.source) ?? [], entry]);
		}
		for (const [relative, entries] of byFile) {
			const starts = new Map(entries.map(entry => [Number(entry.details.start), entry]));
			source.walk(source.file(relative), node => {
				if (!ts.isClassDeclaration(node) && !ts.isClassExpression(node) && !ts.isObjectLiteralExpression(node)) return;
				const entry = starts.get(node.getStart());
				if (!entry) return;
				this.nodes.set(node, entry);
				this.nodeByEntry.set(entry, node);
			});
		}
	}

	named(name: string): readonly ts.Node[] { return this.names().get(name) ?? []; }

	/** A reference followed again while its own trace is open cannot be enumerated, so it is refused rather than answered from a partial trace. */
	valueInvocations(reference: ts.Node): ValueInvocations {
		const known = this.invocationCache.get(reference);
		if (known) return known;
		if (this.tracing.has(reference)) return { rejected: "function value is stored in a cycle of stored callbacks", at: reference };
		this.tracing.add(reference);
		const trace = traceValue(this, reference, "function");
		this.tracing.delete(reference);
		const result = "rejected" in trace ? { rejected: trace.rejected, at: trace.at } : { calls: trace.calls, handoffs: trace.handoffs, parents: trace.parents };
		this.invocationCache.set(reference, result);
		return result;
	}

	nodeOf(entry: InventoryEntry): ts.Node | undefined { return this.nodeByEntry.get(entry); }

	private names(): Map<string, ts.Node[]> {
		if (this.nameIndex) return this.nameIndex;
		const index = new Map<string, ts.Node[]>();
		for (const file of this.source.files) this.source.walk(file, node => {
			if (ts.isIdentifier(node) || ts.isPrivateIdentifier(node)) appendTo(index, node.text, node);
		});
		this.nameIndex = index;
		return index;
	}

	/** Each identifier spelled `name`, resolved once and grouped by the declaration the checker binds it to. */
	private resolvedBySpelling(name: string): Map<ts.Node | undefined, ts.Node[]> {
		let grouped = this.spellingCache.get(name);
		if (!grouped) {
			grouped = new Map();
			for (const identifier of this.names().get(name) ?? []) {
				const declaration = this.source.declaration(identifier);
				appendTo(grouped, declaration, identifier);
			}
			this.spellingCache.set(name, grouped);
		}
		return grouped;
	}

	/** Identifiers spelled `name` that the checker binds to `declaration`, optionally within one scope. */
	uses(declaration: ts.Node, name: string, scope?: ts.Node): ts.Node[] {
		const key = `${this.source.relative(declaration)}:${declaration.getStart()}:${name}:${scope ? scope.getStart() : ""}`;
		let found = this.useCache.get(key);
		if (!found) {
			const own = (declaration as ts.NamedDeclaration).name;
			found = [name, ...this.aliasNames().get(declaration) ?? []].flatMap(each => this.resolvedBySpelling(each).get(declaration) ?? [])
				.filter(identifier => identifier !== own && (!scope || identifier.getSourceFile() === scope.getSourceFile() && identifier.pos >= scope.pos && identifier.end <= scope.end));
			this.useCache.set(key, found);
		}
		return found;
	}

	/** The local names a declaration is imported or re-exported under, so a renamed import is still one of its uses. */
	private aliasNames(): Map<ts.Node, string[]> {
		if (this.aliasIndex) return this.aliasIndex;
		const index = new Map<ts.Node, string[]>();
		for (const file of this.source.files) this.source.walk(file, node => {
			if (!(ts.isImportSpecifier(node) || ts.isExportSpecifier(node) || ts.isImportClause(node) || ts.isImportEqualsDeclaration(node)) || !node.name) return;
			const target = this.source.declaration(node.name);
			if (target && (target as ts.NamedDeclaration).name?.getText() !== node.name.text) index.set(target, [...index.get(target) ?? [], node.name.text]);
		});
		this.aliasIndex = index;
		return index;
	}

	/**
	 * Climb from `start` to the component member that lexically holds it. A
	 * standalone function is entered through the calls whose arguments bound the
	 * traced value (`parents`), and through every caller only where the value
	 * did not arrive that way.
	 */
	climb(start: ts.Node, parents?: Parents, hops: ts.Node[] = [], seen: Climbed = { nodes: new Set(), functions: new Set() }): Climb[] {
		let child: ts.Node = start;
		for (let current: ts.Node | undefined = start.parent; current; child = current, current = current.parent) {
			if (this.nodes.has(current)) {
				const member = membersOf(current).includes(child) ? child : undefined;
				return member ? [{ owner: current, member, site: start, hops }] : [{ rejected: "value sits outside the members of its component", at: child }];
			}
			const standalone = this.standalone(current);
			if (!standalone) continue;
			const outcomes: Climb[] = [];
			const entries = parents ? entryArguments(this.source, start, current, parents) : { arguments: [], callers: true, captured: [] };
			for (const argument of entries.arguments) {
				if (seen.nodes.has(argument)) continue;
				seen.nodes.add(argument);
				const call = callOf(argument);
				outcomes.push(...this.climb(argument, parents, call ? [...hops, call] : hops, seen));
			}
			for (const { origin } of entries.captured) {
				if (seen.nodes.has(origin)) continue;
				seen.nodes.add(origin);
				outcomes.push(...this.climb(origin, parents, hops, seen));
			}
			if (!entries.callers && !entries.captured.length || seen.functions.has(current)) return outcomes;
			seen.functions.add(current);
			// A call made inside the function that closed over the value is the capture's own activation, which the climb from the capture already covers.
			const sites = this.invocations(standalone.declaration, standalone.name, standalone.uses)
				.filter(site => entries.callers || "rejected" in site || !entries.captured.some(({ scope }) => within(scope, "call" in site ? site.call : site)));
			for (const site of sites) {
				if ("rejected" in site) outcomes.push(site);
				else if ("callee" in site) outcomes.push(...this.climb(site.callee, site.parents, [...hops, site.call], seen));
				else outcomes.push(...this.climb(site, undefined, [...hops, site], seen));
			}
			return outcomes;
		}
		return [];
	}

	/** Where a standalone function runs: its call sites, subclass constructions for a constructor, and for a function used as a value every place that value is invoked. */
	private invocations(declaration: ts.Node, name: string, uses: ts.Node[]): Array<ts.Node | FlowInvocation | Rejected> {
		const exported = ts.canHaveModifiers(declaration) && !!ts.getModifiers(declaration)?.some(modifier => modifier.kind === ts.SyntaxKind.ExportKeyword);
		const loaded = exported ? this.dynamicImports().get(declaration.getSourceFile()) ?? [] : [];
		return [...loaded, ...uses.flatMap((identifier): Array<ts.Node | FlowInvocation | Rejected> => {
			const callee = readOf(identifier);
			const holder = identifier.parent;
			if ("name" in holder && holder.name === identifier && !ts.isPropertyAccessExpression(holder) && !ts.isQualifiedName(holder)) return [];
			if ((ts.isCallExpression(callee.parent) || ts.isNewExpression(callee.parent)) && callee.parent.expression === callee) return [callee.parent];
			const derived = ts.isExpressionWithTypeArguments(holder) && ts.isHeritageClause(holder.parent) && holder.parent.token === ts.SyntaxKind.ExtendsKeyword ? holder.parent.parent : undefined;
			if (derived && ts.isClassLike(derived) && derived.name) return this.invocations(derived, derived.name.text, this.uses(derived, derived.name.text));
			if (inertReference(holder)) return [];
			const through = this.valueInvocations(callee);
			return "rejected" in through ? [{ rejected: `${name} is referenced as a value and ${through.rejected}`, at: through.at }] : [...through.calls.map((call): ts.Node | FlowInvocation => {
				const target = ts.isCallExpression(call) || ts.isNewExpression(call) ? call.expression : undefined;
				return target && through.parents.has(target) ? { call, callee: target, parents: through.parents } : call;
			}), ...through.handoffs];
		})];
	}

	/** `import("module")` expressions by the module they load: an exported function nothing calls statically still runs wherever its module is loaded. */
	private dynamicImports(): Map<ts.SourceFile, ts.Node[]> {
		if (this.importIndex) return this.importIndex;
		const index = new Map<ts.SourceFile, ts.Node[]>();
		for (const file of this.source.files) this.source.walk(file, node => {
			if (!ts.isCallExpression(node) || node.expression.kind !== ts.SyntaxKind.ImportKeyword || !node.arguments[0]) return;
			const target = this.source.checker.getSymbolAtLocation(node.arguments[0])?.declarations?.find(ts.isSourceFile);
			if (target) index.set(target, [...index.get(target) ?? [], node]);
		});
		this.importIndex = index;
		return index;
	}

	/** A function or class that is not a component member: where its callers take over. */
	private standalone(node: ts.Node): { name: string; declaration: ts.Node; uses: ts.Node[] } | undefined {
		const parent = node.parent;
		const named = (declaration: ts.Node, name: string) => ({ name, declaration, uses: this.uses(declaration, name) });
		if (ts.isFunctionDeclaration(node) && node.name) return named(node, node.name.text);
		if ((ts.isArrowFunction(node) || ts.isFunctionExpression(node)) && ts.isVariableDeclaration(parent) && ts.isIdentifier(parent.name)) return named(parent, parent.name.text);
		if (ts.isMethodDeclaration(node) && ts.isClassLike(parent) && !this.nodes.has(parent)) {
			const name = ts.isIdentifier(node.name) || ts.isPrivateIdentifier(node.name) ? node.name.text : undefined;
			if (!name) return undefined;
			// A call through a base class or interface declaration reaches the override.
			const reaching = this.named(name).filter(identifier => {
				const declaration = this.source.declaration(identifier);
				if (!declaration) return ts.isPropertyAccessExpression(identifier.parent) && identifier.parent.name === identifier && ts.isCallExpression(identifier.parent.parent) && identifier.parent.parent.expression === identifier.parent;
				return declaration === node || !!declaration && (ts.isMethodDeclaration(declaration) || ts.isMethodSignature(declaration)) && (derives(this, declaration.parent, parent) || derives(this, parent, declaration.parent));
			});
			return { name, declaration: node, uses: reaching };
		}
		// An object literal's function member runs wherever that member is called, not wherever the object was built.
		const method = ts.isMethodDeclaration(node) && ts.isObjectLiteralExpression(parent) ? node : undefined;
		const assigned = (ts.isArrowFunction(node) || ts.isFunctionExpression(node)) && ts.isPropertyAssignment(parent) && ts.isObjectLiteralExpression(parent.parent) ? parent : undefined;
		const member = method ?? assigned;
		const label = member && (ts.isIdentifier(member.name) || ts.isPrivateIdentifier(member.name)) ? member.name.text : undefined;
		if (member && label && !this.nodes.has(member.parent)) {
			const reaching = literalMemberReach(this, member, label);
			return { name: label, declaration: member, uses: reaching };
		}
		if (ts.isConstructorDeclaration(node) && ts.isClassLike(parent) && !this.nodes.has(parent) && parent.name) return named(parent, parent.name.text);
		if (ts.isClassLike(node) && !this.nodes.has(node) && node.name) return named(node, node.name.text);
		return undefined;
	}

	/** Owner members a native hook can run, keyed to the hooks that reach them. */
	private nativeReach(owner: ts.Node): Map<ts.Node, ts.Node[]> {
		const cached = this.reachCache.get(owner);
		if (cached) return cached;
		const reach = new Map<ts.Node, ts.Node[]>();
		for (const root of membersOf(owner).filter(member => NATIVE_HOOK.test(memberName(this.source, member)))) {
			for (const member of this.reachableFrom(owner, [root])) reach.set(member, [...reach.get(member) ?? [], root]);
		}
		this.reachCache.set(owner, reach);
		return reach;
	}

	/** The part's own hooks, by name, and the members it reaches, for judging uses a native hook runs. */
	private inertHost(entry: InventoryEntry): InertUseHost {
		const declared = Array.isArray(entry.details.hooks) ? entry.details.hooks.filter(record).map(hook => hook.name) : [];
		return {
			hookNamed: name => NATIVE_HOOK.test(name) || TERMINAL_HOOK.test(name) || INPUT_HOOK.test(name) || declared.includes(name),
			membersOf,
			memberNamed: member => memberName(this.source, member),
			reach: (part, roots) => this.reachableFrom(part, roots),
		};
	}

	private terminalReach(owner: ts.Node): Set<ts.Node> {
		const cached = this.terminalReachCache.get(owner);
		if (cached) return cached;
		const reach = this.reachableFrom(owner, membersOf(owner).filter(member => { const name = memberName(this.source, member); return TERMINAL_HOOK.test(name) || INPUT_HOOK.test(name); }));
		this.terminalReachCache.set(owner, reach);
		return reach;
	}

	private reachableFrom(owner: ts.Node, roots: ts.Node[]): Set<ts.Node> {
		const local = new Set<ts.Node>(membersOf(owner));
		const reached = new Set<ts.Node>();
		const queue = [...roots];
		while (queue.length) {
			const member = queue.pop()!;
			if (reached.has(member)) continue;
			reached.add(member);
			this.source.walk(member, node => {
				if (!ts.isPropertyAccessExpression(node) && !ts.isIdentifier(node)) return;
				const declaration = this.source.declaration(ts.isPropertyAccessExpression(node) ? node.name : node);
				// A parameter property is a field the constructor's signature declares; naming it does not run the constructor body.
				if (declaration && ts.isParameter(declaration) && ts.isConstructorDeclaration(declaration.parent)) return;
				for (let current: ts.Node | undefined = declaration; current; current = current.parent) if (local.has(current)) { queue.push(current); break; }
			});
		}
		return reached;
	}

	private extendsClass(derived: ts.Node, base: ts.Node, depth = 0): boolean {
		if (!ts.isClassLike(derived) || depth > 6) return false;
		const parent = derived.heritageClauses?.find(clause => clause.token === ts.SyntaxKind.ExtendsKeyword)?.types[0];
		const declaration = parent && this.source.declaration(parent.expression);
		return !!declaration && (declaration === base || this.extendsClass(declaration, base, depth + 1));
	}

	/** A base-class member the construction owner never reaches because it overrides every hook leading there without `super`. */
	private dispatchExempt(constructionOwners: readonly ts.Node[], owner: ts.Node, member: ts.Node): boolean {
		const roots = this.nativeReach(owner).get(member) ?? [];
		return roots.length > 0 && constructionOwners.some(derived => derived !== owner && this.extendsClass(derived, owner)
			&& roots.every(root => membersOf(derived).some(own => memberName(this.source, own) === memberName(this.source, root) && !usesSuper(this.source, own))));
	}

	/**
	 * `held` also admits owners whose native hooks reach the part: the host then runs the part itself, which coverage must see on the browser leg.
	 * Only the part itself (`top`) must be held natively by some owner; a describe-less owner it sits in need only be placed by its own owners.
	 */
	resolve(entry: InventoryEntry, held = false, top = true): TerminalPartResult {
		const key = `${held ? (top ? "held:" : "held-parent:") : ""}${entry.id}`;
		const cached = this.resolved.get(key);
		if (cached) return cached;
		if (this.resolving.has(key)) return { reason: "components hold each other in a cycle" };
		this.resolving.add(key);
		const result = this.resolveUncached(entry, held, top);
		this.resolving.delete(key);
		this.resolved.set(key, result);
		return result;
	}

	/** A static member read never constructs or aliases an instance. */
	private staticMember(name: ts.Identifier | ts.PrivateIdentifier): boolean {
		const declaration = this.source.declaration(name);
		return !!declaration && ts.canHaveModifiers(declaration) && !!ts.getModifiers(declaration)?.some(modifier => modifier.kind === ts.SyntaxKind.StaticKeyword);
	}

	private constructionSites(node: ts.Node): ts.Node[] | AbsenceRejection {
		if (ts.isObjectLiteralExpression(node)) return [node];
		if (!ts.isClassLike(node) || !node.name) return { reason: "anonymous class has no named construction sites" };
		const sites: ts.Node[] = [];
		for (const identifier of this.uses(node, node.name.text)) {
			const parent = identifier.parent;
			if (ts.isNewExpression(parent) && parent.expression === identifier) sites.push(parent);
			else if (ts.isImportSpecifier(parent) || ts.isExportSpecifier(parent) || ts.isImportClause(parent) || ts.isTypeReferenceNode(parent)
				|| ts.isExpressionWithTypeArguments(parent) || ts.isTypeQueryNode(parent)
				|| ts.isBinaryExpression(parent) && parent.operatorToken.kind === ts.SyntaxKind.InstanceOfKeyword
				|| ts.isPropertyAccessExpression(parent) && parent.expression === identifier && this.staticMember(parent.name)) continue;
			else return { reason: `${node.name.text} is used other than by \`new\` (${parent.getText().slice(0, 60)})`, origin: this.source.origin(identifier) };
		}
		return sites.length ? sites : { reason: `${node.name.text} is never constructed`, origin: this.source.origin(node) };
	}

	private resolveUncached(entry: InventoryEntry, held: boolean, top: boolean): TerminalPartResult {
		const node = this.nodeOf(entry);
		if (!node) return { reason: "component node is not in the source index" };
		const sites = this.constructionSites(node);
		if (!Array.isArray(sites)) return sites;
		const landings: Landing[] = [], constructionLandings: Landing[] = [];
		const traces: Array<Exclude<ReturnType<typeof traceValue>, { rejected: string }>> = [];
		for (const site of sites) {
			const trace = traceValue(this, site);
			if ("rejected" in trace) return { reason: trace.rejected, origin: this.source.origin(trace.at) };
			traces.push(trace);
			for (const [index, value] of [site, ...trace.values].entries()) {
				for (const outcome of this.climb(value, trace.parents)) {
					if ("rejected" in outcome) return { reason: outcome.rejected, origin: this.source.origin(outcome.at) };
					landings.push(outcome);
					if (index === 0) constructionLandings.push(outcome);
				}
			}
		}
		const constructionOwners = constructionLandings.map(landing => landing.owner);
		const uses = new Map<ts.Node, PartUse>();
		const holders = new Map<ts.Node, Landing>();
		let heldByNative = false;
		const keyedReads = new Set(traces.flatMap(trace => [...trace.keyedReads]));
		for (const landing of landings) {
			const ownerEntry = this.nodes.get(landing.owner)!;
			if (ownerEntry === entry) continue;
			const nativeReachable = this.nativeReach(landing.owner).has(landing.member) && !this.dispatchExempt(constructionOwners, landing.owner, landing.member);
			const name = memberName(this.source, landing.member);
			const hookInert = nativeReachable && landing.hops.length === 0 ? hookInertUse(this.inertHost(entry), node, landing.site, keyedReads) : undefined;
			const use: PartUse = { ...this.source.origin(landing.member), owner: ownerEntry.id, member: name, memberKind: kindOf(name), nativeReachable, terminalReachable: this.terminalReach(landing.owner).has(landing.member), ...hookInert ? { hookInert } : {} };
			if (nativeReachable && !hookInert && held) { heldByNative = true; holders.set(landing.member, landing); }
			else if (nativeReachable && !hookInert) return { reason: `${ownerEntry.id.replace(/^[^:]*:/, "")} reaches the part from native hook member ${name}, holding it as \`${landing.site.getText().slice(0, 60)}\` at line ${this.source.origin(landing.site).line}`, origin: use };
			uses.set(landing.member, use);
		}
		if (!heldByNative && ![...uses.values()].some(use => use.terminalReachable)) return { reason: "no terminal hook of any component reaches the part", origin: this.source.origin(node) };
		const owners = new Map<string, PartOwner>();
		// A native hook that holds the part without constructing it is an owner too: its describe is where the host first runs the part.
		for (const landing of [...constructionLandings, ...held && top ? holders.values() : []]) {
			const ownerEntry = this.nodes.get(landing.owner)!;
			if (ownerEntry === entry) continue;
			const construction = { ...this.source.origin(landing.site), expression: landing.site.getText().slice(0, 80) };
			const hops = landing.hops.map(hop => ({ ...this.source.origin(hop), expression: hop.getText().slice(0, 80) }));
			const ownUses = [...uses.values()].filter(use => use.owner === ownerEntry.id);
			const nativeHooks = ownNativeHooks(ownerEntry);
			if (nativeHooks.length) {
				const previous = owners.get(ownerEntry.id);
				if (previous) previous.uses.push(...ownUses.filter(use => !previous.uses.includes(use))); else owners.set(ownerEntry.id, { id: ownerEntry.id, chain: [], construction, hops, uses: ownUses, nativeHooks });
				continue;
			}
			const parent = this.resolve(ownerEntry, held, false);
			if (!Array.isArray(parent) && held && top && holders.size) continue; // the native holder's describe plus the part's browser-leg render are the proof
			if (!Array.isArray(parent)) return { reason: `${ownerEntry.id.replace(/^[^:]*:/, "")} holds the part but ${parent.reason}`, origin: parent.origin };
			for (const outer of parent) {
				if (owners.has(outer.id)) continue;
				owners.set(outer.id, { ...outer, chain: [ownerEntry.id, ...outer.chain], construction, hops: [...hops, ...outer.hops], uses: [...ownUses, ...outer.uses] });
			}
		}
		if (!owners.size) return { reason: "no construction site lands in a component with native hooks", origin: this.source.origin(node) };
		if (held && top && ![...owners.values()].some(owner => owner.uses.some(use => use.nativeReachable && !use.hookInert))) return { reason: "no native hook holds the part", origin: this.source.origin(node) };
		return [...owners.values()];
	}
}
