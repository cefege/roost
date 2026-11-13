import ts from "typescript";

/**
 * Decides whether one occurrence of a traced part, inside an owner member a
 * native hook runs, could execute any hook of the part. Only occurrences whose
 * continuation is traced elsewhere or whose effect is bounded qualify: storing
 * the value, reading a property of a container that cannot hold it, and calling
 * a member of the part that reaches none of its hooks. Anything else (passing
 * it on, iterating it, calling a hook) is not inert.
 */
export interface InertUseHost {
	hookNamed(name: string): boolean;
	membersOf(part: ts.Node): ts.Node[];
	memberNamed(member: ts.Node): string;
	reach(part: ts.Node, roots: ts.Node[]): Set<ts.Node>;
}

const COLLECTION_METHODS = new Set(["forEach", "map", "flatMap", "reduce", "reduceRight", "some", "every", "findIndex", "findLast", "findLastIndex", "entries", "keys", "values", "sort", "toSorted"]);
const stores = (parent: ts.Node, child: ts.Node): boolean => ts.isVariableDeclaration(parent) && parent.initializer === child
	|| ts.isPropertyDeclaration(parent) && parent.initializer === child
	|| ts.isPropertyAssignment(parent) && parent.initializer === child
	|| ts.isShorthandPropertyAssignment(parent)
	|| (ts.isParameter(parent) || ts.isBindingElement(parent)) && parent.initializer === child
	|| ts.isBinaryExpression(parent) && parent.right === child && parent.operatorToken.kind === ts.SyntaxKind.EqualsToken && (ts.isIdentifier(parent.left) || ts.isPropertyAccessExpression(parent.left));

/** A description of why the occurrence runs no hook of the part, or `undefined` when it might. */
export function hookInertUse(host: InertUseHost, part: ts.Node, site: ts.Node, keyedReads: ReadonlySet<ts.Node>): string | undefined {
	let child = site;
	while (ts.isParenthesizedExpression(child.parent) || ts.isNonNullExpression(child.parent) || ts.isAsExpression(child.parent)) child = child.parent;
	const parent = child.parent;
	if (stores(parent, child)) return "stored; each place it is read back is judged on its own";
	if (!ts.isPropertyAccessExpression(parent) || parent.expression !== child) return undefined;
	const name = parent.name.text;
	if (keyedReads.has(site)) return `reads .${name} of a container that holds the part under another property or selects it for tracing`;
	if (COLLECTION_METHODS.has(name) || host.hookNamed(name)) return undefined;
	const member = host.membersOf(part).find(candidate => host.memberNamed(candidate) === name);
	if (!member) return undefined;
	const reached = host.reach(part, [member]);
	return [...reached].some(each => host.hookNamed(host.memberNamed(each))) ? undefined : `calls ${name}, which reaches no hook of the part`;
}
