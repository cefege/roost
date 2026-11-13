import ts from "typescript";
import type { SourceIndex } from "./inventory-source";

/**
 * Terminal owners for components that have a native hook but no terminal hook:
 * the component whose terminal render paints the state the native hook
 * describes. Called by `inventory-components.ts`; reads the shared checker only.
 */
export interface CarrierHost { id: string; node: ts.Node; terminal: Array<{ hook: string; bodies: ts.Node[] }> }
export interface CarrierUse { owner: string; hook: string; expression: string; source: string; line: number }

const MAX_DEPTH = 3;
const isFunctionBody = (node: ts.Node): node is ts.FunctionLikeDeclaration => ts.isMethodDeclaration(node) || ts.isFunctionDeclaration(node) || ts.isGetAccessor(node) || ts.isArrowFunction(node) || ts.isFunctionExpression(node);

/** State a native hook reads or its callers' callbacks write, as checker symbols declared in source classes or literals. */
function sharedSymbols(source: SourceIndex, roots: readonly ts.Node[]): Set<ts.Symbol> {
	const symbols = new Set<ts.Symbol>();
	const visited = new Set<ts.Node>();
	const visit = (node: ts.Node, depth: number): void => source.walk(node, inner => {
		if (!ts.isPropertyAccessExpression(inner)) return;
		const symbol = source.symbol(inner.name);
		if (symbol?.declarations?.some(declaration => !declaration.getSourceFile().isDeclarationFile)) symbols.add(symbol);
		const callee = source.declaration(inner.name);
		if (depth < MAX_DEPTH && callee && isFunctionBody(callee) && callee.body && !visited.has(callee)) { visited.add(callee); visit(callee.body, depth + 1); }
	});
	for (const root of roots) visit(root, 0);
	return symbols;
}

/** The first access in each host's terminal render bodies to state the host itself declares and the subject's native hooks share. */
export function sharedStateCarriers(source: SourceIndex, nativeBodies: readonly ts.Node[], constructionCallbacks: readonly ts.Node[], hosts: readonly CarrierHost[], subject: ts.Node): CarrierUse[] {
	const shared = sharedSymbols(source, [...nativeBodies, ...constructionCallbacks]);
	const uses: CarrierUse[] = [];
	for (const host of hosts) {
		if (host.node === subject) continue;
		let found: CarrierUse | undefined;
		for (const { hook, bodies } of host.terminal) for (const body of bodies) source.walk(body, node => {
			if (found || !ts.isPropertyAccessExpression(node)) return;
			const symbol = source.symbol(node.name);
			const declaredByHost = symbol?.declarations?.some(declaration => declaration.pos >= host.node.pos && declaration.end <= host.node.end && declaration.getSourceFile() === host.node.getSourceFile());
			if (symbol && declaredByHost && shared.has(symbol)) found = { owner: host.id, hook, expression: node.getText(), ...source.origin(node) };
		});
		if (found) uses.push(found);
	}
	return uses;
}

/** Callbacks passed where a class is constructed: what they write is state its host paints. */
export function constructionCallbacks(source: SourceIndex, classNode: ts.Node): ts.Node[] {
	const callbacks: ts.Node[] = [];
	for (const file of source.files) source.walk(file, node => {
		if (!ts.isNewExpression(node) || source.declaration(node.expression) !== classNode) return;
		for (const argument of node.arguments ?? []) if (ts.isArrowFunction(argument) || ts.isFunctionExpression(argument)) callbacks.push(argument.body);
	});
	return callbacks;
}

/** A literal merged onto a factory's product by `Object.assign` is that product: its render hook is the literal's terminal output. */
export function mergedProductCarriers(source: SourceIndex, literal: ts.ObjectLiteralExpression, hosts: readonly CarrierHost[]): CarrierUse[] {
	const call = literal.parent;
	if (!ts.isCallExpression(call) || call.expression.getText() !== "Object.assign" || call.arguments[0] === literal) return [];
	const target = call.arguments[0];
	const declaration = target && ts.isIdentifier(target) ? source.declaration(target) : undefined;
	const initializer = declaration && ts.isVariableDeclaration(declaration) ? declaration.initializer : undefined;
	const factory = initializer && ts.isCallExpression(initializer) ? source.declaration(initializer.expression) : undefined;
	if (!factory || !ts.isFunctionDeclaration(factory) || !factory.body) return [];
	const parameters = new Set<ts.Node>(factory.parameters);
	const uses: CarrierUse[] = [];
	for (const host of hosts) {
		if (host.node.getSourceFile() !== factory.getSourceFile() || host.node.pos < factory.body.pos || host.node.end > factory.body.end) continue;
		let found: CarrierUse | undefined;
		for (const { hook, bodies } of host.terminal) for (const body of bodies) source.walk(body, node => {
			if (found || !ts.isCallExpression(node) || !ts.isIdentifier(node.expression)) return;
			const callee = source.declaration(node.expression);
			if (callee && parameters.has(callee)) found = { owner: host.id, hook, expression: node.expression.getText(), ...source.origin(node) };
		});
		if (found) uses.push(found);
	}
	return uses;
}
