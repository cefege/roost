import ts from "typescript";
import { resolveHook } from "./inventory-hooks";
import { SourceIndex } from "./inventory-source";

const SELECTOR = "packages/tui/src/overlays/settings-selector.ts";
const DEFINITIONS = "packages/tui/src/overlays/settings-defs.ts";
export interface SourceSpan { source: string; line: number; start: number; end: number; expression: string }
export const sourceSpan = (source: SourceIndex, node: ts.Node): SourceSpan => ({ ...source.origin(node), start: node.getStart(), end: node.getEnd(), expression: node.getText() });

export function predicateProof(source: SourceIndex, declaration: ts.Node): Record<string, unknown> {
	const implementation = resolveHook(source, declaration);
	if (!(ts.isFunctionDeclaration(implementation) || ts.isFunctionExpression(implementation) || ts.isArrowFunction(implementation) || ts.isMethodDeclaration(implementation)) || !implementation.body) return { ...sourceSpan(source, implementation), unresolved: true, declaration: sourceSpan(source, declaration) };
	const returns: SourceSpan[] = [];
	const visit = (node: ts.Node): void => {
		if (node !== implementation && ts.isFunctionLike(node)) return;
		if (ts.isReturnStatement(node) && node.expression) returns.push(sourceSpan(source, node.expression));
		node.forEachChild(visit);
	};
	visit(implementation);
	return { ...sourceSpan(source, implementation), declaration: sourceSpan(source, declaration), body: sourceSpan(source, implementation.body), returns };
}

export function conditionRowGates(source: SourceIndex): Record<string, unknown>[] {
	const gates: Record<string, unknown>[] = [];
	source.walk(source.file(SELECTOR), node => {
		if (!ts.isCallExpression(node) || !ts.isPropertyAccessExpression(node.expression) || node.expression.name.text !== "condition") return;
		const declaration = source.declaration(node.expression.name);
		if (!declaration || source.relative(declaration) !== DEFINITIONS) return;
		const receiver = source.symbol(node.expression.expression);
		let method: ts.Node | undefined = node;
		while (method && !ts.isMethodDeclaration(method)) method = method.parent;
		if (!method) return;
		const identities: SourceSpan[] = [];
		source.walk(method, member => {
			if (ts.isPropertyAssignment(member) && source.name(member.name) === "id" && ts.isPropertyAccessExpression(member.initializer) && member.initializer.name.text === "path" && source.symbol(member.initializer.expression) === receiver) identities.push(sourceSpan(source, member.initializer));
		});
		let branch: ts.Node | undefined = node.parent;
		while (branch && !ts.isIfStatement(branch) && branch !== method) branch = branch.parent;
		gates.push({ ...sourceSpan(source, node), settingId: identities, branch: branch && ts.isIfStatement(branch) ? sourceSpan(source, branch) : undefined, owner: sourceSpan(source, method) });
	});
	return gates;
}

export interface NativeControlProjection {
	producer: SourceSpan;
	case: SourceSpan;
	returns: Array<{ kind: string; source: string; line: number; start: number; end: number; expression: string; conditions: Record<string, unknown>[] }>;
	converter?: ts.FunctionDeclaration;
	numericKind?: string;
}

/** Derive native control kinds from the real narrowed SettingDef switch and returned k values. */
export function nativeControlProjections(source: SourceIndex): Map<string, NativeControlProjection> {
	const methods: ts.MethodDeclaration[] = [];
	source.walk(source.file(SELECTOR), node => { if (ts.isMethodDeclaration(node) && node.name.getText() === "#prefsControl") methods.push(node); });
	if (methods.length !== 1) throw new Error(`Expected one canonical prefs control producer; found ${methods.length}`);
	const method = methods[0]!;
	const projections = new Map<string, NativeControlProjection>();
	source.walk(method, clause => {
		if (!ts.isCaseClause(clause) || !ts.isStringLiteralLike(clause.expression)) return;
		const returns: NativeControlProjection["returns"] = [];
		let converter: ts.FunctionDeclaration | undefined;
		let numericKind: string | undefined;
		const visit = (node: ts.Node): void => {
			if (node !== clause && ts.isFunctionLike(node)) return;
			if (ts.isReturnStatement(node) && node.expression && ts.isObjectLiteralExpression(node.expression)) {
				const kind = node.expression.properties.find(property => ts.isPropertyAssignment(property) && source.name(property.name) === "k");
				if (kind && ts.isPropertyAssignment(kind) && ts.isStringLiteralLike(kind.initializer)) {
					const kindName = kind.initializer.text;
					const conditions: Record<string, unknown>[] = [];
					for (let child: ts.Node = node; child.parent && child.parent !== clause; child = child.parent) if (ts.isIfStatement(child.parent)) {
						const condition = child.parent.expression;
						const dependency = ts.isIdentifier(condition) ? source.declaration(condition) : undefined;
						conditions.push({ ...sourceSpan(source, condition), branch: child.parent.thenStatement === child ? "then" : "else", dependency: dependency ? sourceSpan(source, dependency) : undefined });
						if (dependency && ts.isVariableDeclaration(dependency) && dependency.initializer && ts.isCallExpression(dependency.initializer)) {
							const helper = source.declaration(dependency.initializer.expression);
							if (helper && ts.isFunctionDeclaration(helper)) source.walk(helper, call => {
								if (!ts.isCallExpression(call)) return;
								const target = source.declaration(call.expression);
								if (!target || !ts.isFunctionDeclaration(target) || source.relative(target) !== DEFINITIONS || target.parameters.length !== 1) return;
								const signature = source.checker.getSignatureFromDeclaration(target);
								const returned = signature && source.checker.getReturnTypeOfSignature(signature);
								const types = returned ? returned.isUnion() ? returned.types : [returned] : [];
								if (types.some(type => !!(type.flags & ts.TypeFlags.NumberLike)) && types.every(type => !!(type.flags & (ts.TypeFlags.NumberLike | ts.TypeFlags.Undefined)))) { converter = target; numericKind = kindName; }
							});
						}
					}
					returns.push({ kind: kindName, ...sourceSpan(source, node.expression), conditions });
				}
			}
			node.forEachChild(visit);
		};
		visit(clause);
		if (returns.length === 0) throw new Error(`Unenumerated native prefs control ${clause.expression.text}`);
		projections.set(clause.expression.text, { producer: sourceSpan(source, method), case: sourceSpan(source, clause), returns, converter, numericKind });
	});
	return projections;
}

export function projectionDetails(source: SourceIndex, projection: NativeControlProjection): Record<string, unknown> {
	return { producer: projection.producer, case: projection.case, returns: projection.returns, converter: projection.converter ? sourceSpan(source, projection.converter) : undefined, numericKind: projection.numericKind };
}
