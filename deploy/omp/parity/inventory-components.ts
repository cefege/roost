import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";
import { SourceIndex } from "./inventory-source";
import { kindGatedFunction, unsupportedKindBranch, type KindGate } from "./inventory-kind-gates";
import { returnsNativeDescriptor } from "./inventory-descriptor-types";
import { constructionCallbacks, mergedProductCarriers, sharedStateCarriers, type CarrierHost } from "./inventory-terminal-carriers";
import { componentContract, hookShape, nativeFallbackGuard, resolveHook } from "./inventory-hooks";

interface Candidate {
	id: string;
	name: string;
	node: ts.ClassLikeDeclaration | ts.ObjectLiteralExpression;
	shape: "class" | "object-factory";
	own: Map<string, ts.Node>;
	effective: Map<string, ts.Node>;
	bases: Array<{ expression: string; declaration?: string }>;
}
const terminalHook = (name: string): boolean => name === "render" || /^render[A-Z]/.test(name);
const nativeHook = (name: string): boolean => name === "describe" || /^describe[A-Z]/.test(name) || name === "nativeSheet";
/** A `describe` member of a non-Component whose return type holds no native descriptor is a data query that shares the name, not a native hook. */
function namesOnlyData(source: SourceIndex, container: ts.ClassLikeDeclaration | ts.ObjectLiteralExpression, member: ts.Node): boolean {
	if (returnsNativeDescriptor(source, member)) return false;
	const contract = componentContract(source, container);
	return !contract.declared && !contract.structural;
}
const inputHook = (name: string): boolean => /^(?:handleInput|handleNativeEvent|handleMouse|handleKey|onKey|onMouse)/.test(name);

function lexicalName(source: SourceIndex, node: ts.Node): string {
	const names: string[] = [];
	for (let current: ts.Node | undefined = node; current && !ts.isSourceFile(current); current = current.parent) {
		if ((ts.isClassDeclaration(current) || ts.isClassExpression(current) || ts.isFunctionDeclaration(current) || ts.isMethodDeclaration(current) || ts.isVariableDeclaration(current) || ts.isPropertyAssignment(current) || ts.isPropertyDeclaration(current)) && current.name) {
			const name = source.name(current.name);
			if (name && names[0] !== name) names.unshift(name);
		}
	}
	return names.join(".") || `anonymous@${source.origin(node).line}`;
}

/** Resolve inventory identities against the same source syntax before instrumenting it. */
export function runtimeComponentIds(file: ts.SourceFile, relative: string, entries: readonly InventoryEntry[]): Map<ts.Node, string> {
	const expected = new Map<number, string>();
	for (const entry of entries) if (entry.category === "component" && entry.source === relative) {
		const start = entry.details.start;
		if (typeof start !== "number") throw new Error(`Missing component syntax location: ${entry.id}`);
		expected.set(start, entry.id);
	}
	const result = new Map<ts.Node, string>();
	const visit = (node: ts.Node): void => {
		if (ts.isClassDeclaration(node) || ts.isClassExpression(node) || ts.isObjectLiteralExpression(node)) {
			const start = node.getStart(file);
			const id = expected.get(start);
			if (id) { result.set(node, id); expected.delete(start); }
		}
		node.forEachChild(visit);
	};
	visit(file);
	if (expected.size) throw new Error(`Inventory component syntax changed in ${relative}: ${[...expected.values()].join(", ")}`);
	return result;
}


/** Trace inherited hooks and native parent access before classifying a rows fallback. */
export function componentInventory(source: SourceIndex): InventoryEntry[] {
	const candidates: Candidate[] = [];
	const byNode = new Map<ts.Node, Candidate>();
	for (const file of source.files) {
		const relative = source.relative(file);
		if (!relative.startsWith("packages/tui/src/") && !relative.startsWith("packages/coding-agent/src/")) continue;
		source.walk(file, node => {
			if (!ts.isClassDeclaration(node) && !ts.isClassExpression(node) && !ts.isObjectLiteralExpression(node)) return;
			const own = new Map<string, ts.Node>();
			const members = ts.isObjectLiteralExpression(node) ? node.properties : node.members;
			for (const member of members) {
				if (!member.name) continue;
				const name = source.name(member.name);
				if (!name || !(terminalHook(name) || nativeHook(name) || inputHook(name))) continue;
				const shape = hookShape(source, member);
				if (shape.callable && !(nativeHook(name) && namesOnlyData(source, node, member))) own.set(name, member);
			}
			const isClass = !ts.isObjectLiteralExpression(node);
			const name = isClass && node.name ? node.name.text : lexicalName(source, node);
			const candidate: Candidate = { id: `component:${relative}#${name}`, name, node, shape: isClass ? "class" : "object-factory", own, effective: new Map(), bases: [] };
			candidates.push(candidate); byNode.set(node, candidate);
		});
	}
	const resolving = new Set<Candidate>();
	const resolved = new Set<Candidate>();
	const resolve = (candidate: Candidate): void => {
		if (resolved.has(candidate) || resolving.has(candidate)) return;
		resolving.add(candidate);
		if (!ts.isObjectLiteralExpression(candidate.node)) for (const heritage of candidate.node.heritageClauses ?? []) {
			if (heritage.token !== ts.SyntaxKind.ExtendsKeyword) continue;
			for (const base of heritage.types) {
				const declaration = source.declaration(base.expression);
				const resolvedDeclaration = declaration ? resolveHook(source, declaration) : undefined;
				const baseType = source.checker.getTypeAtLocation(base.expression);
				const constructor = baseType.getConstructSignatures()[0];
				const instance = constructor ? source.checker.getReturnTypeOfSignature(constructor) : baseType;
				const parent = resolvedDeclaration ? byNode.get(resolvedDeclaration) : undefined;
				const inherited = parent ?? instance.getSymbol()?.declarations?.map(node => byNode.get(node)).find(Boolean);
				candidate.bases.push({ expression: base.expression.getText(), declaration: inherited?.id });
				if (inherited) { resolve(inherited); for (const [name, node] of inherited.effective) candidate.effective.set(name, node); }
			}
		}
		for (const [name, node] of candidate.own) candidate.effective.set(name, node);
		const valueType = source.checker.getTypeAtLocation(candidate.node);
		const constructor = valueType.getConstructSignatures()[0];
		const instance = constructor ? source.checker.getReturnTypeOfSignature(constructor) : valueType;
		for (const property of source.checker.getPropertiesOfType(instance)) {
			const name = property.name;
			if (candidate.effective.has(name) || !(terminalHook(name) || nativeHook(name) || inputHook(name))) continue;
			const declaration = property.valueDeclaration ?? property.declarations?.[0];
			const shape = declaration ? hookShape(source, declaration) : undefined;
			if (declaration && shape?.callable && !(nativeHook(name) && namesOnlyData(source, candidate.node, declaration))) candidate.effective.set(name, declaration);
		}
		resolving.delete(candidate); resolved.add(candidate);
	};
	for (const candidate of candidates) resolve(candidate);
	const included = candidates.filter(candidate => candidate.effective.size > 0);
	const classifications = new Map(included.map(candidate => {
		const contract = componentContract(source, candidate.node);
		const terminal = [...candidate.effective].filter(([name]) => terminalHook(name));
		const returns = terminal.flatMap(([, method]) => hookShape(source, method).returns);
		const helper = !contract.declared && !contract.structural && terminal.length > 0 && returns.length > 0
			&& returns.every(returned => returned.shape === "string" || returned.shape === "value")
			&& !["describe", "describeSurface", "describeScreen"].some(name => candidate.effective.has(name));
		if (helper) candidate.id = candidate.id.replace(/^component:/, "render-helper:");
		return [candidate, { contract, helper, returns }] as const;
	}));
	const fallbackGuard = nativeFallbackGuard(source);
	const names = new Map<string, Candidate[]>();
	for (const candidate of included) { const previous = names.get(candidate.id) ?? []; previous.push(candidate); names.set(candidate.id, previous); }
	for (const duplicates of names.values()) if (duplicates.length > 1) for (const candidate of duplicates) candidate.id += `@${source.origin(candidate.node).line}:${candidate.node.getStart()}`;
	const enclosing = (node: ts.Node | undefined): Candidate | undefined => {
		for (let current = node; current; current = current.parent) { const candidate = byNode.get(current); if (candidate) return candidate; }
		return undefined;
	};
	interface Use { owner: string; hook: string; expression: string; source: string; line: number; kindGate?: KindGate }
	const nativeOwners = new Map<Candidate, Use[]>();
	const terminalOwners = new Map<Candidate, Use[]>();
	const dependencies = new Map<Candidate, Use[]>();
	// A terminal hook paints through the methods of its own component, so the uses inside those methods are the hook's.
	const terminalBodies = (owner: Candidate, implementation: ts.Node): ts.Node[] => {
		const bodies = [implementation];
		for (let index = 0; index < bodies.length && index < 24; index++) source.walk(bodies[index]!, node => {
			if (!ts.isCallExpression(node)) return;
			const callee = source.declaration(ts.isPropertyAccessExpression(node.expression) ? node.expression.name : node.expression);
			const own = callee && (ts.isMethodDeclaration(callee) || ts.isFunctionDeclaration(callee)) && callee.body && enclosing(callee) === owner;
			if (callee && own && !bodies.includes(callee)) bodies.push(callee);
		});
		return bodies;
	};
	for (const owner of included) for (const [hook, method] of owner.effective) {
		if (!nativeHook(hook) && !terminalHook(hook)) continue;
		const destination = nativeHook(hook) ? nativeOwners : terminalOwners;
		const implementation = resolveHook(source, method);
		const bodies = terminalHook(hook) ? terminalBodies(owner, implementation) : [implementation];
		for (const body of bodies) source.walk(body, node => {
			if (!ts.isCallExpression(node) && !ts.isPropertyAccessExpression(node)) return;
			const expression = ts.isCallExpression(node) ? node.expression : node;
			const symbolNode = ts.isPropertyAccessExpression(expression) ? expression.name : expression;
			const declaration = source.declaration(symbolNode);
			const target = enclosing(declaration);
			const use = { owner: owner.id, hook, expression: expression.getText(), ...source.origin(node) };
			if (target && target !== owner) {
				const uses = destination.get(target) ?? [];
				if (!uses.some(previous => previous.owner === use.owner && previous.hook === hook && previous.expression === use.expression)) uses.push(use);
				destination.set(target, uses);
			}
			if (body === implementation && ts.isCallExpression(node) && declaration && !target && (ts.isFunctionDeclaration(declaration) || ts.isVariableDeclaration(declaration))) {
				const uses = dependencies.get(owner) ?? [];
				const kindGate = nativeHook(hook) ? unsupportedKindBranch(source, node, implementation) : undefined;
				const dependency = { ...use, declaration: source.origin(declaration), ...(kindGate ? { kindGate } : {}) };
				const previous = uses.find(item => item.expression === use.expression && item.hook === hook);
				if (!previous) uses.push(dependency);
				else if (previous.kindGate?.kind !== kindGate?.kind) delete previous.kindGate;
				dependencies.set(owner, uses);
			}
			if (ts.isPropertyAccessExpression(expression)) {
				const type = source.checker.getTypeAtLocation(expression.expression);
				for (const declaration of type.getSymbol()?.declarations ?? []) {
					const receiver = byNode.get(declaration);
					if (!receiver || receiver === owner || receiver === target) continue;
					const uses = destination.get(receiver) ?? [];
					if (!uses.some(previous => previous.owner === use.owner && previous.hook === hook && previous.expression === use.expression)) uses.push(use);
					destination.set(receiver, uses);
				}
			}
		});
	}
	const hosts: CarrierHost[] = included.filter(candidate => [...candidate.effective.keys()].some(terminalHook)).map(candidate => ({
		id: candidate.id, node: candidate.node,
		terminal: [...candidate.effective].filter(([hook]) => terminalHook(hook)).map(([hook, method]) => ({ hook, bodies: terminalBodies(candidate, resolveHook(source, method)) })),
	}));
	for (const subject of included) {
		const native = [...subject.effective].filter(([hook]) => nativeHook(hook));
		if (!native.length || hosts.some(host => host.node === subject.node) || terminalOwners.get(subject)?.length) continue;
		const bodies = native.map(([, method]) => resolveHook(source, method));
		const callbacks = ts.isClassLike(subject.node) ? constructionCallbacks(source, subject.node) : [];
		const uses = [...sharedStateCarriers(source, bodies, callbacks, hosts, subject.node), ...ts.isObjectLiteralExpression(subject.node) ? mergedProductCarriers(source, subject.node, hosts) : []];
		if (uses.length) terminalOwners.set(subject, uses);
	}
	const memberReferences = referencedMemberNames(source);
	return included.map(candidate => {
		const terminal = [...candidate.effective].filter(([name]) => terminalHook(name));
		const native = [...candidate.effective].filter(([name]) => nativeHook(name));
		const interactive = [...candidate.effective.keys()].some(inputHook);
		const render = candidate.effective.get("render");
		const describe = candidate.effective.get("describe");
		const inheritedContainerDescription = !!fallbackGuard && !!describe && !!render
			&& resolveHook(source, describe) === resolveHook(source, fallbackGuard.describe)
			&& resolveHook(source, render) !== resolveHook(source, fallbackGuard.render);
		const nativeParents = nativeOwners.get(candidate) ?? [];
		const terminalParents = terminalOwners.get(candidate) ?? [];
		const nullReturns: Array<{ hook: string; source: string; line: number; kindGate?: KindGate }> = [];
		let concreteReturns = 0;
		for (const [hook, method] of native) {
			const implementation = resolveHook(source, method);
			if (ts.isArrowFunction(implementation) && !ts.isBlock(implementation.body)) {
				if (implementation.body.kind === ts.SyntaxKind.NullKeyword || ts.isIdentifier(implementation.body) && implementation.body.text === "undefined") nullReturns.push({ hook, ...source.origin(implementation.body) });
				else concreteReturns++;
			}
			const returns = (node: ts.Node): void => {
				if (node !== implementation && (ts.isFunctionExpression(node) || ts.isArrowFunction(node) || ts.isFunctionDeclaration(node) || ts.isMethodDeclaration(node))) return;
				if (ts.isReturnStatement(node)) {
					if (!node.expression || node.expression.kind === ts.SyntaxKind.NullKeyword || ts.isIdentifier(node.expression) && node.expression.text === "undefined") { const kindGate = unsupportedKindBranch(source, node, implementation); nullReturns.push({ hook, ...source.origin(node), ...(kindGate ? { kindGate } : {}) }); }
					else concreteReturns++;
				}
				node.forEachChild(returns);
			};
			returns(implementation);
		}
		const classification = classifications.get(candidate)!;
		const declaresComponent = classification.contract.declared;
		const nativeEntrypoints = native.filter(([name]) => ["describe", "describeSurface", "describeScreen"].includes(name));
		const nativeStatus = inheritedContainerDescription ? "rows-fallback" : native.length === 0 ? nativeParents.length > 0 ? "parent-owned" : "native-hook-absent" : concreteReturns === 0 && nullReturns.length > 0 ? "null-description" : nativeEntrypoints.length === 0 ? nativeParents.length > 0 ? "parent-owned" : "native-helper-hooks" : nullReturns.length > 0 ? "conditional-native" : "native-hooks";
		const obligations: string[] = [];
		if (inheritedContainerDescription || nativeStatus === "null-description") obligations.push("native-rows-fallback");
		if (interactive && terminal.length > 0 && native.length === 0 && nativeParents.length === 0) obligations.push("interactive-native-missing");
		if (nullReturns.length > 0) obligations.push("conditional-native-paths");
		if (!classification.helper && !declaresComponent && !classification.contract.structural && !interactive && native.length === 0 && terminal.length > 0) obligations.push("terminal-helper-native-owner-review");
		if (candidate.bases.some(base => !base.declaration)) obligations.push("unresolved-inherited-hooks");
		const hooks = [...candidate.effective].map(([name, node]) => {
			const implementation = resolveHook(source, node);
			const delegatedCalls: Array<{ expression: string; source: string; line: number }> = [];
			const kindGate = nativeHook(name) ? kindGatedFunction(source, implementation) : undefined;
			source.walk(implementation, child => { if (ts.isCallExpression(child)) delegatedCalls.push({ expression: child.expression.getText(), ...source.origin(child) }); });
			return { name, inherited: !candidate.own.has(name), referenced: memberReferences.has(name), declaration: { ...source.origin(node), start: node.getStart() }, implementation: { ...source.origin(implementation), start: implementation.getStart(), end: implementation.getEnd(), text: implementation.getText() }, returns: hookShape(source, node).returns, delegatedCalls, ...(kindGate ? { kindGate } : {}) };
		});
		return source.entry(candidate.id, classification.helper ? "render-helper" : "component", candidate.node, {
			name: candidate.name, start: candidate.node.getStart(), shape: candidate.shape, abstract: ts.isClassLike(candidate.node) && !!(ts.getCombinedModifierFlags(candidate.node) & ts.ModifierFlags.Abstract), declaresComponent,
			exposure: classification.helper ? "non-component-helper" : "ui",
			classification: { componentContract: classification.contract, terminalReturns: classification.returns, declaration: { ...source.origin(candidate.node), start: candidate.node.getStart() } },
			bases: candidate.bases, interactive, terminalHooks: terminal.map(([name]) => name), nativeHooks: native.map(([name]) => name),
			nativeEntrypoints: nativeEntrypoints.map(([name]) => name), hooks, nativeStatus,
			nativeFallback: inheritedContainerDescription && fallbackGuard ? { ...source.origin(fallbackGuard.describe), guard: { source: fallbackGuard.source, line: fallbackGuard.line, expression: fallbackGuard.expression } } : undefined,
			nativeOwners: nativeParents, terminalOwners: terminalParents, delegatedHelpers: dependencies.get(candidate) ?? [], nullReturns,
			obligations: classification.helper ? [] : obligations,
		});
	});
}

/**
 * Every member name read anywhere in the source (`x.name`, `x["name"]`,
 * `{ name } = x`). A hook whose name never appears here has no caller: the
 * framework reaches hooks only through such reads.
 */
function referencedMemberNames(source: SourceIndex): Set<string> {
	const names = new Set<string>();
	for (const file of source.files) source.walk(file, node => {
		if (ts.isPropertyAccessExpression(node)) names.add(node.name.text);
		else if (ts.isElementAccessExpression(node) && ts.isStringLiteralLike(node.argumentExpression)) names.add(node.argumentExpression.text);
		else if (ts.isBindingElement(node) && ts.isObjectBindingPattern(node.parent)) {
			const name = node.propertyName ?? node.name;
			if (ts.isIdentifier(name) || ts.isStringLiteralLike(name)) names.add(name.text);
		}
	});
	return names;
}

/**
 * Whether `receiver` holds the TSP-negotiated feature set: it is assigned from
 * a `.features` read of a wire-declared type (`TspHello.features`). Catalog
 * model capability lists also call `features.includes(...)`; they are not TSP.
 */
function negotiatedFeatureSet(source: SourceIndex, receiver: ts.Expression): boolean {
	const nameNode = ts.isPropertyAccessExpression(receiver) ? receiver.name : receiver;
	const symbol = source.symbol(nameNode);
	const declaration = symbol?.valueDeclaration;
	if (!symbol || !declaration) return false;
	const values: ts.Expression[] = [];
	if ((ts.isPropertyDeclaration(declaration) || ts.isVariableDeclaration(declaration)) && declaration.initializer) values.push(declaration.initializer);
	source.walk(declaration.getSourceFile(), node => {
		if (!ts.isBinaryExpression(node) || node.operatorToken.kind !== ts.SyntaxKind.EqualsToken) return;
		const left = ts.isPropertyAccessExpression(node.left) ? node.left.name : node.left;
		if (source.symbol(left) === symbol) values.push(node.right);
	});
	return values.some(value => {
		let wire = false;
		source.walk(value, node => {
			if (wire || !ts.isPropertyAccessExpression(node) || node.name.text !== "features") return;
			const owner = source.checker.getTypeAtLocation(node.expression).getSymbol();
			wire = owner?.declarations?.some(item => source.relative(item).startsWith("packages/wire/")) ?? false;
		});
		return wire;
	});
}

/** Features are negotiated strings, not an enum. Keep each literal check and dynamic boundary. */
export function featureInventory(source: SourceIndex): InventoryEntry[] {
	const entries = new Map<string, InventoryEntry>();
	for (const file of source.files) source.walk(file, node => {
		if (!ts.isCallExpression(node) || !ts.isPropertyAccessExpression(node.expression)) return;
		const method = node.expression.name.text;
		const receiver = node.expression.expression;
		const receiverName = ts.isIdentifier(receiver) || ts.isPrivateIdentifier(receiver) ? receiver.text : ts.isPropertyAccessExpression(receiver) ? receiver.name.text : "";
		if (method !== "feature" && !(["has", "includes"].includes(method) && /^#?features?$/i.test(receiverName) && negotiatedFeatureSet(source, receiver))) return;
		const argument = node.arguments[0];
		if (!argument) return;
		const value = source.value(argument);
		const check = { ...source.origin(node), expression: node.getText() };
		if (typeof value !== "string") {
			const id = `feature-check:${check.source}#${check.line}:${node.getStart()}`;
			entries.set(id, source.entry(id, "feature-check", node, { openVocabulary: true, argument: source.serialize(value), obligations: ["runtime-feature-name"] }));
			return;
		}
		const id = `feature:${value}`;
		const previous = entries.get(id);
		if (previous) (previous.details.checks as unknown[]).push(check);
		else entries.set(id, source.entry(id, "feature", node, { name: value, openVocabulary: true, checks: [check] }));
	});
	return [...entries.values()];
}
