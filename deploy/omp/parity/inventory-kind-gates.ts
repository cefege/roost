import path from "node:path";
import { readFileSync } from "node:fs";
import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";
import { record, type SourceIndex } from "./inventory-source";
import { ROOST_ROOT, RUST_HOST, rustFiles } from "./inventory-host-audit";

/**
 * Source reachable only when the host lacks a `supports(kind)` capability: the
 * branch tests, the functions reached only from such branches, and the
 * browser's own advertisement of kinds, which proves no browser case enters them.
 * Read by `inventory-components.ts` and `coverage-provenance.ts`.
 */
export interface KindGate { kind: string; source: string; line: number; expression: string }

const MAX_CALLER_DEPTH = 4;

/** The `supports(kind)` test that a branch condition reduces to, and whether it is negated. */
function supportsTest(source: SourceIndex, test: ts.Expression, negated = false): { kind: string; negated: boolean } | undefined {
	if (ts.isParenthesizedExpression(test)) return supportsTest(source, test.expression, negated);
	if (ts.isPrefixUnaryExpression(test) && test.operator === ts.SyntaxKind.ExclamationToken) return supportsTest(source, test.operand, !negated);
	if (ts.isBinaryExpression(test) && test.operatorToken.kind === ts.SyntaxKind.EqualsEqualsEqualsToken && test.right.kind === ts.SyntaxKind.TrueKeyword) return supportsTest(source, test.left, negated);
	if (ts.isIdentifier(test)) {
		const declaration = source.declaration(test);
		if (declaration && ts.isParameter(declaration)) return parameterTest(source, declaration, negated);
		const constant = declaration && ts.isVariableDeclaration(declaration) && declaration.initializer && ts.isVariableDeclarationList(declaration.parent) && (declaration.parent.flags & ts.NodeFlags.Const) !== 0;
		return constant ? supportsTest(source, declaration.initializer!, negated) : undefined;
	}
	if (!ts.isCallExpression(test)) return undefined;
	const callee = ts.isPropertyAccessExpression(test.expression) ? test.expression.name : ts.isIdentifier(test.expression) ? test.expression : undefined;
	if (!callee) return undefined;
	if (callee.text !== "supports") {
		// A predicate method whose whole body is `return <supports test>`.
		const declaration = source.declaration(callee);
		const body = declaration && (ts.isMethodDeclaration(declaration) || ts.isFunctionDeclaration(declaration)) ? declaration.body : undefined;
		const only = body && body.statements.length === 1 ? body.statements[0] : undefined;
		return only && ts.isReturnStatement(only) && only.expression ? supportsTest(source, only.expression, negated) : undefined;
	}
	const argument = test.arguments[0];
	return argument && ts.isStringLiteralLike(argument) ? { kind: argument.text, negated } : undefined;
}

/**
 * The kind whose absence alone reaches `node` inside `hook`: the node sits in
 * the branch a `supports(kind)` test rejects. A host that always advertises the
 * kind can never reach it.
 */
export function unsupportedKindBranch(source: SourceIndex, node: ts.Node, hook: ts.Node): KindGate | undefined {
	for (let child: ts.Node = node, parent = node.parent; parent && child !== hook; child = parent, parent = parent.parent) {
		const branch = ts.isConditionalExpression(parent) ? { test: parent.condition, accepted: parent.whenTrue, rejected: parent.whenFalse }
			: ts.isIfStatement(parent) ? { test: parent.expression, accepted: parent.thenStatement, rejected: parent.elseStatement } : undefined;
		if (!branch || child === branch.test) continue;
		const gate = supportsTest(source, branch.test);
		if (gate && child === (gate.negated ? branch.accepted : branch.rejected)) return { kind: gate.kind, ...source.origin(branch.test), expression: branch.test.getText() };
	}
	return undefined;
}

/** A parameter every call of its function fills with the same `supports(kind)` test: the parameter is that test. */
function parameterTest(source: SourceIndex, parameter: ts.ParameterDeclaration, negated: boolean): { kind: string; negated: boolean } | undefined {
	const fn = parameter.parent;
	if (!isNamedFunction(fn) || parameter.initializer || parameter.questionToken || parameter.dotDotDotToken || overridesInherited(source, fn)) return undefined;
	const index = fn.parameters.indexOf(parameter);
	let found: { kind: string; negated: boolean } | undefined;
	for (const reference of referencesNamed(source, fn.name!.getText())) {
		if (reference === fn.name || !source.symbol(reference)?.declarations?.includes(fn)) continue;
		const callee = reference.parent && ts.isPropertyAccessExpression(reference.parent) && reference.parent.name === reference ? reference.parent : reference;
		const call = callee.parent;
		if (!ts.isCallExpression(call) || call.expression !== callee || call.arguments.some(argument => ts.isSpreadElement(argument))) return undefined;
		const argument = call.arguments[index];
		const test = argument ? supportsTest(source, argument) : undefined;
		if (!test || found && (found.kind !== test.kind || found.negated !== test.negated)) return undefined;
		found = test;
	}
	return found && { kind: found.kind, negated: found.negated !== negated };
}

type NamedFunction = ts.MethodDeclaration | ts.FunctionDeclaration;
const isNamedFunction = (node: ts.Node): node is NamedFunction => (ts.isMethodDeclaration(node) || ts.isFunctionDeclaration(node)) && !!node.name && !!node.body;

/** Every identifier and string-keyed element access by name, so a function's references are found without a symbol search per file. */
const referenceIndexes = new WeakMap<SourceIndex, Map<string, ts.Node[]>>();
function referencesNamed(source: SourceIndex, name: string): ts.Node[] {
	let index = referenceIndexes.get(source);
	if (!index) {
		index = new Map();
		for (const file of source.files) source.walk(file, node => {
			const key = ts.isIdentifier(node) || ts.isPrivateIdentifier(node) ? node.text : ts.isStringLiteralLike(node) && ts.isElementAccessExpression(node.parent) ? node.text : undefined;
			if (key !== undefined) index!.set(key, [...index!.get(key) ?? [], node]);
		});
		referenceIndexes.set(source, index);
	}
	return index.get(name) ?? [];
}

/** Whether a base type of the function's class declares a member of the same name, so a call through the base would resolve elsewhere. */
function overridesInherited(source: SourceIndex, fn: NamedFunction): boolean {
	if (!ts.isClassLike(fn.parent)) return false;
	const type = source.checker.getTypeAtLocation(fn.parent);
	return source.checker.getBaseTypes(type as ts.InterfaceType).some(base => base.getProperty(fn.name!.getText()) !== undefined)
		|| (fn.parent.heritageClauses ?? []).some(clause => clause.types.some(item => source.checker.getTypeAtLocation(item).getProperty(fn.name!.getText()) !== undefined));
}

/**
 * The kind whose absence alone reaches every call of `fn`: all of its
 * references are calls, each sits in an unsupported-kind branch or in a
 * function that is itself reached only that way.
 */
export function kindGatedFunction(source: SourceIndex, fn: ts.Node, depth = 0, visiting = new Set<ts.Node>()): KindGate | undefined {
	if (!isNamedFunction(fn) || depth > MAX_CALLER_DEPTH || visiting.has(fn) || overridesInherited(source, fn)) return undefined;
	visiting.add(fn);
	let gate: KindGate | undefined;
	let calls = 0;
	for (const reference of referencesNamed(source, fn.name!.getText())) {
		if (reference === fn.name) continue;
		const symbolNode = reference.parent && ts.isPropertyAccessExpression(reference.parent) && reference.parent.name === reference ? reference.parent : reference;
		if (!source.symbol(reference)?.declarations?.includes(fn)) continue;
		const call = symbolNode.parent;
		if (!ts.isCallExpression(call) || call.expression !== symbolNode) return undefined;
		const reached = gatedByUnsupportedKind(source, call, depth + 1, visiting);
		if (!reached || gate && gate.kind !== reached.kind) return undefined;
		gate ??= reached;
		calls++;
	}
	visiting.delete(fn);
	return calls ? gate : undefined;
}

/** The unsupported-kind gate that alone reaches `node`: a branch around it, or around every call of the function holding it. */
export function gatedByUnsupportedKind(source: SourceIndex, node: ts.Node, depth = 0, visiting = new Set<ts.Node>()): KindGate | undefined {
	const holder = ts.findAncestor(node, isNamedFunction);
	if (!holder) return undefined;
	return unsupportedKindBranch(source, node, holder) ?? kindGatedFunction(source, holder, depth, visiting);
}

/** The kinds the browser's single `open` action always advertises, with that action's source line. */
export interface HostKinds { kinds: Set<string>; source: string; line: number }

export function hostAdvertisedKinds(root = ROOST_ROOT): HostKinds {
	const opens = RUST_HOST.flatMap(directory => rustFiles(path.join(root, directory))).flatMap(file => {
		const text = readFileSync(file, "utf8");
		return [...text.matchAll(/"method":"open","params":\{[^}]*"kinds":\[([^\]]*)\]/g)].map(match => ({
			kinds: new Set([...match[1]!.matchAll(/"([^"]+)"/g)].map(kind => kind[1]!)),
			source: path.relative(root, file).split(path.sep).join("/"),
			line: text.slice(0, match.index).split("\n").length,
		}));
	});
	if (opens.length !== 1) throw new Error(`Expected one browser open action advertising kinds; found ${opens.length}`);
	return opens[0]!;
}

/**
 * Attach the browser's advertisement to every kind gate on a component: a
 * delegated helper, a whole hook, or an empty return reachable only without
 * a kind. When Roost always advertises that kind no browser case can reach it,
 * and the coverage gate audits it from source instead.
 */
export function classifyHostKindFallbacks(entries: InventoryEntry[], host: HostKinds = hostAdvertisedKinds()): void {
	for (const entry of entries) for (const key of ["delegatedHelpers", "hooks", "nullReturns"]) {
		const uses = entry.details[key];
		if (!Array.isArray(uses)) continue;
		for (const use of uses) {
			if (!record(use) || !record(use.kindGate) || typeof use.kindGate.kind !== "string") continue;
			use.kindGate.hostAdvertised = host.kinds.has(use.kindGate.kind);
			use.kindGate.host = { source: host.source, line: host.line };
		}
	}
}
