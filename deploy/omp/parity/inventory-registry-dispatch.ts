//! Proves a lazy command registry is dispatched by argv only: its `load` thunks
//! are not session edges when every `.load()` call is reached solely through
//! exported functions that no session file calls. Used by `inventory-site-reach.ts`.
import ts from "typescript";
import type { SourceIndex } from "./inventory-source";

interface Located { source: string; line: number }
type NamedFunction = ts.FunctionDeclaration | ts.MethodDeclaration;

export interface RegistryDispatchProof {
	/** Each `<entry>.load()` call in the session closure and the function that holds it. */
	invokers: Array<Located & { function: string }>;
	/** The exported functions every invoker is reached through. */
	dispatchers: Array<Located & { name: string }>;
	importersChecked: number;
	/** `return require(…)` sites that give a dispatcher's module namespace to code outside the fork (extension code); the fork never calls it. */
	namespaceHandoffs: Located[];
	/** Calls in the closure that start another process from this executable. */
	selfSpawnSites: Array<Located & { expression: string }>;
}
export type RegistryDispatch = { proven: RegistryDispatchProof } | { unproven: string };

/** `{ name: "x", load: () => import("./x") }`: the import runs only when something calls `load`. */
export function isRegistryThunk(node: ts.Node): boolean {
	for (let current: ts.Node | undefined = node.parent; current && !ts.isSourceFile(current); current = current.parent) {
		if (ts.isPropertyAssignment(current) && current.name.getText() === "load" && ts.isObjectLiteralExpression(current.parent)
			&& current.parent.properties.some(property => ts.isPropertyAssignment(property) && property.name.getText() === "name" && ts.isStringLiteralLike(property.initializer))) return true;
	}
	return false;
}

const SPAWNERS = /(^|\.)(spawn|spawnSync|exec|execFile|execSync|fork)$/;

function enclosingFunction(node: ts.Node): NamedFunction | undefined {
	for (let current: ts.Node | undefined = node.parent; current; current = current.parent) {
		if ((ts.isFunctionDeclaration(current) || ts.isMethodDeclaration(current)) && current.name && current.body) return current;
	}
	return undefined;
}

const exported = (fn: NamedFunction): boolean => {
	if (ts.isMethodDeclaration(fn)) return false;
	const name = fn.name!.getText();
	return (ts.getModifiers(fn) ?? []).some(modifier => modifier.kind === ts.SyntaxKind.ExportKeyword)
		|| fn.getSourceFile().statements.some(statement => ts.isExportDeclaration(statement) && !statement.moduleSpecifier && statement.exportClause && ts.isNamedExports(statement.exportClause) && statement.exportClause.elements.some(element => (element.propertyName ?? element.name).getText() === name)
			|| ts.isExportAssignment(statement) && statement.expression.getText() === name);
};

/** The exported functions a call to `fn` can only come from, through named callers inside `files`; `undefined` when a caller is not a named function. */
function dispatchers(source: SourceIndex, files: readonly ts.SourceFile[], fn: NamedFunction, seen: Set<NamedFunction>, depth = 0): NamedFunction[] | undefined {
	if (exported(fn)) return [fn];
	// A method is reached by virtual dispatch, which a symbol search cannot enumerate.
	if (ts.isMethodDeclaration(fn)) return undefined;
	if (depth > 6 || seen.has(fn)) return undefined;
	seen.add(fn);
	const found: NamedFunction[] = [];
	let failed = false;
	const name = fn.name!.getText();
	for (const file of files) source.walk(file, node => {
		if (failed || !ts.isIdentifier(node) || node.text !== name || node === fn.name || !source.symbol(node)?.declarations?.includes(fn)) return;
		const callee = ts.isPropertyAccessExpression(node.parent) && node.parent.name === node ? node.parent : node;
		const call = callee.parent;
		const caller = ts.isCallExpression(call) && call.expression === callee ? enclosingFunction(call) : undefined;
		const reached = caller && dispatchers(source, files, caller, seen, depth + 1);
		if (!reached) failed = true;
		else found.push(...reached);
	});
	seen.delete(fn);
	return failed ? undefined : found;
}

/** The pattern an import in `await Promise.all([import(…), …])` is destructured into: `[{ a }, { b }] = await …`. */
function destructuredImport(call: ts.CallExpression): ts.ObjectBindingPattern | undefined {
	const declared = (value: ts.Node) => ts.isAwaitExpression(value) && ts.isVariableDeclaration(value.parent) ? value.parent : undefined;
	const list = call.parent;
	if (!ts.isArrayLiteralExpression(list) || !ts.isCallExpression(list.parent) || list.parent.expression.getText() !== "Promise.all") return undefined;
	const declaration = declared(list.parent.parent);
	if (!declaration || !ts.isArrayBindingPattern(declaration.name)) return undefined;
	const element = declaration.name.elements[list.elements.indexOf(call)];
	return element && ts.isBindingElement(element) && ts.isObjectBindingPattern(element.name) ? element.name : undefined;
}

interface Carry { names: ts.Identifier[]; namespaces: ts.Identifier[]; /** `(await import(…)).name` accesses of the dispatcher. */ members: ts.PropertyAccessExpression[]; /** The module's exports are returned to callers outside the traced code. */ escapes: boolean }

/** How the import-like `node` carries `name` into its file, or a violation message when it can carry it somewhere untraceable. */
function carryOf(node: ts.Node, name: string): Carry | string {
	const carry: Carry = { names: [], namespaces: [], members: [], escapes: false };
	const matching = <T extends ts.ImportSpecifier | ts.ExportSpecifier | ts.BindingElement>(elements: readonly T[]) => elements.filter(element => (element.propertyName ?? element.name).getText() === name);
	if (ts.isImportDeclaration(node)) {
		const bindings = node.importClause?.namedBindings;
		if (node.importClause?.isTypeOnly || !bindings) return carry;
		if (ts.isNamespaceImport(bindings)) carry.namespaces.push(bindings.name);
		else carry.names.push(...matching(bindings.elements).filter(element => !element.isTypeOnly).map(element => element.name));
		return carry;
	}
	if (ts.isExportDeclaration(node)) {
		// `export *` hands the names to the re-exporter, whose importers are checked in turn.
		if (node.isTypeOnly || !node.exportClause) return carry;
		if (ts.isNamespaceExport(node.exportClause)) return "namespace re-export";
		return matching(node.exportClause.elements).length ? "named re-export" : carry;
	}
	if (!ts.isCallExpression(node)) return "import-equals";
	const value = ts.isAwaitExpression(node.parent) ? node.parent : node;
	let held: ts.Node = value;
	while (ts.isParenthesizedExpression(held.parent)) held = held.parent;
	const declaration = ts.isVariableDeclaration(value.parent) ? value.parent : undefined;
	if (declaration && ts.isIdentifier(declaration.name)) carry.namespaces.push(declaration.name);
	else if (declaration && ts.isObjectBindingPattern(declaration.name)) return pushNames(carry, matching(declaration.name.elements));
	else if (ts.isReturnStatement(value.parent) && ts.isIdentifier(node.expression)) carry.escapes = true;
	else if (ts.isAwaitExpression(value) && ts.isPropertyAccessExpression(held.parent) && held.parent.expression === held) {
		if (held.parent.name.text === name) carry.members.push(held.parent);
	}
	else {
		const pattern = destructuredImport(node);
		if (!pattern) return `${node.expression.getText()} whose result is not bound`;
		return pushNames(carry, matching(pattern.elements));
	}
	return carry;
}

function pushNames(carry: Carry, elements: readonly ts.BindingElement[]): Carry | string {
	if (!elements.every(element => ts.isIdentifier(element.name))) return "nested destructuring of the dispatcher";
	carry.names.push(...elements.map(element => element.name as ts.Identifier));
	return carry;
}

/** The exported functions that call `dispatcher` through the bindings the closure's importers bind it to; a message when a use cannot be traced. */
function importedCallers(source: SourceIndex, files: readonly ts.SourceFile[], dispatcher: NamedFunction): { callers: NamedFunction[]; importers: number; handoffs: Located[] } | string {
	const name = dispatcher.name!.getText();
	const modules = new Set([dispatcher.getSourceFile().fileName]);
	// A module re-exporting a dispatcher's file hands its exports to whoever imports the re-exporter.
	for (let grown = true; grown;) {
		grown = false;
		for (const file of source.files) {
			if (modules.has(file.fileName)) continue;
			for (const statement of file.statements) {
				if (!ts.isExportDeclaration(statement) || !statement.moduleSpecifier || !ts.isStringLiteralLike(statement.moduleSpecifier)) continue;
				const target = ts.resolveModuleName(statement.moduleSpecifier.text, file.fileName, source.program.getCompilerOptions(), ts.sys).resolvedModule?.resolvedFileName;
				if (target && modules.has(target)) { modules.add(file.fileName); grown = true; }
			}
		}
	}
	const callers: NamedFunction[] = [];
	const handoffs: Located[] = [];
	let importers = 0;
	let failure: string | undefined;
	for (const file of files) source.walk(file, node => {
		const specifier = ts.isImportDeclaration(node) || ts.isExportDeclaration(node) ? node.moduleSpecifier
			: ts.isImportEqualsDeclaration(node) && ts.isExternalModuleReference(node.moduleReference) ? node.moduleReference.expression
				: ts.isCallExpression(node) && (node.expression.kind === ts.SyntaxKind.ImportKeyword || ts.isIdentifier(node.expression) && node.expression.text === "require") ? node.arguments[0] : undefined;
		if (failure || !specifier || !ts.isStringLiteralLike(specifier)) return;
		const target = ts.resolveModuleName(specifier.text, file.fileName, source.program.getCompilerOptions(), ts.sys).resolvedModule?.resolvedFileName;
		if (!target || !modules.has(target)) return;
		importers++;
		const carry = carryOf(node, name);
		if (typeof carry === "string") { failure = `${source.relative(file)}:${source.origin(node).line} reaches the dispatcher module through a ${carry}`; return; }
		if (carry.escapes) handoffs.push(source.origin(node));
		const useOf = (reference: ts.Identifier, callee: ts.Node) => {
			const call = callee.parent;
			const caller = ts.isCallExpression(call) && call.expression === callee ? enclosingFunction(call) : undefined;
			const reached = caller && dispatchers(source, files, caller, new Set());
			if (reached) callers.push(...reached);
			else failure = `${source.relative(file)}:${source.origin(reference).line} uses the imported dispatcher "${name}" outside a named-caller chain ending in an exported function`;
		};
		const references = (binding: ts.Identifier, visit: (reference: ts.Identifier) => void) => {
			const symbol = source.checker.getSymbolAtLocation(binding);
			source.walk(file, use => {
				if (!failure && use !== binding && ts.isIdentifier(use) && use.text === binding.text && source.checker.getSymbolAtLocation(use) === symbol) visit(use);
			});
		};
		for (const binding of carry.names) references(binding, reference => useOf(reference, reference));
		for (const access of carry.members) useOf(access.name, access);
		for (const binding of carry.namespaces) references(binding, reference => {
			const access = reference.parent;
			if (ts.isTypeQueryNode(access)) return;
			if (!ts.isPropertyAccessExpression(access) || access.expression !== reference) failure = `${source.relative(file)}:${source.origin(reference).line} passes the namespace of the dispatcher module on untraced`;
			else if (access.name.text === name) useOf(reference, access);
		});
	});
	return failure ?? { callers, importers, handoffs };
}

export function registryDispatchProof(source: SourceIndex, closure: ReadonlySet<string>): RegistryDispatch {
	const files = source.files.filter(file => closure.has(source.relative(file)));
	const invokers: Array<Located & { function: string; node: NamedFunction | undefined }> = [];
	const selfSpawnSites: RegistryDispatchProof["selfSpawnSites"] = [];
	for (const file of files) source.walk(file, node => {
		if (!ts.isCallExpression(node)) return;
		if (ts.isPropertyAccessExpression(node.expression) && node.expression.name.text === "load" && !node.arguments.length) {
			const receiver = source.checker.getTypeAtLocation(node.expression.expression);
			// A class constructor also has `load` and `name`; its `name` comes from lib.d.ts, not from a declared entry shape.
			const declaredName = receiver.getProperty("name")?.declarations?.some(declaration => !source.program.isSourceFileDefaultLibrary(declaration.getSourceFile()));
			if (receiver.flags & ts.TypeFlags.Any || receiver.getProperty("load") && declaredName) {
				const holder = enclosingFunction(node);
				invokers.push({ ...source.origin(node), function: holder?.name?.getText() ?? "", node: holder });
			}
		}
		if (SPAWNERS.test(node.expression.getText()) && /process\.execPath|argv\[1\]/.test(node.getText())) selfSpawnSites.push({ ...source.origin(node), expression: node.getText().slice(0, 120) });
	});
	const queue: NamedFunction[] = [];
	for (const invoker of invokers) {
		const reached = invoker.node && dispatchers(source, files, invoker.node, new Set());
		if (!reached) return { unproven: `${invoker.source}:${invoker.line} calls a registry entry's load() outside a named-caller chain ending in an exported function` };
		queue.push(...reached);
	}
	const terminals = new Set<NamedFunction>();
	let importersChecked = 0;
	const namespaceHandoffs: Located[] = [];
	for (let dispatcher = queue.pop(); dispatcher; dispatcher = queue.pop()) {
		if (terminals.has(dispatcher)) continue;
		terminals.add(dispatcher);
		const up = importedCallers(source, files, dispatcher);
		if (typeof up === "string") return { unproven: up };
		importersChecked += up.importers;
		namespaceHandoffs.push(...up.handoffs);
		queue.push(...up.callers);
	}
	return { proven: {
		invokers: invokers.map(({ source: at, line, function: name }) => ({ source: at, line, function: name })),
		dispatchers: [...terminals].map(fn => ({ ...source.origin(fn), name: fn.name!.getText() })),
		importersChecked, namespaceHandoffs, selfSpawnSites,
	} };
}
