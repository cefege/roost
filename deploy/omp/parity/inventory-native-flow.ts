import ts from "typescript";
import { calleeFunctions, calleeTarget, expressionFunctions, functionOf, inertReference, readOf, type FlowHost } from "./inventory-native-callees";
import { cannotHoldFunction, carriesOnlyData } from "./inventory-native-data";

/**
 * Forward data-flow of one constructed value (class instance or object literal)
 * through aliases, fields, parameters and return values. Every node the value
 * is held or consumed at is reported so the caller can place it inside a
 * component member; any flow this analysis cannot follow rejects the proof.
 */
export type { FlowHost };
/** `parents` maps each reached node to every node it was reached from; `calls` lists the calls made through a traced function value and `handoffs` the library calls that receive it and run it themselves; `keyedReads` are container nodes read through a property, which either selects the traced property (the selection is traced on) or cannot reach it. */
export type ValueTrace = { values: ts.Node[]; parents: Map<ts.Node, Set<ts.Node>>; calls: ts.Node[]; handoffs: ts.Node[]; keyedReads: Set<ts.Node> } | { rejected: string; at: ts.Node; parents: Map<ts.Node, Set<ts.Node>> };
/** `instance` is the constructed object itself; `value` a container that may hold it; `callable` a function returning it. */
type Taint = "instance" | "value" | "callable" | "function";

const MAX_VALUES = 40000;
const wrapper = (node: ts.Node): boolean => ts.isParenthesizedExpression(node) || ts.isAsExpression(node) || ts.isNonNullExpression(node)
	|| ts.isSatisfiesExpression(node) || ts.isAwaitExpression(node) || ts.isTypeAssertionExpression(node);
/** Library receivers that hold what they are given until it is read back out. */
const CONTAINER_ADDERS = new Set(["push", "unshift", "splice", "add", "set", "fill"]);
const CALLBACK_INVOKERS = new Set(["map", "flatMap", "forEach", "filter", "find", "findIndex", "findLast", "findLastIndex", "some", "every", "reduce", "reduceRight", "sort", "toSorted", "then", "catch", "finally", "setTimeout", "setInterval", "setImmediate", "queueMicrotask"]);
/** Array and collection methods that hand each element to their callback. */
const ELEMENT_CALLBACKS = new Set(["map", "flatMap", "forEach", "filter", "find", "findIndex", "findLast", "findLastIndex", "some", "every", "reduce", "reduceRight", "sort", "toSorted"]);
/** Library functions that only look at their first argument and never call it. */
const INSPECTORS = new Set(["isArray", "keys", "values", "entries", "getOwnPropertyNames", "freeze", "isFrozen", "isSealed", "seal", "hasOwn", "stringify", "isFinite", "isInteger", "isNaN", "log", "debug", "info", "warn", "error"]);
type Path = readonly string[];
const MAX_PATH = 3;
/** A path whose first segment names one property, so a read of any other property misses the value. */
const named = (path: Path): boolean => path.length > 0 && path[0] !== "*";
const ELEMENT_READERS = new Set(["filter", "slice", "concat", "flat", "at", "find", "findLast", "pop", "shift", "get", "values", "entries", "keys", "toSorted", "toReversed", "with", "reverse", "sort"]);
const RETAINING_LIBRARY_CALLS = new Set(["then", "catch", "finally", "resolve", "reject", "setTimeout", "setInterval", "setImmediate", "queueMicrotask", "addEventListener", "on", "once", "call", "apply", "defineProperty", "defineProperties", "bind", "registerTool"]);
const PASS_THROUGH_OPERATORS = new Set([ts.SyntaxKind.AmpersandAmpersandToken, ts.SyntaxKind.BarBarToken, ts.SyntaxKind.QuestionQuestionToken, ts.SyntaxKind.CommaToken]);
const ASSIGNMENT_OPERATORS = new Set([ts.SyntaxKind.EqualsToken, ts.SyntaxKind.BarBarEqualsToken, ts.SyntaxKind.QuestionQuestionEqualsToken, ts.SyntaxKind.AmpersandAmpersandEqualsToken]);

/** `instance` traces a constructed object; `function` traces a function value, reporting in `calls` every place it can run. */
/**
 * Calling a promise's resolver settles that promise, so the argument reaches
 * whatever awaits it: `new Promise((resolve) => …)` yields the construction,
 * and `const { promise, resolve } = Promise.withResolvers()` the `promise` binding.
 */
function settledPromise(resolver: ts.Node): { promise: ts.Node; name?: string } | undefined {
	if (ts.isParameter(resolver) && ts.isFunctionLike(resolver.parent) && ts.isNewExpression(resolver.parent.parent) && resolver.parent.parent.expression.getText() === "Promise") return { promise: resolver.parent.parent };
	const pattern = ts.isBindingElement(resolver) && ts.isObjectBindingPattern(resolver.parent) ? resolver.parent : undefined;
	const declaration = pattern?.parent;
	const initializer = declaration && ts.isVariableDeclaration(declaration) ? declaration.initializer : undefined;
	if (!pattern || !declaration || !initializer || !ts.isCallExpression(initializer) || initializer.expression.getText() !== "Promise.withResolvers") return undefined;
	const promise = pattern.elements.find(element => ts.isIdentifier(element.name) && (element.propertyName ?? element.name).getText() === "promise");
	return promise && ts.isIdentifier(promise.name) ? { promise, name: promise.name.text } : undefined;
}

export function traceValue(host: FlowHost, start: ts.Node, mode: "instance" | "function" = "instance"): ValueTrace {
	const values: ts.Node[] = [];
	const calls: ts.Node[] = [];
	const handoffs: ts.Node[] = [];
	const keyedReads = new Set<ts.Node>();
	const seen = new Map<ts.Node, Set<string>>();
	const queue: Array<{ node: ts.Node; taint: Taint; path: Path }> = [{ node: start, taint: mode === "function" ? "function" : ts.isObjectLiteralExpression(start) ? "value" : "instance", path: [] }];
	let rejection: { rejected: string; at: ts.Node; parents: Map<ts.Node, Set<ts.Node>> } | undefined;
	const reject = (reason: string, at: ts.Node): void => { rejection ??= { rejected: reason, at, parents }; };
	const parents = new Map<ts.Node, Set<ts.Node>>();
	let current: ts.Node = start;
	/**
	 * Where the traced value sits inside what is being followed, outermost key
	 * first: `{ fetch: { render: fn } }` holds it at `["fetch", "render"]`. A read
	 * steps through one segment, so a sibling property never carries it; `"*"`
	 * is any key or element and an empty path is the value itself.
	 */
	let currentPath: Path = [];
	const withPath = (path: Path, action: () => void): void => {
		const saved = currentPath;
		currentPath = path;
		action();
		currentPath = saved;
	};
	const wrapped = (segment: string): Path => currentPath.length < MAX_PATH ? [segment, ...currentPath] : [];
	const enqueue = (node: ts.Node, taint: Taint): void => {
		const from = parents.get(node) ?? new Set<ts.Node>();
		from.add(current);
		parents.set(node, from);
		queue.push({ node, taint, path: currentPath });
	};
	const propertyKey = (name: ts.PropertyName | ts.BindingName | undefined): string | undefined => name && (ts.isIdentifier(name) || ts.isStringLiteral(name) || ts.isNoSubstitutionTemplateLiteral(name)) ? name.text : undefined;
	const isAssignmentTarget = (read: ts.Node): boolean => ts.isBinaryExpression(read.parent) && read.parent.left === read && ASSIGNMENT_OPERATORS.has(read.parent.operatorToken.kind);
	/** A place the value runs or is read, reached from `from`, so a climb out of it follows the flow that delivered the value. */
	const landAt = (node: ts.Node, from: ts.Node): void => {
		values.push(node);
		const origins = parents.get(node) ?? new Set<ts.Node>();
		origins.add(from);
		parents.set(node, origins);
	};
	const enqueueReferences = (declaration: ts.Node, name: string, taint: Taint, scope?: ts.Node): void => {
		for (const identifier of host.uses(declaration, name, scope)) {
			const read = readOf(identifier);
			if (!isAssignmentTarget(read)) enqueue(read, taint);
		}
	};
	const bindName = (name: ts.BindingName, declaration: ts.Node, taint: Taint, scope: ts.Node | undefined): void => {
		if (ts.isIdentifier(name)) { enqueueReferences(declaration, name.text, taint, scope); return; }
		for (const element of name.elements) {
			if (ts.isOmittedExpression(element)) continue;
			const elementTaint = ts.isArrayBindingPattern(name) || element.dotDotDotToken ? containerOf(taint) : taint;
			const property = ts.isObjectBindingPattern(name) && !element.dotDotDotToken ? propertyKey(element.propertyName ?? element.name) : undefined;
			if (property !== undefined && named(currentPath) && property !== currentPath[0]) continue;
			// The rest of an object pattern holds only the properties no sibling element named.
			if (named(currentPath) && ts.isObjectBindingPattern(name) && element.dotDotDotToken && name.elements.some(sibling => !sibling.dotDotDotToken && propertyKey(sibling.propertyName ?? sibling.name) === currentPath[0])) continue;
			withPath(ts.isArrayBindingPattern(name) ? [] : property !== undefined ? named(currentPath) ? currentPath.slice(1) : [] : currentPath, () => bindName(element.name, element, elementTaint, scope));
		}
	};
	const field = (target: ts.Node, taint: Taint): void => {
		const name = ts.isPropertyAccessExpression(target) ? target.name : target;
		const declaration = host.source.declaration(name);
		if (!declaration && ts.isPropertyAccessExpression(target)) { withPath(wrapped(target.name.text), () => holdContainer(target.expression, taint)); return; }
		if (!declaration || !(ts.isIdentifier(name) || ts.isPrivateIdentifier(name))) { reject("assignment target has no declaration", target); return; }
		if (ts.isParameter(declaration) || ts.isVariableDeclaration(declaration)) {
			enqueueReferences(declaration, name.text, taint, ts.isParameter(declaration) && !declaration.modifiers?.length ? functionOf(declaration) : undefined);
			return;
		}
		enqueueReferences(declaration, name.text, taint);
	};
	/** The expression that holds what a container-adding call receives: `list.push(x)` makes `list` carry it. */
	const holdContainer = (receiver: ts.Expression, taint: Taint): void => {
		if (ts.isParenthesizedExpression(receiver) || ts.isAsExpression(receiver) || ts.isNonNullExpression(receiver) || ts.isSatisfiesExpression(receiver)) { holdContainer(receiver.expression, taint); return; }
		if (ts.isElementAccessExpression(receiver)) { holdContainer(receiver.expression, taint); return; }
		// Either branch of a choice can be the list that receives the value.
		if (ts.isConditionalExpression(receiver)) { holdContainer(receiver.whenTrue, taint); holdContainer(receiver.whenFalse, taint); return; }
		if (ts.isBinaryExpression(receiver) && PASS_THROUGH_OPERATORS.has(receiver.operatorToken.kind)) { holdContainer(receiver.left, taint); holdContainer(receiver.right, taint); return; }
		const target = ts.isPropertyAccessExpression(receiver) ? receiver.name : receiver;
		const declaration = ts.isIdentifier(target) || ts.isPrivateIdentifier(target) ? host.source.declaration(target) : undefined;
		if (!declaration || !(ts.isIdentifier(target) || ts.isPrivateIdentifier(target))) { reject(`value is added to ${receiver.getText().slice(0, 40)}, which has no declaration`, receiver); return; }
		enqueueReferences(declaration, target.text, taint === "function" ? taint : "value", ts.isVariableDeclaration(declaration) || ts.isParameter(declaration) ? functionOf(declaration) : undefined);
	};
	const libraryCall = (call: ts.CallExpression | ts.NewExpression, index: number, taint: Taint): void => {
		const callee = call.expression;
		const name = ts.isPropertyAccessExpression(callee) ? callee.name.text : ts.isIdentifier(callee) ? callee.text : "";
		if (taint === "function") {
			// A library that is not merely storing the callback invokes it itself, so the call that hands it over is where it runs; a stored one runs where it is read back.
			if (!CONTAINER_ADDERS.has(name) && !(index === 0 && INSPECTORS.has(name))) handoffs.push(call);
			// These only call the callback and keep what it returns, never the callback.
			if (index === 0 && CALLBACK_INVOKERS.has(name)) return;
			withPath([], () => {
				enqueue(call, taint);
				if (ts.isPropertyAccessExpression(callee) && CONTAINER_ADDERS.has(name)) holdContainer(callee.expression, taint);
			});
			return;
		}
		if (name === "assign" && index === 0) { enqueue(call, taint); return; }
		if (name === "assign" && call.arguments?.[0]) { const target = call.arguments[0]; withPath([], () => holdContainer(target, taint)); return; }
		// A callback's return value becomes the settled value of the promise `then` hands back.
		if (taint === "callable" && ts.isPropertyAccessExpression(callee) && (name === "then" || name === "catch" || name === "finally")) { withPath([], () => enqueue(call, "value")); return; }
		if (RETAINING_LIBRARY_CALLS.has(name)) { reject(`value flows into library call ${name}, which retains it`, call); return; }
		if (ts.isNewExpression(call) || name === "from" || name === "of") { withPath([], () => enqueue(call, "value")); return; }
		if (ts.isPropertyAccessExpression(callee) && CONTAINER_ADDERS.has(name)) withPath([], () => holdContainer(callee.expression, taint));
		if (ts.isPropertyAccessExpression(callee) && (name === "map" || name === "flatMap") && taint === "callable") withPath([], () => enqueue(call, "value"));
	};
	/** A spread before an argument leaves its parameter position open, so every later parameter, and a rest parameter, may receive it. */
	const argumentFlow = (call: ts.CallExpression | ts.NewExpression, argument: ts.Node, taint: Taint): void => {
		const args: readonly ts.Node[] = call.arguments ?? [];
		const position = args.indexOf(argument);
		const spreadsBefore = args.slice(0, position).filter(item => ts.isSpreadElement(item)).length;
		const open = spreadsBefore > 0 || ts.isSpreadElement(argument);
		const lowest = position - spreadsBefore;
		const target = calleeTarget(call);
		const declaration = host.source.declaration(target);
		if (declaration?.getSourceFile().isDeclarationFile) { libraryCall(call, position, taint); return; }
		const settled = ts.isIdentifier(target) && declaration ? settledPromise(declaration) : undefined;
		if (settled) { withPath([], () => settled.name ? enqueueReferences(settled.promise, settled.name, "value") : enqueue(settled.promise, "value")); return; }
		const callees = calleeFunctions(host, call);
		if (callees === "none") return;
		if (!callees) { reject(`value flows into ${call.expression.getText().slice(0, 50)}, which has no enumerable source function`, call); return; }
		for (const callee of callees) {
			callee.parameters.forEach((parameter, slot) => {
				const rest = parameter.dotDotDotToken !== undefined;
				const hit = rest ? open || lowest >= slot : open ? slot >= lowest : slot === lowest;
				if (!hit) return;
				const holdsField = !!parameter.modifiers?.some(modifier => modifier.kind !== ts.SyntaxKind.OverrideKeyword);
				const bound = rest ? containerOf(taint) : taint;
				withPath(rest ? wrapped("*") : currentPath, () => {
					bindName(parameter.name, parameter, bound, holdsField ? undefined : callee);
					if (holdsField && ts.isIdentifier(parameter.name)) enqueueReferences(parameter, parameter.name.text, bound);
				});
			});
		}
	};
	/** `elements.forEach(callback)` gives each element to the callback's first parameter (its second for `reduce`, both for a comparator), which is where the traced value is then used. */
	const elementCallbacks = (call: ts.CallExpression, method: string): void => {
		const callback = call.arguments[0];
		if (!callback) return;
		const callbacks = expressionFunctions(host, callback, new Set());
		if (!callbacks) { reject(`${method} callback has no enumerable source function`, call); return; }
		const slots = method.startsWith("reduce") ? [1] : method === "sort" || method === "toSorted" ? [0, 1] : [0];
		for (const fn of callbacks) for (const slot of slots) {
			const parameter = fn.parameters[slot];
			if (parameter) withPath([], () => bindName(parameter.name, parameter, "value", fn));
		}
	};
	const returned = (fn: ts.SignatureDeclaration | undefined, taint: Taint, at: ts.Node): void => {
		if (!fn) { reject("value is returned outside a function", at); return; }
		if (ts.isConstructorDeclaration(fn)) return;
		if (ts.isArrowFunction(fn) || ts.isFunctionExpression(fn)) { enqueue(fn, "callable"); return; }
		const name = fn.name;
		if (!name || !(ts.isIdentifier(name) || ts.isPrivateIdentifier(name))) { reject("value is returned from a function with a computed name", at); return; }
		const getter = ts.isGetAccessorDeclaration(fn);
		const declaration = host.source.declaration(name);
		if (declaration) enqueueReferences(declaration, name.text, getter ? taint : "callable");
	};
	/** The place an assignment writes: a variable, a field, a container element, or each target of a destructuring pattern. */
	const assignTo = (target: ts.Expression, taint: Taint): void => {
		if (ts.isParenthesizedExpression(target)) { assignTo(target.expression, taint); return; }
		if (ts.isIdentifier(target) || ts.isPropertyAccessExpression(target)) {
			const name = ts.isIdentifier(target) ? target : target.name;
			const declaration = host.source.declaration(name);
			if (declaration && (ts.isVariableDeclaration(declaration) || ts.isParameter(declaration)) && ts.isIdentifier(target)) enqueueReferences(declaration, name.text, taint, functionOf(declaration));
			else field(target, taint);
		} else if (ts.isElementAccessExpression(target)) withPath([], () => holdContainer(target.expression, taint));
		else if (ts.isArrayLiteralExpression(target)) {
			for (const element of target.elements) {
				if (ts.isOmittedExpression(element)) continue;
				withPath([], () => assignTo(ts.isSpreadElement(element) ? element.expression : element, ts.isSpreadElement(element) ? taint : containerOf(taint)));
			}
		} else if (ts.isObjectLiteralExpression(target)) {
			for (const property of target.properties) {
				if (ts.isSpreadAssignment(property)) { assignTo(property.expression, taint); continue; }
				const key = ts.isShorthandPropertyAssignment(property) ? property.name.text : ts.isPropertyAssignment(property) ? propertyKey(property.name) : undefined;
				if (key !== undefined && named(currentPath) && key !== currentPath[0]) continue;
				const slot = ts.isShorthandPropertyAssignment(property) ? property.name : ts.isPropertyAssignment(property) ? property.initializer : undefined;
				// `{ key: target = fallback }` writes `target`; the fallback is only a default.
				const written = slot && ts.isBinaryExpression(slot) && slot.operatorToken.kind === ts.SyntaxKind.EqualsToken ? slot.left : slot;
				if (!written) { reject("value is destructured into a computed property", property); return; }
				withPath(key !== undefined && named(currentPath) ? currentPath.slice(1) : [], () => assignTo(written, taint));
			}
		} else reject("value is assigned to a computed target", target);
	};
	const containerOf = (taint: Taint): Taint => taint === "instance" ? "value" : taint;

	const step = (node: ts.Node, taint: Taint): void => {
		const path = currentPath;
		if (node !== start && !ts.isFunctionLike(node) && carriesOnlyData(host.source.checker, host.source.checker.getTypeAtLocation(node))) return;
		// A function value itself cannot sit in a binding whose type has members a function lacks, however many parameters a spread offered it to.
		if (node !== start && taint === "function" && path.length === 0 && !ts.isFunctionLike(node) && cannotHoldFunction(host.source.checker, host.source.checker.getTypeAtLocation(node))) return;
		const holder = node.parent;
		if (inertReference(holder)) return;
		values.push(node);
		let child: ts.Node = node;
		let parent = node.parent;
		while (wrapper(parent)) { child = parent; parent = parent.parent; }
		if (ts.isPropertyAccessExpression(parent) && parent.expression === child) {
			// A read of another property of the container never touches the traced value.
			if (named(path) && path[0] !== parent.name.text && !ELEMENT_READERS.has(parent.name.text)) return;
			landAt(parent, node);
			const call = ts.isCallExpression(parent.parent) && parent.parent.expression === parent ? parent.parent : undefined;
			if (path.length > 0 && !ELEMENT_READERS.has(parent.name.text)) keyedReads.add(node);
			if (call && taint === "function") {
				calls.push(call);
				if (["bind", "call", "apply"].includes(parent.name.text) || ELEMENT_READERS.has(parent.name.text)) withPath([], () => enqueue(call, taint));
			} else if (call) {
				if (taint === "value" && ELEMENT_READERS.has(parent.name.text)) withPath([], () => enqueue(call, "value"));
				if (taint === "value" && ELEMENT_CALLBACKS.has(parent.name.text)) elementCallbacks(call, parent.name.text);
			}
			else if (taint !== "instance") withPath(path[0] === parent.name.text ? path.slice(1) : [], () => enqueue(parent, taint));
		} else if (ts.isElementAccessExpression(parent)) {
			const literal = ts.isStringLiteralLike(parent.argumentExpression) ? parent.argumentExpression.text : undefined;
			if (parent.expression === child && taint !== "instance" && !(named(path) && literal !== undefined && literal !== path[0])) withPath(path.slice(1), () => enqueue(parent, taint));
		} else if (ts.isCallExpression(parent) || ts.isNewExpression(parent)) {
			if (parent.expression === child) {
				landAt(parent, node);
				if (taint === "function") calls.push(parent);
				if (ts.isCallExpression(parent) && taint === "callable") enqueue(parent, mode === "function" ? "function" : "value");
				// A traced function that is a class runs its constructor here; any other value cannot be constructed.
				if (ts.isNewExpression(parent) && taint !== "function") reject("value is constructed", parent);
			} else if (parent.arguments) argumentFlow(parent, child, taint);
		} else if (ts.isVariableDeclaration(parent) && parent.initializer === child) {
			const scope = ts.isVariableDeclarationList(parent.parent) && ts.isVariableStatement(parent.parent.parent) && ts.isSourceFile(parent.parent.parent.parent) ? undefined : functionOf(parent);
			bindName(parent.name, parent, taint, scope);
		} else if ((ts.isParameter(parent) || ts.isBindingElement(parent)) && parent.initializer === child) {
			// A default value is what the parameter or element holds when nothing else is passed.
			bindName(parent.name, parent, taint, functionOf(parent));
		} else if (ts.isBindingElement(parent) && parent.propertyName === child) {
			// `{ fetch: impl } = options` reads the property the flow stored under that name.
			withPath([], () => bindName(parent.name, parent, taint, functionOf(parent)));
		} else if (ts.isForOfStatement(parent) && parent.expression === child) {
			if (ts.isVariableDeclarationList(parent.initializer) && parent.initializer.declarations[0]) {
				const declaration = parent.initializer.declarations[0];
				bindName(declaration.name, declaration, taint === "value" ? "value" : taint, functionOf(declaration));
			} else reject("value is iterated into an existing variable", parent);
		} else if (ts.isBinaryExpression(parent)) {
			if (ASSIGNMENT_OPERATORS.has(parent.operatorToken.kind) && parent.right === child) {
				assignTo(parent.left, taint);
			} else if (PASS_THROUGH_OPERATORS.has(parent.operatorToken.kind)) enqueue(parent, taint);
		} else if (ts.isConditionalExpression(parent)) { if (parent.condition !== child) enqueue(parent, taint); }
		else if (ts.isPropertyDeclaration(parent) && parent.initializer === child) field(parent.name, taint);
		else if (ts.isPropertyAssignment(parent) && parent.initializer === child) withPath(wrapped(propertyKey(parent.name) ?? "*"), () => enqueue(parent.parent, containerOf(taint)));
		else if (ts.isShorthandPropertyAssignment(parent)) withPath(wrapped(propertyKey(parent.name) ?? "*"), () => enqueue(parent.parent, containerOf(taint)));
		else if (ts.isSpreadAssignment(parent)) enqueue(parent.parent, containerOf(taint));
		else if (ts.isArrayLiteralExpression(parent)) withPath(wrapped("*"), () => enqueue(parent, containerOf(taint)));
		else if (ts.isSpreadElement(parent) && ts.isArrayLiteralExpression(parent.parent)) withPath([], () => enqueue(parent.parent, containerOf(taint)));
		else if (ts.isSpreadElement(parent) && (ts.isCallExpression(parent.parent) || ts.isNewExpression(parent.parent))) {
			const spreadCall = parent.parent;
			withPath(path[0] === "*" ? path.slice(1) : path, () => argumentFlow(spreadCall, parent, containerOf(taint)));
		}
		else if (ts.isReturnStatement(parent)) returned(functionOf(parent.parent), taint, parent);
		else if (ts.isArrowFunction(parent) && parent.body === child) returned(parent, taint, parent);
		else if (ts.isExpressionStatement(parent) || ts.isVoidExpression(parent) || ts.isDeleteExpression(parent) || ts.isTypeOfExpression(parent) || ts.isIfStatement(parent) || ts.isTemplateSpan(parent)
			|| ts.isPrefixUnaryExpression(parent) || ts.isForInStatement(parent) || ts.isCaseClause(parent) || ts.isSwitchStatement(parent) || ts.isThrowStatement(parent) || ts.isWhileStatement(parent)) return;
		else reject(`value reaches an unrecognised ${ts.SyntaxKind[parent.kind]} via ${ts.SyntaxKind[child.kind]} ${child.getText().slice(0, 40)}`, parent);
	};

	while (queue.length && !rejection) {
		const next = queue.pop()!;
		const keys = seen.get(next.node) ?? new Set<string>();
		const key = next.path.length ? next.path.join("\0") : "*";
		if (keys.has(key) || keys.has("*")) continue;
		keys.add(key);
		seen.set(next.node, keys);
		if (seen.size > MAX_VALUES) { reject("value flow is too large to follow", next.node); break; }
		current = next.node;
		currentPath = next.path;
		step(next.node, next.taint);
	}
	return rejection ?? { values, parents, calls, handoffs, keyedReads };
}
