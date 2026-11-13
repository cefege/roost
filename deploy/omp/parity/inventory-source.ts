import { existsSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";

export interface Origin { source: string; line: number }
export interface ExpressionValue extends Origin { expression: string; reason: string }
interface Callable { node: ts.FunctionLikeDeclaration; environment: Environment }
type Environment = Map<ts.Symbol | string, unknown>;
const callable = (value: unknown): value is Callable => !!value && typeof value === "object" && "environment" in value && "node" in value;
export const record = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value) && !callable(value) && !("reason" in value && "expression" in value);
export const symbolic = (value: unknown): value is ExpressionValue => !!value && typeof value === "object" && "reason" in value && "expression" in value;

export function sourcePaths(directory: string): string[] {
	if (!existsSync(directory)) return [];
	return readdirSync(directory, { withFileTypes: true }).sort((left, right) => left.name.localeCompare(right.name)).flatMap(entry => {
		const filename = path.join(directory, entry.name);
		return entry.isDirectory() ? sourcePaths(filename) : /\.(?:ts|tsx)$/.test(entry.name) && !/\.(?:d|test|spec)\.tsx?$/.test(entry.name) ? [filename] : [];
	});
}

/** Parse source and resolve lexical/import bindings without importing OMP or reading its profile. */
export class SourceIndex {
	readonly program: ts.Program;
	readonly checker: ts.TypeChecker;
	readonly files: ts.SourceFile[];
	readonly origins = new WeakMap<object, ts.Node>();
	readonly fields = new WeakMap<object, Map<string, ts.Node>>();
	private readonly evaluating = new Set<ts.Node>();
	private readonly cache = new Map<ts.Node, unknown>();
	constructor(readonly root: string) {
		const paths: Record<string, string[]> = {};
		for (const entry of readdirSync(path.join(root, "packages"), { withFileTypes: true })) {
			const manifest = path.join(root, "packages", entry.name, "package.json");
			if (!entry.isDirectory() || !existsSync(manifest)) continue;
			const pkg = JSON.parse(readFileSync(manifest, "utf8"));
			if (typeof pkg.name !== "string") continue;
			paths[pkg.name] = [path.join(root, "packages", entry.name, pkg.types ?? pkg.main ?? "src/index.ts")];
			paths[`${pkg.name}/*`] = [path.join(root, "packages", entry.name, "src/*")];
		}
		const roots = ["tui", "coding-agent", "wire"].flatMap(name => sourcePaths(path.join(root, "packages", name, "src")));
		this.program = ts.createProgram(roots, { target: ts.ScriptTarget.ESNext, module: ts.ModuleKind.ESNext, moduleResolution: ts.ModuleResolutionKind.Bundler, baseUrl: root, paths, skipLibCheck: true, noEmit: true, strictNullChecks: true, allowImportingTsExtensions: true });
		this.checker = this.program.getTypeChecker();
		this.files = this.program.getSourceFiles().filter(file => this.relative(file).startsWith("packages/") && !this.relative(file).includes("/node_modules/") && !file.isDeclarationFile);
	}
	relative(node: ts.Node): string { return path.relative(this.root, node.getSourceFile().fileName).split(path.sep).join("/"); }
	origin(node: ts.Node): Origin { const file = node.getSourceFile(); return { source: this.relative(node), line: file.getLineAndCharacterOfPosition(node.getStart(file)).line + 1 }; }
	entry(id: string, category: string, node: ts.Node, details: Record<string, unknown> = {}): InventoryEntry { return { id, category, ...this.origin(node), details }; }
	unknown(node: ts.Node, reason = "requires runtime state"): ExpressionValue { return { ...this.origin(node), expression: node.getText(), reason }; }
	file(relative: string): ts.SourceFile {
		const file = this.program.getSourceFile(path.join(this.root, relative));
		if (!file) throw new Error(`Missing canonical source: ${relative}`);
		return file;
	}
	walk(node: ts.Node, visit: (node: ts.Node) => void): void { visit(node); node.forEachChild(child => this.walk(child, visit)); }
	symbol(node: ts.Node): ts.Symbol | undefined {
		let symbol = this.checker.getSymbolAtLocation(node);
		if (symbol?.flags && symbol.flags & ts.SymbolFlags.Alias) symbol = this.checker.getAliasedSymbol(symbol);
		return symbol;
	}
	declaration(node: ts.Node): ts.Declaration | undefined {
		const symbol = this.symbol(node);
		return symbol?.declarations?.find(declaration => ts.isFunctionDeclaration(declaration) && declaration.body !== undefined) ?? symbol?.valueDeclaration ?? symbol?.declarations?.[0];
	}
	declarations(file: ts.SourceFile, name: string): ts.Node[] {
		const found: ts.Node[] = [];
		this.walk(file, node => { if ((ts.isVariableDeclaration(node) || ts.isFunctionDeclaration(node) || ts.isClassDeclaration(node) || ts.isInterfaceDeclaration(node) || ts.isTypeAliasDeclaration(node)) && node.name?.getText() === name) found.push(node); });
		return found;
	}
	named(file: string, name: string): ts.Node {
		const declarations = this.declarations(this.file(file), name);
		if (declarations.length !== 1) throw new Error(`Expected one declaration of ${name} in ${file}; found ${declarations.length}`);
		return declarations[0]!;
	}
	valueNamed(file: string, name: string): unknown { return this.value(this.named(file, name)); }
	invokeSource(node: ts.FunctionLikeDeclaration, args: unknown[], bindings: Environment = new Map()): unknown {
		return this.invoke({ node, environment: bindings } satisfies Callable, args);
	}
	name(node: ts.PropertyName | ts.BindingName | undefined, environment: Environment = new Map()): string | undefined {
		if (!node) return undefined;
		if (ts.isIdentifier(node) || ts.isPrivateIdentifier(node) || ts.isStringLiteralLike(node) || ts.isNumericLiteral(node)) return node.text;
		if (ts.isComputedPropertyName(node)) { const value = this.value(node.expression, environment); if (["string", "number"].includes(typeof value)) return String(value); }
		return undefined;
	}
	serialize(value: unknown): unknown {
		if (callable(value)) return { ...this.origin(value.node), expression: value.node.name?.getText() ?? "callback", reason: "controller-owned function", parameters: value.node.parameters.map(parameter => parameter.name.getText()) };
		if (Array.isArray(value)) return value.map(item => this.serialize(item));
		if (record(value)) return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, this.serialize(item)]));
		return value;
	}
	value(node: ts.Node | undefined, environment: Environment = new Map()): unknown {
		if (!node) return undefined;
		if (environment.size === 0 && this.cache.has(node)) return this.cache.get(node);
		if (this.evaluating.has(node)) return this.unknown(node, "cyclic source reference");
		this.evaluating.add(node);
		let result: unknown;
		try { result = this.evaluate(node, environment); } finally { this.evaluating.delete(node); }
		if (result && typeof result === "object" && !this.origins.has(result)) this.origins.set(result, node);
		if (environment.size === 0) this.cache.set(node, result);
		return result;
	}
	private evaluate(node: ts.Node, environment: Environment): unknown {
		if (ts.isVariableDeclaration(node) || ts.isPropertyAssignment(node) || ts.isEnumMember(node) || ts.isPropertyDeclaration(node)) return this.value(node.initializer, environment);
		if (ts.isAsExpression(node) || ts.isSatisfiesExpression(node) || ts.isParenthesizedExpression(node) || ts.isTypeAssertionExpression(node) || ts.isNonNullExpression(node)) return this.value(node.expression, environment);
		if (ts.isStringLiteralLike(node)) return node.text;
		if (ts.isNumericLiteral(node)) return Number(node.text);
		if (node.kind === ts.SyntaxKind.TrueKeyword) return true;
		if (node.kind === ts.SyntaxKind.FalseKeyword) return false;
		if (node.kind === ts.SyntaxKind.NullKeyword) return null;
		if (node.kind === ts.SyntaxKind.ThisKeyword) return environment.get("this") ?? this.unknown(node, "unbound source instance");
		if (ts.isTypeOfExpression(node)) {
			const value = this.value(node.expression, environment);
			return symbolic(value) ? this.unknown(node) : callable(value) ? "function" : typeof value;
		}
		if (ts.isIdentifier(node)) {
			const symbol = this.symbol(node);
			if (symbol && environment.has(symbol)) return environment.get(symbol);
			if (environment.has(node.text)) return environment.get(node.text);
			if (node.text === "undefined") return undefined;
			const declaration = this.declaration(node);
			return declaration ? this.value(declaration, environment) : this.unknown(node, "unresolved identifier");
		}
		if (ts.isShorthandPropertyAssignment(node)) {
			const symbol = this.checker.getShorthandAssignmentValueSymbol(node);
			if (symbol && environment.has(symbol)) return environment.get(symbol);
			if (environment.has(node.name.text)) return environment.get(node.name.text);
			return symbol?.valueDeclaration ? this.value(symbol.valueDeclaration, environment) : this.value(node.name, environment);
		}
		if (ts.isFunctionDeclaration(node) || ts.isArrowFunction(node) || ts.isFunctionExpression(node) || ts.isMethodDeclaration(node) || ts.isGetAccessorDeclaration(node)) return { node, environment: new Map(environment) } satisfies Callable;
		if (ts.isArrayLiteralExpression(node)) {
			const values: unknown[] = [];
			for (const element of node.elements) {
				if (ts.isSpreadElement(element)) { const value = this.value(element.expression, environment); values.push(...(Array.isArray(value) ? value : [this.unknown(element, "unresolved array spread")])); }
				else values.push(this.value(element, environment));
			}
			return values;
		}
		if (ts.isObjectLiteralExpression(node)) {
			const value: Record<string, unknown> = {};
			const fields = new Map<string, ts.Node>();
			for (const property of node.properties) {
				if (ts.isSpreadAssignment(property)) {
					const spread = this.value(property.expression, environment);
					if (record(spread)) { Object.assign(value, spread); for (const [key, origin] of this.fields.get(spread) ?? []) fields.set(key, origin); }
					else value[`$spread:${property.pos}`] = this.unknown(property, "unresolved object spread");
					continue;
				}
				const name = this.name(property.name, environment);
				if (name === undefined) { value[`$key:${property.pos}`] = this.unknown(property, "unresolved computed property"); continue; }
				value[name] = this.value(property, environment); fields.set(name, property);
			}
			this.fields.set(value, fields);
			return value;
		}
		if (ts.isPropertyAccessExpression(node) || ts.isElementAccessExpression(node)) {
			const object = this.value(node.expression, environment);
			const key = ts.isPropertyAccessExpression(node) ? node.name.text : this.value(node.argumentExpression, environment);
			if ((record(object) || Array.isArray(object)) && (typeof key === "string" || typeof key === "number")) return (object as Record<string, unknown>)[String(key)];
			const declaration = this.declaration(ts.isPropertyAccessExpression(node) ? node.name : node);
			return declaration ? this.value(declaration, environment) : this.unknown(node);
		}
		if (ts.isTemplateExpression(node)) {
			let text = node.head.text;
			for (const span of node.templateSpans) { const value = this.value(span.expression, environment); if (typeof value === "object" || callable(value)) return this.unknown(node); text += String(value) + span.literal.text; }
			return text;
		}
		if (ts.isConditionalExpression(node)) {
			const test = this.value(node.condition, environment);
			return symbolic(test) ? this.unknown(node) : this.value(test ? node.whenTrue : node.whenFalse, environment);
		}
		if (ts.isPrefixUnaryExpression(node)) {
			const value = this.value(node.operand, environment);
			if (symbolic(value)) return this.unknown(node);
			if (node.operator === ts.SyntaxKind.ExclamationToken) return !value;
			if (typeof value === "number" && node.operator === ts.SyntaxKind.MinusToken) return -value;
		}
		if (ts.isBinaryExpression(node)) {
			const left = this.value(node.left, environment);
			const right = this.value(node.right, environment);
			if (symbolic(left) || symbolic(right)) return this.unknown(node);
			switch (node.operatorToken.kind) {
				case ts.SyntaxKind.PlusToken: if (typeof left === "string" || typeof right === "string") return String(left) + String(right); if (typeof left === "number" && typeof right === "number") return left + right; break;
				case ts.SyntaxKind.EqualsEqualsEqualsToken: return left === right;
				case ts.SyntaxKind.ExclamationEqualsEqualsToken: return left !== right;
				case ts.SyntaxKind.AmpersandAmpersandToken: return left && right;
				case ts.SyntaxKind.BarBarToken: return left || right;
				case ts.SyntaxKind.QuestionQuestionToken: return left ?? right;
			}
		}
		if (ts.isCallExpression(node)) return this.call(node, environment);
		return this.unknown(node, `static evaluation unavailable for ${ts.SyntaxKind[node.kind]}`);
	}
	private bind(name: ts.BindingName, value: unknown, environment: Environment): void {
		if (ts.isIdentifier(name)) {
			const symbol = this.symbol(name);
			if (symbol) environment.set(symbol, value);
			environment.set(name.text, value);
			return;
		}
		for (const [index, element] of name.elements.entries()) {
			if (ts.isOmittedExpression(element)) continue;
			const key = ts.isObjectBindingPattern(name) ? this.name(element.propertyName ?? element.name, environment) : String(index);
			let member = key !== undefined && (record(value) || Array.isArray(value)) ? (value as Record<string, unknown>)[key] : this.unknown(element, "unresolved destructuring source");
			if (element.dotDotDotToken) member = this.unknown(element, "rest binding requires source evaluation");
			if (member === undefined && element.initializer) member = this.value(element.initializer, environment);
			this.bind(element.name, member, environment);
		}
	}
	private invoke(fn: unknown, args: unknown[]): unknown {
		if (!callable(fn)) return fn;
		if (!fn.node.body) return this.unknown(fn.node, "source helper has no implementation");
		const environment = new Map(fn.environment);
		fn.node.parameters.forEach((parameter, index) => {
			const value = args[index] === undefined ? this.value(parameter.initializer, environment) : args[index];
			this.bind(parameter.name, value, environment);
		});
		if (!ts.isBlock(fn.node.body)) return this.value(fn.node.body, environment);
		const evaluateStatements = (statements: readonly ts.Statement[]): { returned: boolean; value?: unknown } => {
			for (const statement of statements) {
				if (ts.isReturnStatement(statement)) return { returned: true, value: this.value(statement.expression, environment) };
				if (ts.isVariableStatement(statement)) {
					for (const declaration of statement.declarationList.declarations) {
						this.bind(declaration.name, this.value(declaration.initializer, environment), environment);
					}
					continue;
				}
				if (ts.isIfStatement(statement)) {
					const condition = this.value(statement.expression, environment);
					if (symbolic(condition)) return { returned: true, value: this.unknown(statement, "runtime branch in source helper") };
					const branch = condition ? statement.thenStatement : statement.elseStatement;
					if (branch) { const result = evaluateStatements(ts.isBlock(branch) ? branch.statements : [branch]); if (result.returned) return result; }
					continue;
				}
				return { returned: true, value: this.unknown(statement, "non-declarative source helper") };
			}
			return { returned: false };
		};
		return evaluateStatements(fn.node.body.statements).value;
	}
	private call(node: ts.CallExpression, environment: Environment): unknown {
		const args = node.arguments.map(argument => this.value(argument, environment));
		if (ts.isIdentifier(node.expression) && ["Number", "String"].includes(node.expression.text)) {
			const declaration = this.declaration(node.expression);
			if ((!declaration || declaration.getSourceFile().isDeclarationFile) && args.every(value => value === null || ["undefined", "string", "number", "boolean"].includes(typeof value))) return node.expression.text === "Number" ? args.length === 0 ? 0 : Number(args[0]) : args.length === 0 ? "" : String(args[0]);
		}
		if (ts.isIdentifier(node.expression) && node.expression.text === "structuredClone") {
			const declaration = this.declaration(node.expression);
			if (!declaration || declaration.getSourceFile().isDeclarationFile) return args[0];
		}
		if (ts.isPropertyAccessExpression(node.expression)) {
			const method = node.expression.name.text;
			const object = this.value(node.expression.expression, environment);
			if (typeof object === "string" && method === "trim") return object.trim();
			if (ts.isIdentifier(node.expression.expression) && node.expression.expression.text === "Number" && method === "isFinite") {
				const declaration = this.declaration(node.expression.expression);
				if (!declaration || declaration.getSourceFile().isDeclarationFile) return symbolic(args[0]) ? this.unknown(node) : Number.isFinite(args[0]);
			}
			if (Array.isArray(object)) {
				if (["map", "flatMap", "filter"].includes(method) && !callable(args[0])) return this.unknown(node, "unresolved array callback");
				if (method === "map") return object.map((value, index) => this.invoke(args[0], [value, index, object]));
				if (method === "flatMap") return object.flatMap((value, index) => this.invoke(args[0], [value, index, object]));
				if (method === "filter") { const selected = object.map((value, index) => this.invoke(args[0], [value, index, object])); return selected.some(symbolic) ? this.unknown(node) : object.filter((_value, index) => selected[index]); }
				if (method === "join" && object.every(value => ["string", "number"].includes(typeof value))) return object.join(typeof args[0] === "string" ? args[0] : ",");
				if (method === "concat") return object.concat(...args);
				if (method === "includes") return object.includes(args[0]);
			}
			if (node.expression.expression.getText() === "Object" && record(args[0])) {
				if (method === "keys") return Object.keys(args[0]);
				if (method === "values") return Object.values(args[0]);
				if (method === "entries") return Object.entries(args[0]);
				if (method === "freeze") return args[0];
			}
		}
		const fn = this.value(node.expression, environment);
		return callable(fn) ? this.invoke(fn, args) : this.unknown(node, "runtime call; not executed by inventory");
	}
}
