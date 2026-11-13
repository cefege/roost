import ts from "typescript";
import type { SourceIndex } from "./inventory-source";

/**
 * Which source functions a call, or a function-typed variable, parameter or
 * field, can reach. Answers over-approximate (every override, every assigned
 * value, every caller's argument); `undefined` means a reachable value could
 * not be enumerated, which the caller must treat as an escape.
 */
export interface FlowHost {
	source: SourceIndex;
	uses(declaration: ts.Node, name: string, scope?: ts.Node): ts.Node[];
	/** Every identifier in the program spelled `name`. */
	named(name: string): readonly ts.Node[];
	/** Where a function referenced as a value runs, or why that cannot be followed. */
	valueInvocations(reference: ts.Node): { calls: ts.Node[]; handoffs: ts.Node[]; parents: Map<ts.Node, Set<ts.Node>> } | { rejected: string; at: ts.Node };
}
export type Functions = ts.SignatureDeclaration[] | undefined;
type Call = ts.CallExpression | ts.NewExpression;

const ASSIGNMENTS = [ts.SyntaxKind.EqualsToken, ts.SyntaxKind.BarBarEqualsToken, ts.SyntaxKind.QuestionQuestionEqualsToken];
const SELECTORS = [ts.SyntaxKind.BarBarToken, ts.SyntaxKind.QuestionQuestionToken, ts.SyntaxKind.AmpersandAmpersandToken];

export const functionOf = (node: ts.Node | undefined): ts.SignatureDeclaration | undefined => {
	for (let current = node; current; current = current.parent) if (ts.isFunctionLike(current)) return current;
	return undefined;
};

/** A reference that neither runs nor stores the value: imports, exports and type positions. */
export const inertReference = (holder: ts.Node): boolean => ts.isTypeParameterDeclaration(holder) || ts.isImportSpecifier(holder) || ts.isExportSpecifier(holder) || ts.isImportClause(holder) || ts.isExportAssignment(holder) || ts.isNamespaceImport(holder) || ts.isImportEqualsDeclaration(holder)
	|| ts.isImportTypeNode(holder) || ts.isTypeReferenceNode(holder) || ts.isTypeQueryNode(holder) || ts.isTypePredicateNode(holder) || ts.isQualifiedName(holder) || ts.isExpressionWithTypeArguments(holder);

/** An identifier as the expression that reads it: `object.member` reads through its property access. */
export const readOf = (identifier: ts.Node): ts.Node => ts.isPropertyAccessExpression(identifier.parent) && identifier.parent.name === identifier ? identifier.parent : identifier;

export const derives = (host: FlowHost, candidate: ts.Node, base: ts.Node, depth = 0): boolean => {
	if (candidate === base) return true;
	if ((!ts.isClassLike(candidate) && !ts.isInterfaceDeclaration(candidate)) || depth > 8) return false;
	return (candidate.heritageClauses ?? []).some(clause => clause.types.some(type => {
		const parent = host.source.declaration(type.expression);
		return !!parent && derives(host, parent, base, depth + 1);
	}));
};
const related = (host: FlowHost, left: ts.Node, right: ts.Node): boolean => derives(host, left, right) || derives(host, right, left);

const union = (...parts: Functions[]): Functions => parts.some(part => !part) ? undefined : parts.flatMap(part => part ?? []);

/** The call sites of a function-like, or `undefined` when something other than a call refers to it. */
function callSites(host: FlowHost, owner: ts.SignatureDeclaration): Call[] | undefined {
	const sites: Call[] = [];
	const collect = (identifier: ts.Node): boolean => {
		const callee = readOf(identifier);
		if ((ts.isCallExpression(callee.parent) || ts.isNewExpression(callee.parent)) && callee.parent.expression === callee) sites.push(callee.parent);
		else if (!inertReference(identifier.parent)) {
			// Calls through a stored reference bind their arguments the same way; a library that runs the callback itself supplies unknown arguments.
			const through = host.valueInvocations(callee);
			if ("rejected" in through || through.handoffs.length) return false;
			sites.push(...through.calls.filter(call => ts.isCallExpression(call) || ts.isNewExpression(call)));
		}
		return true;
	};
	const viaName = (declaration: ts.Node, name: string): Call[] | undefined => host.uses(declaration, name).every(collect) ? sites : undefined;
	if (ts.isFunctionDeclaration(owner) && owner.name) return viaName(owner, owner.name.text);
	if ((ts.isArrowFunction(owner) || ts.isFunctionExpression(owner)) && ts.isVariableDeclaration(owner.parent) && ts.isIdentifier(owner.parent.name)) return viaName(owner.parent, owner.parent.name.text);
	if (ts.isMethodDeclaration(owner) && (ts.isIdentifier(owner.name) || ts.isPrivateIdentifier(owner.name)) && ts.isClassLike(owner.parent)) {
		for (const identifier of host.named(owner.name.text)) {
			if ((ts.isMethodDeclaration(identifier.parent) || ts.isMethodSignature(identifier.parent)) && identifier.parent.name === identifier) continue;
			const declaration = host.source.declaration(identifier);
			// A call through an untyped receiver names the method but resolves to nothing; it may reach any implementation.
			if (!declaration) { if (ts.isPropertyAccessExpression(identifier.parent) && identifier.parent.name === identifier && ts.isCallExpression(identifier.parent.parent) && identifier.parent.parent.expression === identifier.parent) sites.push(identifier.parent.parent); continue; }
			const relevant = declaration === owner || !!declaration && (ts.isMethodDeclaration(declaration) || ts.isMethodSignature(declaration)) && related(host, declaration.parent, owner.parent);
			if (relevant && !collect(identifier)) return undefined;
		}
		return sites;
	}
	// A function written as an object-literal member runs wherever the interface property it implements is called.
	const member = ts.isMethodDeclaration(owner) && ts.isObjectLiteralExpression(owner.parent) ? owner : (ts.isArrowFunction(owner) || ts.isFunctionExpression(owner)) && ts.isPropertyAssignment(owner.parent) ? owner.parent : undefined;
	if (member && ts.isObjectLiteralExpression(member.parent) && (ts.isIdentifier(member.name) || ts.isPrivateIdentifier(member.name))) {
		const contextual = host.source.checker.getContextualType(member.parent);
		const targets = (contextual?.isUnion() ? contextual.types : contextual ? [contextual] : []).flatMap(type => type.getProperty(member.name.getText())?.declarations ?? []);
		if (!targets.length) return undefined;
		for (const identifier of host.named(member.name.text)) {
			if ((identifier.parent as ts.NamedDeclaration).name === identifier && !ts.isPropertyAccessExpression(identifier.parent)) continue;
			const declaration = host.source.declaration(identifier);
			// A call through an untyped receiver names the property but resolves to nothing; it may reach any implementation.
			if (!declaration) { if (ts.isPropertyAccessExpression(identifier.parent) && identifier.parent.name === identifier && ts.isCallExpression(identifier.parent.parent) && identifier.parent.parent.expression === identifier.parent) sites.push(identifier.parent.parent); continue; }
			if (targets.includes(declaration as ts.Declaration) && !collect(identifier)) return undefined;
		}
		return sites;
	}
	if (ts.isConstructorDeclaration(owner) && ts.isClassLike(owner.parent) && owner.parent.name) {
		const sitesOfClass = (klass: ts.ClassLikeDeclaration, depth: number): boolean => {
			if (!klass.name || depth > 6) return false;
			for (const identifier of host.uses(klass, klass.name.text)) {
				const heritage = identifier.parent.parent;
				const derived = ts.isExpressionWithTypeArguments(identifier.parent) && ts.isHeritageClause(heritage) && heritage.token === ts.SyntaxKind.ExtendsKeyword ? heritage.parent : undefined;
				if (derived && ts.isClassLike(derived)) {
					host.source.walk(derived, node => { if (ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.SuperKeyword && functionOf(node)?.parent === derived) sites.push(node); });
					if (!sitesOfClass(derived, depth + 1)) return false;
				} else if (!collect(identifier)) return false;
			}
			return true;
		};
		return sitesOfClass(owner.parent, 0) ? sites : undefined;
	}
	return undefined;
}

/** The constructor a `new` of the class runs, searching base classes; empty when none is written, `undefined` when a base is not source. */
function constructorFunctions(host: FlowHost, declaration: ts.ClassLikeDeclaration): Functions {
	for (let klass: ts.ClassLikeDeclaration | undefined = declaration, depth = 0; klass && depth < 8; depth++) {
		const implemented = klass.members.filter(ts.isConstructorDeclaration).find(constructor => constructor.body);
		if (implemented) return [implemented];
		const base = klass.heritageClauses?.find(clause => clause.token === ts.SyntaxKind.ExtendsKeyword)?.types[0];
		if (!base) return [];
		const parent = host.source.declaration(base.expression);
		klass = parent && ts.isClassLike(parent) ? parent : undefined;
	}
	return undefined;
}

/** What a function can return, so `await load()` and `makeFactory()` resolve to the functions they hand back. */
function returnedFunctions(host: FlowHost, fn: ts.SignatureDeclaration, visited: Set<ts.Node>): Functions {
	// A function that returns its own result contributes nothing beyond what its other returns already name.
	if (visited.has(fn)) return [];
	visited.add(fn);
	const body = "body" in fn ? fn.body : undefined;
	if (!body || !ts.isBlock(body) && !ts.isExpression(body)) return undefined;
	if (!ts.isBlock(body)) return expressionFunctions(host, body, visited);
	const parts: Functions[] = [];
	host.source.walk(body, node => { if (ts.isReturnStatement(node) && node.expression && functionOf(node) === fn) parts.push(expressionFunctions(host, node.expression, visited)); });
	return union(...parts);
}

const COMPARISONS = [ts.SyntaxKind.EqualsEqualsEqualsToken, ts.SyntaxKind.ExclamationEqualsEqualsToken, ts.SyntaxKind.EqualsEqualsToken, ts.SyntaxKind.ExclamationEqualsToken, ts.SyntaxKind.LessThanToken, ts.SyntaxKind.LessThanEqualsToken, ts.SyntaxKind.GreaterThanToken, ts.SyntaxKind.GreaterThanEqualsToken, ts.SyntaxKind.InstanceOfKeyword, ts.SyntaxKind.InKeyword];

/** An expression whose syntax fixes a primitive result, so calling it can run no function. */
const neverCallable = (expression: ts.Expression): boolean => expression.kind === ts.SyntaxKind.TrueKeyword || expression.kind === ts.SyntaxKind.FalseKeyword || ts.isNumericLiteral(expression)
	|| ts.isStringLiteralLike(expression) || ts.isPrefixUnaryExpression(expression) && expression.operator === ts.SyntaxKind.ExclamationToken
	|| ts.isBinaryExpression(expression) && COMPARISONS.includes(expression.operatorToken.kind);

/** Source functions an expression can evaluate to. */
export function expressionFunctions(host: FlowHost, expression: ts.Expression, visited: Set<ts.Node>): Functions {
	if (ts.isParenthesizedExpression(expression) || ts.isAsExpression(expression) || ts.isNonNullExpression(expression) || ts.isSatisfiesExpression(expression) || ts.isAwaitExpression(expression)) return expressionFunctions(host, expression.expression, visited);
	if (ts.isCallExpression(expression) && expression.expression.kind !== ts.SyntaxKind.ImportKeyword) {
		const callees = calleeFunctions(host, expression, visited);
		return callees === "none" || !callees ? undefined : union(...callees.map(callee => returnedFunctions(host, callee, visited)));
	}
	// An absent value is no function; calling it would throw, so it contributes no callee.
	if (expression.kind === ts.SyntaxKind.NullKeyword || ts.isVoidExpression(expression) || ts.isIdentifier(expression) && expression.text === "undefined" && !host.source.declaration(expression)?.getSourceFile().fileName.includes("/packages/")) return [];
	if (neverCallable(expression)) return [];
	if (ts.isArrowFunction(expression) || ts.isFunctionExpression(expression)) return [expression];
	if (ts.isConditionalExpression(expression)) return union(expressionFunctions(host, expression.whenTrue, visited), expressionFunctions(host, expression.whenFalse, visited));
	if (ts.isBinaryExpression(expression) && SELECTORS.includes(expression.operatorToken.kind)) return union(expressionFunctions(host, expression.left, visited), expressionFunctions(host, expression.right, visited));
	if (ts.isElementAccessExpression(expression)) return tableFunctions(host, expression.expression, visited);
	if (!ts.isIdentifier(expression) && !ts.isPropertyAccessExpression(expression)) return undefined;
	// A shorthand member's name resolves to the property it defines; its value is the variable of the same name.
	const shorthand = ts.isIdentifier(expression) && ts.isShorthandPropertyAssignment(expression.parent) && expression.parent.name === expression ? host.source.checker.getShorthandAssignmentValueSymbol(expression.parent) : undefined;
	const declaration = shorthand ? shorthand.valueDeclaration ?? shorthand.declarations?.[0] : host.source.declaration(ts.isPropertyAccessExpression(expression) ? expression.name : expression);
	return declaration ? declarationFunctions(host, declaration, visited) : undefined;
}

/**
 * Every function a dispatch table can hold: an object written out as literals
 * (initializer or assignment), indexed by a key the analysis does not follow.
 */
function tableFunctions(host: FlowHost, table: ts.Expression, visited: Set<ts.Node>): Functions {
	if (ts.isParenthesizedExpression(table) || ts.isAsExpression(table) || ts.isNonNullExpression(table) || ts.isSatisfiesExpression(table)) return tableFunctions(host, table.expression, visited);
	if (ts.isObjectLiteralExpression(table)) {
		return union(...table.properties.map((member): Functions => {
			if (ts.isMethodDeclaration(member)) return [member];
			if (ts.isPropertyAssignment(member)) return expressionFunctions(host, member.initializer, visited);
			if (ts.isShorthandPropertyAssignment(member)) return expressionFunctions(host, member.name, visited);
			return undefined;
		}));
	}
	if (!ts.isIdentifier(table) && !ts.isPropertyAccessExpression(table)) return undefined;
	const declaration = host.source.declaration(ts.isPropertyAccessExpression(table) ? table.name : table);
	const name = declaration && (declaration as ts.NamedDeclaration).name;
	if (!declaration || !name || !ts.isIdentifier(name) && !ts.isPrivateIdentifier(name) || !ts.isVariableDeclaration(declaration) && !ts.isPropertyDeclaration(declaration)) return undefined;
	if (declaration.getSourceFile().isDeclarationFile) return undefined;
	if (visited.has(declaration)) return [];
	visited.add(declaration);
	const parts: Functions[] = [];
	if (declaration.initializer) parts.push(tableFunctions(host, declaration.initializer, visited));
	for (const identifier of host.uses(declaration, name.text)) {
		const read = readOf(identifier);
		if (ts.isBinaryExpression(read.parent) && read.parent.left === read && ASSIGNMENTS.includes(read.parent.operatorToken.kind)) parts.push(tableFunctions(host, read.parent.right, visited));
	}
	return parts.length ? union(...parts) : undefined;
}

/** Everything written to a variable, parameter or field: its initializer, assignments and, for a parameter, each caller's argument. */
function assignedFunctions(host: FlowHost, declaration: ts.Node, visited: Set<ts.Node>): Functions {
	const name = (declaration as ts.NamedDeclaration).name;
	if (!name || !ts.isIdentifier(name) && !ts.isPrivateIdentifier(name)) return undefined;
	// An ambient declaration is initialised by code outside the source.
	if (declaration.getSourceFile().isDeclarationFile || ts.getCombinedModifierFlags(declaration as ts.Declaration) & ts.ModifierFlags.Ambient) return undefined;
	const parts: Functions[] = [];
	const initializer = (declaration as ts.VariableDeclaration).initializer;
	if (initializer) parts.push(expressionFunctions(host, initializer, visited));
	for (const identifier of host.uses(declaration, name.text)) {
		const read = readOf(identifier);
		if (ts.isBinaryExpression(read.parent) && read.parent.left === read && ASSIGNMENTS.includes(read.parent.operatorToken.kind)) parts.push(expressionFunctions(host, read.parent.right, visited));
	}
	if (ts.isParameter(declaration) && ts.isFunctionLike(declaration.parent)) {
		const owner = declaration.parent;
		const index = owner.parameters.indexOf(declaration);
		const sites = callSites(host, owner);
		if (!sites || declaration.dotDotDotToken) return undefined;
		for (const site of sites) {
			const args = site.arguments ?? [];
			if (args.slice(0, index + 1).some(argument => ts.isSpreadElement(argument))) return undefined;
			if (args[index]) parts.push(expressionFunctions(host, args[index], visited));
		}
	}
	return union(...parts);
}

/** A class that satisfies an interface by shape, without declaring `implements`, is stored and called through it all the same. */
function structurallyImplements(host: FlowHost, klass: ts.ClassLikeDeclaration, contract: ts.Node): boolean {
	if (!ts.isInterfaceDeclaration(contract) || !klass.name) return false;
	const { checker } = host.source;
	return checker.isTypeAssignableTo(checker.getTypeAtLocation(klass.name), checker.getTypeAtLocation(contract.name));
}

/** Overrides, implementing classes and object literals typed by the interface that declares a method or function-typed property. */
function implementations(host: FlowHost, declaration: ts.Node, visited: Set<ts.Node>): Functions {
	let target = declaration;
	if (ts.isBindingElement(target)) {
		const key = target.propertyName ?? target.name;
		const container = host.source.checker.getTypeAtLocation(target.parent);
		const found = ts.isIdentifier(key) ? container.getProperty(key.text) : undefined;
		const property = found && found.flags & ts.SymbolFlags.Alias ? host.source.checker.getAliasedSymbol(found) : found;
		if (!property?.valueDeclaration) return undefined;
		// A module namespace binds exactly the export it names, so the binding is that export itself.
		if (container.symbol && container.symbol.flags & ts.SymbolFlags.ValueModule && !ts.isMethodDeclaration(property.valueDeclaration) && !ts.isPropertyDeclaration(property.valueDeclaration)) return declarationFunctions(host, property.valueDeclaration, visited);
		target = property.valueDeclaration;
	}
	const name = ts.isMethodDeclaration(target) || ts.isMethodSignature(target) || ts.isPropertySignature(target) || ts.isPropertyDeclaration(target) || ts.isFunctionDeclaration(target) ? target.name : undefined;
	if (!name || !ts.isIdentifier(name) && !ts.isPrivateIdentifier(name)) return undefined;
	const found: ts.SignatureDeclaration[] = [];
	if ((ts.isMethodDeclaration(target) || ts.isFunctionDeclaration(target)) && target.body) found.push(target);
	if (ts.isFunctionDeclaration(target)) return found.length ? found : undefined;
	const parts: Functions[] = [found];
	if (ts.isPropertyDeclaration(target)) parts.push(assignedFunctions(host, target, visited));
	// An optional hook whose every writer is accounted for below and holds nothing is absent, so the guarded call runs nothing.
	// A shape written inline at a cast names no declaration to match, so any member of that name may be what it reads.
	const inlineShape = ts.isTypeLiteralNode(target.parent);
	let unaccountedWriter = false;
	for (const identifier of host.named(name.text)) {
		const member = identifier.parent;
		const read = readOf(identifier);
		if (ts.isBinaryExpression(read.parent) && read.parent.left === read && ASSIGNMENTS.includes(read.parent.operatorToken.kind) || ts.isBindingElement(member) || ts.isJsxAttribute(member)) unaccountedWriter = true;
		if (member === target || (member as ts.NamedDeclaration).name !== identifier) continue;
		const container = member.parent;
		const signature = ts.isPropertySignature(member) || ts.isMethodSignature(member);
		if (ts.isClassLike(container) && (inlineShape || derives(host, container, target.parent) || structurallyImplements(host, container, target.parent))) {
			if (ts.isMethodDeclaration(member) && member.body) found.push(member);
			else if (ts.isPropertyDeclaration(member)) parts.push(assignedFunctions(host, member, visited));
		} else if (ts.isObjectLiteralExpression(container) && (ts.isPropertyAssignment(member) || ts.isMethodDeclaration(member) || ts.isShorthandPropertyAssignment(member))) {
			const contextual = host.source.checker.getContextualType(container);
			const types = contextual?.isUnion() ? contextual.types : contextual ? [contextual] : [];
			if (!inlineShape && !types.some(type => type.getProperty(name.text)?.declarations?.includes(target as ts.Declaration))) { unaccountedWriter = true; continue; }
			parts.push(ts.isMethodDeclaration(member) ? [member] : ts.isPropertyAssignment(member) ? expressionFunctions(host, member.initializer, visited) : expressionFunctions(host, member.name, visited));
		} else if (!signature && !ts.isPropertyAccessExpression(member)) unaccountedWriter = true;
	}
	const all = union(...parts);
	if (all && !all.length && !unaccountedWriter && (ts.isPropertySignature(target) || ts.isMethodSignature(target)) && target.questionToken) return all;
	return all?.length ? all : undefined;
}

/** What a declaration can be when called; `undefined` when some reaching value is not a source function. */
export function declarationFunctions(host: FlowHost, declaration: ts.Node, visited: Set<ts.Node>): Functions {
	if (visited.has(declaration)) return [];
	visited.add(declaration);
	if (ts.isVariableDeclaration(declaration) || ts.isParameter(declaration) || ts.isPropertyAssignment(declaration) || ts.isShorthandPropertyAssignment(declaration)) {
		if (ts.isParameter(declaration) && ts.isObjectBindingPattern(declaration.name)) return undefined;
		if (ts.isPropertyAssignment(declaration)) return expressionFunctions(host, declaration.initializer, visited);
		if (ts.isShorthandPropertyAssignment(declaration)) return expressionFunctions(host, declaration.name, visited);
		return assignedFunctions(host, declaration, visited);
	}
	if (ts.isClassLike(declaration)) return constructorFunctions(host, declaration);
	return implementations(host, declaration, visited);
}

/** The node whose declaration a call runs: the member name, seen through `!` and parentheses. */
export function calleeTarget(call: Call): ts.Node {
	let callee: ts.Expression = call.expression;
	while (ts.isNonNullExpression(callee) || ts.isParenthesizedExpression(callee)) callee = callee.expression;
	return ts.isPropertyAccessExpression(callee) ? callee.name : callee;
}

/** The call's source functions, `"none"` for a bodiless default constructor, `undefined` when they cannot be enumerated. */
export function calleeFunctions(host: FlowHost, call: Call, visited: Set<ts.Node> = new Set()): Functions | "none" {
	const declaration = host.source.declaration(calleeTarget(call));
	if (!declaration) return undefined;
	if (!ts.isNewExpression(call)) return declarationFunctions(host, declaration, visited);
	if (ts.isClassLike(declaration)) {
		const constructors = constructorFunctions(host, declaration);
		return constructors?.length === 0 ? "none" : constructors;
	}
	return declarationFunctions(host, declaration, visited);
}
