import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";
import { record, type Origin, type SourceIndex } from "./inventory-source";
import type { AbsenceRejection } from "./inventory-native-types";

/**
 * A native transport is the reconciler a terminal host builds once the TSP
 * peer answers its hello. Terminal rendering never reaches it, so only the
 * browser leg executes its hooks. The proof is structural: the class is not a
 * `Component`, its single construction sits behind a method that is handed a
 * negotiated `TspHello`, the host's field never escapes, and every hook call
 * goes through that field under the flag the construction method raises.
 */
export interface TransportSite extends Origin { expression: string }
export interface NativeTransportProof {
	class: "native-transport";
	component: Origin & { name: string; renderReturns: string; contract: Origin };
	construction: TransportSite & { holder: string; helloParameter: string; method: string };
	callers: TransportSite[];
	hookCalls: Array<TransportSite & { hook: string; guard: TransportSite }>;
}

const reject = (reason: string, node?: ts.Node, source?: SourceIndex): AbsenceRejection => ({ reason, ...node && source ? { origin: source.origin(node) } : {} });
const peel = (node: ts.Expression): ts.Expression => ts.isParenthesizedExpression(node) || ts.isNonNullExpression(node) ? peel(node.expression) : node;
const fieldNameOf = (node: ts.Node): string | undefined => ts.isPropertyAccessExpression(node) && node.expression.kind === ts.SyntaxKind.ThisKeyword ? node.name.text : undefined;
const site = (source: SourceIndex, node: ts.Node): TransportSite => ({ ...source.origin(node), expression: node.getText().replace(/\s+/g, " ").slice(0, 80) });

export function nativeTransportProof(source: SourceIndex, entry: InventoryEntry, node: ts.Node): NativeTransportProof | AbsenceRejection {
	if (!ts.isClassDeclaration(node) || !node.name) return reject("not a named class");
	const checker = source.checker;
	const symbol = checker.getSymbolAtLocation(node.name);
	const classification = record(entry.details.classification) ? entry.details.classification : undefined;
	const declared = record(classification?.componentContract) ? classification.componentContract.declaration : undefined;
	const contract = record(declared) && typeof declared.source === "string" && typeof declared.line === "number" ? { source: declared.source, line: declared.line } : undefined;
	if (!symbol || !contract) return reject("class symbol or Component contract location is missing", node, source);
	const contractNode = source.declarations(source.file(contract.source), "Component").find(ts.isInterfaceDeclaration);
	if (!contractNode) return reject("Component contract declaration is not in the source index", node, source);
	const instance = checker.getDeclaredTypeOfSymbol(symbol);
	if (checker.isTypeAssignableTo(instance, checker.getTypeAtLocation(contractNode))) return reject("class structurally satisfies Component", node, source);
	const render = node.members.find((member): member is ts.MethodDeclaration => ts.isMethodDeclaration(member) && member.name.getText() === "render");
	const signature = render && checker.getSignatureFromDeclaration(render);
	if (!render || !signature || !(checker.getReturnTypeOfSignature(signature).flags & ts.TypeFlags.Void)) return reject("render is missing or does not return void", node, source);

	const hooks = (Array.isArray(entry.details.hooks) ? entry.details.hooks : []).filter(record).map(hook => hook.name).filter((name): name is string => typeof name === "string");
	if (!hooks.length) return reject("entry declares no hooks", node, source);
	const constructions: ts.NewExpression[] = [];
	const hookReads: ts.PropertyAccessExpression[] = [];
	const problems: Array<{ reason: string; at: ts.Node }> = [];
	for (const file of source.files) source.walk(file, candidate => {
		if (ts.isIdentifier(candidate) && candidate !== node.name && source.symbol(candidate) === symbol) {
			const parent = candidate.parent;
			if (ts.isNewExpression(parent) && parent.expression === candidate) constructions.push(parent);
			else if (!ts.isTypeReferenceNode(parent) && !ts.isImportSpecifier(parent)) problems.push({ reason: "class is referenced other than by construction or as a type", at: candidate });
		}
		if (ts.isPropertyAccessExpression(candidate) && hooks.includes(candidate.name.text) && checker.getTypeAtLocation(candidate.expression).getSymbol() === symbol) hookReads.push(candidate);
	});
	const first = problems.at(0);
	if (first) return reject(first.reason, first.at, source);
	const [construction, ...extra] = constructions;
	if (!construction || extra.length) return reject(`expected exactly one construction site, found ${constructions.length}`, node, source);

	const assignment = construction.parent;
	const holderName = ts.isBinaryExpression(assignment) && assignment.operatorToken.kind === ts.SyntaxKind.EqualsToken && assignment.right === construction ? fieldNameOf(assignment.left) : undefined;
	const method = ts.findAncestor(construction, ts.isMethodDeclaration);
	const host = method && ts.isClassLike(method.parent) ? method.parent : undefined;
	if (!holderName || !method || !host) return reject("construction is not an assignment to a host field inside a host method", construction, source);
	const helloArgument = construction.arguments?.[1];
	const helloName = helloArgument && ts.isIdentifier(helloArgument) ? helloArgument.text : undefined;
	const hello = method.parameters.find(parameter => ts.isIdentifier(parameter.name) && parameter.name.text === helloName);
	if (!hello || hello.type?.getText() !== "TspHello") return reject("construction does not pass the method's negotiated TspHello parameter", construction, source);
	const holder = host.members.find((member): member is ts.PropertyDeclaration => ts.isPropertyDeclaration(member) && member.name.getText() === holderName);
	if (!holder) return reject("host declares no field for the transport", construction, source);
	const holderSymbol = checker.getSymbolAtLocation(holder.name);

	const raised = new Set<string>();
	source.walk(method, candidate => {
		if (ts.isBinaryExpression(candidate) && candidate.operatorToken.kind === ts.SyntaxKind.EqualsToken && candidate.right.kind === ts.SyntaxKind.TrueKeyword) { const name = fieldNameOf(candidate.left); if (name) raised.add(name); }
	});
	if (!raised.size) return reject("construction method raises no live flag", method, source);

	const callers: TransportSite[] = [];
	const methodSymbol = checker.getSymbolAtLocation(method.name);
	for (const file of source.files) source.walk(file, candidate => {
		if (!ts.isIdentifier(candidate) && !ts.isPrivateIdentifier(candidate) || candidate === method.name || checker.getSymbolAtLocation(candidate) !== methodSymbol) return;
		const call = candidate.parent.parent;
		if (ts.isCallExpression(call) && ts.isPropertyAccessExpression(candidate.parent) && fieldNameOf(candidate.parent) !== undefined) callers.push(site(source, call));
		else problems.push({ reason: "construction method is referenced other than by a call through this", at: candidate });
	});
	const unnegotiated = problems.at(0);
	if (unnegotiated) return reject(unnegotiated.reason, unnegotiated.at, source);
	if (!callers.length) return reject("construction method has no caller", method, source);

	const hookCalls: NativeTransportProof["hookCalls"] = [];
	for (const read of hookReads) {
		if (ts.findAncestor(read, ts.isClassDeclaration) === node) continue;
		const call = read.parent;
		const receiver = peel(read.expression);
		const guard = ts.isCallExpression(call) && call.expression === read && fieldNameOf(receiver) === holderName ? ts.findAncestor(read, (ancestor): ancestor is ts.IfStatement => ts.isIfStatement(ancestor)
			&& ancestor.thenStatement.pos <= read.pos && read.end <= ancestor.thenStatement.end && raisedFlagIn(source, ancestor.expression, raised)) : undefined;
		if (!guard) return reject("hook is called other than through the host field under the live flag", read, source);
		hookCalls.push({ ...site(source, call), hook: read.name.text, guard: site(source, guard.expression) });
	}
	for (const hook of hooks) if (!hookCalls.some(call => call.hook === hook)) return reject(`no host call site for hook ${hook}`, node, source);

	// Reading the field any other way could hand the transport to code the guard does not cover.
	let escape: ts.Node | undefined;
	for (const file of source.files) source.walk(file, candidate => {
		if (escape || !ts.isIdentifier(candidate) && !ts.isPrivateIdentifier(candidate) || candidate === holder.name || checker.getSymbolAtLocation(candidate) !== holderSymbol) return;
		const access = candidate.parent;
		let outer: ts.Node = access;
		while (ts.isNonNullExpression(outer.parent) || ts.isParenthesizedExpression(outer.parent)) outer = outer.parent;
		const user = outer.parent;
		const allowed = ts.isBinaryExpression(user) && user.left === outer || ts.isPropertyAccessExpression(user) && user.expression === outer || ts.isIfStatement(user) && user.expression === outer
			|| ts.isBinaryExpression(user) && [ts.SyntaxKind.EqualsEqualsEqualsToken, ts.SyntaxKind.ExclamationEqualsEqualsToken].includes(user.operatorToken.kind);
		if (!allowed) escape = candidate;
	});
	if (escape) return reject("host field is used other than as a receiver, assignment target or condition", escape, source);

	return {
		class: "native-transport",
		component: { ...source.origin(node), name: node.name.text, renderReturns: "void", contract },
		construction: { ...site(source, construction), holder: holderName, helloParameter: hello.name.getText(), method: method.name.getText() },
		callers,
		hookCalls,
	};
}

function raisedFlagIn(source: SourceIndex, condition: ts.Expression, raised: Set<string>): boolean {
	let found = false;
	source.walk(condition, candidate => { const name = fieldNameOf(candidate); if (name && raised.has(name)) found = true; });
	return found;
}
