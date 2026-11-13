import ts from "typescript";
import type { ProbeLocation } from "./probe";
import type { InventoryEntry } from "./inventory-types";
import { runtimeComponentIds } from "./inventory-components";

export interface SourceInstrumentation { contents: string; visits: number; sessionBindings: number; wireBoundaries: number }
const probeName = "__roostOwnedParityProbe";
const hook = (name: string) => /^(?:render|describe|handle|onKey|onMouse|on[A-Z])/.test(name) || name === "nativeSheet";
type Callable = ts.FunctionDeclaration | ts.FunctionExpression | ts.ArrowFunction | ts.MethodDeclaration | ts.GetAccessorDeclaration | ts.SetAccessorDeclaration | ts.ConstructorDeclaration;

function nodeName(node: ts.Node | undefined): string | undefined {
  if (!node) return undefined;
  if (ts.isIdentifier(node) || ts.isPrivateIdentifier(node) || ts.isStringLiteralLike(node) || ts.isNumericLiteral(node)) return node.text;
  return undefined;
}

function callable(node: ts.Node): node is Callable {
  return ts.isFunctionDeclaration(node) || ts.isFunctionExpression(node) || ts.isArrowFunction(node)
    || ts.isMethodDeclaration(node) || ts.isGetAccessorDeclaration(node) || ts.isSetAccessorDeclaration(node)
    || ts.isConstructorDeclaration(node);
}

function withBody(node: Callable, body: ts.Block): Callable {
  const factory = ts.factory;
  if (ts.isFunctionDeclaration(node)) return factory.updateFunctionDeclaration(node, node.modifiers, node.asteriskToken, node.name, node.typeParameters, node.parameters, node.type, body);
  if (ts.isFunctionExpression(node)) return factory.updateFunctionExpression(node, node.modifiers, node.asteriskToken, node.name, node.typeParameters, node.parameters, node.type, body);
  if (ts.isArrowFunction(node)) return factory.updateArrowFunction(node, node.modifiers, node.typeParameters, node.parameters, node.type, node.equalsGreaterThanToken, body);
  if (ts.isMethodDeclaration(node)) return factory.updateMethodDeclaration(node, node.modifiers, node.asteriskToken, node.name, node.questionToken, node.typeParameters, node.parameters, node.type, body);
  if (ts.isGetAccessorDeclaration(node)) return factory.updateGetAccessorDeclaration(node, node.modifiers, node.name, node.parameters, node.type, body);
  if (ts.isSetAccessorDeclaration(node)) return factory.updateSetAccessorDeclaration(node, node.modifiers, node.name, node.parameters, body);
  return factory.updateConstructorDeclaration(node, node.modifiers, node.parameters, body);
}

interface SourceIndex {
  implementationLines: Set<number>;
  nativeImplementationLines: Set<number>;
  boundaries: Map<number, Set<string>>;
  featureExpressions: Set<string>;
  featureOffsets: Set<number>;
}
const sourceIndexes = new WeakMap<readonly InventoryEntry[], Map<string, SourceIndex>>();

/** Inventory provenance grouped by source file once per inventory, not rescanned for every loaded module. */
function sourceIndex(entries: readonly InventoryEntry[], source: string): SourceIndex {
  let bySource = sourceIndexes.get(entries);
  if (!bySource) {
    bySource = indexInventory(entries);
    sourceIndexes.set(entries, bySource);
  }
  return bySource.get(source) ?? { implementationLines: new Set(), nativeImplementationLines: new Set(), boundaries: new Map(), featureExpressions: new Set(), featureOffsets: new Set() };
}

function indexInventory(entries: readonly InventoryEntry[]): Map<string, SourceIndex> {
  const bySource = new Map<string, SourceIndex>();
  const forSource = (source: string): SourceIndex => {
    let index = bySource.get(source);
    if (!index) {
      index = { implementationLines: new Set(), nativeImplementationLines: new Set(), boundaries: new Map(), featureExpressions: new Set(), featureOffsets: new Set() };
      bySource.set(source, index);
    }
    return index;
  };
  const retainProvenance = (value: unknown): void => {
    if (!value || typeof value !== "object") return;
    const fields = value as Record<string, unknown>;
    if (typeof fields.source === "string" && Number.isInteger(fields.line)) forSource(fields.source).implementationLines.add(fields.line as number);
    for (const child of Object.values(fields)) retainProvenance(child);
  };
  for (const entry of entries) {
    retainProvenance(entry.details);
    if (entry.category === "component" && Array.isArray(entry.details.hooks)) {
      for (const implementation of entry.details.hooks) if (/^(?:describe|nativeSheet)/.test(implementation?.name ?? "")
        && typeof implementation.implementation?.source === "string") forSource(implementation.implementation.source).nativeImplementationLines.add(implementation.implementation.line);
    }
    for (const key of ["nativeOwners", "terminalOwners", "delegatedHelpers"]) {
      const uses = entry.details[key];
      if (!Array.isArray(uses)) continue;
      for (const use of uses) if (typeof use?.source === "string" && Number.isInteger(use.line) && typeof use.expression === "string") {
        const boundaries = forSource(use.source).boundaries;
        const expressions = boundaries.get(use.line) ?? new Set<string>();
        expressions.add(use.expression);
        boundaries.set(use.line, expressions);
      }
    }
    if (entry.category === "feature" && Array.isArray(entry.details.checks)) {
      for (const check of entry.details.checks) if (typeof check?.source === "string" && typeof check.expression === "string") forSource(check.source).featureExpressions.add(check.expression);
    }
    if (entry.category === "feature-check") forSource(entry.source).featureOffsets.add(Number(entry.id.slice(entry.id.lastIndexOf(":") + 1)));
  }
  return bySource;
}

/** Adds observations inside existing bodies; no handler, descriptor or constructor is replaced. */
export function instrumentSource(text: string, source: string, probeModuleUrl: string, entries: readonly InventoryEntry[]): SourceInstrumentation {
  const file = ts.createSourceFile(source, text, ts.ScriptTarget.Latest, true, source.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS);
  // The parser's own syntax diagnostics; a second transpile per loaded module doubled CLI startup.
  const parsed = "parseDiagnostics" in file ? file.parseDiagnostics : undefined;
  if (!Array.isArray(parsed)) throw new Error("TypeScript no longer exposes source-file parse diagnostics");
  if (parsed.length) throw new Error(`Cannot instrument malformed canonical source ${source}`);
  if (text.includes(probeName)) throw new Error(`Canonical source collides with parity instrumentation binding: ${source}`);
  const componentIds = runtimeComponentIds(file, source, entries);
  const index = sourceIndex(entries, source);
  const { implementationLines, nativeImplementationLines, boundaries, featureExpressions, featureOffsets } = index;
  const factory = ts.factory;
  let visits = 0;
  let sessionBindings = 0;
  let wireBoundaries = 0;
  const line = (node: ts.Node) => file.getLineAndCharacterOfPosition(node.getStart(file)).line + 1;
  const endLine = (node: ts.Node) => file.getLineAndCharacterOfPosition(node.getEnd()).line + 1;
  const owner = (node: ts.Node): ts.Node | undefined => {
    for (let current = node.parent; current && !ts.isSourceFile(current); current = current.parent) {
      if (ts.isClassLike(current) || ts.isObjectLiteralExpression(current)) return current;
    }
    return undefined;
  };
  const metadata = (node: ts.Node, method?: string): ProbeLocation => {
    const enclosing = owner(node);
    const identity = enclosing && componentIds.get(enclosing);
    const name = enclosing && ts.isClassLike(enclosing) && enclosing.name ? enclosing.name.text : `callable@${line(node)}`;
    return {
      source, line: line(node), endLine: endLine(node), component: identity ? identity.slice("component:".length) : `${source}#${name}`,
      method: method ?? nodeName((node as ts.NamedDeclaration).name) ?? `anonymous@${line(node)}`,
      ...(enclosing ? { ownerLine: line(enclosing), ownerEndLine: endLine(enclosing) } : {}),
    };
  };
  const literal = (value: string | number) => typeof value === "number" ? factory.createNumericLiteral(value) : factory.createStringLiteral(value);
  const origin = (location: ProbeLocation) => factory.createObjectLiteralExpression(Object.entries(location).map(([key, value]) => factory.createPropertyAssignment(key, literal(value))), false);
  const call = (method: string, args: ts.Expression[]) => factory.createCallExpression(factory.createPropertyAccessExpression(factory.createIdentifier(probeName), method), undefined, args);
  const statement = (method: string, args: ts.Expression[]) => factory.createExpressionStatement(call(method, args));
  const field = (name: string) => factory.createPropertyAccessExpression(factory.createThis(), name.startsWith("#") ? factory.createPrivateIdentifier(name) : name);
  const state = (fields: Record<string, ts.Expression>) => factory.createArrowFunction(undefined, undefined, [], undefined,
    factory.createToken(ts.SyntaxKind.EqualsGreaterThanToken), factory.createObjectLiteralExpression(Object.entries(fields).map(([key, value]) => factory.createPropertyAssignment(key, value)), false));
  const isRegistry = source.includes("/slash-commands/") || source.includes("/overlays/settings") || source.includes("/setup/");
  const transformed = ts.transform(file, [context => {
    const visitor: ts.Visitor = node => {
      if (ts.isClassDeclaration(node) || ts.isClassExpression(node)) {
        const visited = ts.visitEachChild(node, visitor, context);
        const identity = componentIds.get(node);
        if (!identity) return visited;
        const observe = factory.createClassStaticBlockDeclaration(factory.createBlock([
          statement("observeComponentClass", [factory.createThis(), factory.createStringLiteral(identity.slice("component:".length))]),
        ], true));
        const members = [...visited.members, observe];
        return ts.isClassDeclaration(visited)
          ? factory.updateClassDeclaration(visited, visited.modifiers, visited.name, visited.typeParameters, visited.heritageClauses, members)
          : factory.updateClassExpression(visited, visited.modifiers, visited.name, visited.typeParameters, visited.heritageClauses, members);
      }
      if (ts.isObjectLiteralExpression(node) && componentIds.has(node)) {
        return call("observeComponentObject", [ts.visitEachChild(node, visitor, context), factory.createStringLiteral(componentIds.get(node)!.slice("component:".length))]);
      }
      if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)
        && (node.expression.name.text === "feature" || featureExpressions.has(node.getText(file)) || featureOffsets.has(node.getStart(file))) && node.arguments.length > 0) {
        const original = node.arguments[0]!;
        return factory.updateCallExpression(node, ts.visitNode(node.expression, visitor) as ts.Expression, node.typeArguments,
          [call("featureArgument", [ts.visitNode(original, visitor) as ts.Expression, origin(metadata(node, "feature"))]),
            ...node.arguments.slice(1).map(argument => ts.visitNode(argument, visitor) as ts.Expression)]);
      }
      if (ts.isExpressionStatement(node) && source === "packages/coding-agent/src/sdk.ts"
        && ts.isBinaryExpression(node.expression) && node.expression.operatorToken.kind === ts.SyntaxKind.EqualsToken
        && ts.isNewExpression(node.expression.right) && node.expression.right.expression.getText(file) === "AgentSession") {
        if (!ts.isIdentifier(node.expression.left)) throw new Error("Actual SDK AgentSession construction lost its stable local binding.");
        sessionBindings++;
        return [ts.visitEachChild(node, visitor, context), statement("observeSession", [factory.createIdentifier(node.expression.left.text), origin(metadata(node, "constructed"))])];
      }
      if (source === "packages/tui/src/native/backend.ts" && ts.isBinaryExpression(node)
        && node.operatorToken.kind === ts.SyntaxKind.EqualsToken && node.left.getText(file) === "this.#mirror") {
        return factory.updateBinaryExpression(node, node.left, node.operatorToken, call("enableNativeMirror", [
          factory.createThis(), factory.createPropertyAccessExpression(factory.createIdentifier("options"), "mirror"), origin(metadata(node, "constructor:mirror")),
        ]));
      }
      if (source === "packages/tui/src/overlays/settings-selector.ts" && ts.isCallExpression(node) && node.expression.getText(file) === "def.condition") {
        const original = ts.visitEachChild(node, visitor, context);
        return call("observeCondition", [original, factory.createPropertyAccessExpression(factory.createIdentifier("def"), "path"), origin(metadata(node, "condition"))]);
      }
      if (ts.isCallExpression(node) || ts.isPropertyAccessExpression(node)) {
        const expression = ts.isCallExpression(node) ? node.expression.getText(file) : node.getText(file);
        const parent = node.parent;
        const written = ts.isDeleteExpression(parent) || ts.isPostfixUnaryExpression(parent) || ts.isPrefixUnaryExpression(parent)
          || ts.isBinaryExpression(parent) && parent.left === node
            && parent.operatorToken.kind >= ts.SyntaxKind.FirstAssignment && parent.operatorToken.kind <= ts.SyntaxKind.LastAssignment
          || ts.isCallExpression(parent) && parent.expression === node
          || ts.isNewExpression(parent) && parent.expression === node;
        if (!written && !ts.isCallChain(node) && !ts.isPropertyAccessChain(node) && boundaries.get(line(node))?.has(expression)) {
          visits++;
          let scope: ts.Node | undefined = node.parent;
          while (scope && (!callable(scope) || ts.isArrowFunction(scope))) scope = scope.parent;
          const receiver = scope && callable(scope) && !ts.isConstructorDeclaration(scope) ? factory.createThis() : factory.createVoidZero();
          return call("observedValue", [ts.visitEachChild(node, visitor, context) as ts.Expression, origin({ ...metadata(node, "access"), expression }), receiver]);
        }
      }
      if (!callable(node) || !node.body) return ts.visitEachChild(node, visitor, context);
      const parent = node.parent;
      const named = nodeName((node as ts.NamedDeclaration).name) ?? (ts.isPropertyAssignment(parent) || ts.isPropertyDeclaration(parent) || ts.isVariableDeclaration(parent) ? nodeName(parent.name) : undefined);
      const constructor = ts.isConstructorDeclaration(node);
      const enclosing = owner(node);
      const className = enclosing && ts.isClassLike(enclosing) ? enclosing.name?.text : undefined;
      const location = metadata(node, constructor ? "constructor" : named);
      const receiver = !ts.isArrowFunction(node) || ts.isPropertyDeclaration(parent) ? factory.createThis() : factory.createVoidZero();
      let visited = ts.visitEachChild(node, visitor, context) as Callable;
      const leading: ts.Statement[] = [];
      const trailing: ts.Statement[] = [];
      if (!constructor && (named && (hook(named) || isRegistry) || implementationLines.has(line(node)))) {
        visits++;
        const firstParameter = node.parameters[0]?.name;
        const firstArgument = isRegistry && firstParameter && ts.isIdentifier(firstParameter) ? factory.createIdentifier(firstParameter.text) : factory.createVoidZero();
        leading.push(statement("visit", [origin(location), receiver, firstArgument]));
      }
      if (constructor && source === "packages/coding-agent/src/session/agent-session.ts" && className === "AgentSession") {
        trailing.push(statement("observeToolSession", [factory.createThis(), field("#evalToolSession"), origin(location)]));
      }
      if (constructor && source === "packages/coding-agent/src/modes/interactive-mode.ts" && className === "InteractiveMode") {
        trailing.push(statement("observeInteractiveMode", [factory.createThis(), state({
          paused: factory.createPropertyAccessExpression(factory.createIdentifier("agentPauseGate"), "paused"),
          loopEnabled: field("loopModeEnabled"), todoExpanded: field("todoExpanded"),
        }), origin(location)]));
      }
      if (constructor && source === "packages/coding-agent/src/modes/controllers/btw-controller.ts" && className === "BtwController") {
        trailing.push(statement("observeBtwController", [factory.createThis(), origin(location)]));
      }
      if (constructor && source === "packages/coding-agent/src/stream/recording.ts" && className === "SessionRecorder") {
        trailing.push(statement("observeRecorder", [factory.createThis(), state({
          active: factory.createBinaryExpression(field("#unsubscribe"), ts.SyntaxKind.ExclamationEqualsEqualsToken, factory.createIdentifier("undefined")),
          lastPaintAt: field("#lastPaintAt"), path: field("path"),
        }), origin(location)]));
      }
      if (source === "packages/coding-agent/src/slash-commands/builtin-marketplace.ts" && named === "reloadTuiPluginState") {
        trailing.push(statement("completedPluginReload", [origin(location)]));
      }
      if (source === "packages/coding-agent/src/session/agent-session.ts" && named === "setForcedToolChoice") {
        trailing.push(statement("completedForcedToolChoice", [factory.createThis(), factory.createIdentifier("toolName"), origin(location)]));
      }
      if (source === "packages/coding-agent/src/config/settings.ts" && named === "writeValue" && className === "Settings") {
        trailing.push(statement("completedSettingWrite", [
          factory.createPropertyAccessExpression(factory.createIdentifier("setting"), "id"), factory.createIdentifier("layer"), origin(location),
        ]));
      }
      if (source === "packages/tui/src/native/backend.ts" && named === "#sendFrame") {
        trailing.push(statement("nativeDocument", [
          factory.createThis(), factory.createPropertyAccessExpression(factory.createIdentifier("surface"), "id"),
          factory.createPropertyAccessExpression(factory.createIdentifier("surface"), "seq"),
          factory.createCallChain(factory.createPropertyAccessChain(
            factory.createPropertyAccessExpression(factory.createIdentifier("surface"), "doc"),
            factory.createToken(ts.SyntaxKind.QuestionDotToken), "snapshot"), undefined, undefined, []),
          origin(location),
        ]));
      }
      if (source === "packages/tui/src/native/backend.ts" && named === "#send") {
        wireBoundaries++;
        leading.push(statement("wire", [factory.createStringLiteral("out"), factory.createIdentifier("message"), origin(location)]));
      }
      if (source === "packages/tui/src/native/encode.ts" && named === "encodeTspJson") {
        wireBoundaries++;
        leading.push(statement("wire", [factory.createStringLiteral("out"), factory.createObjectLiteralExpression([
          factory.createShorthandPropertyAssignment("verb"), factory.createPropertyAssignment("body", factory.createIdentifier("value")),
        ]), origin(location)]));
      }
      if (source === "packages/tui/src/native/encode.ts" && named === "decodeTspBody") {
        wireBoundaries++;
        const observeReturns: ts.Visitor = child => {
          if (child !== visited && callable(child)) return child;
          if (ts.isReturnStatement(child) && child.expression) return factory.updateReturnStatement(child, call("decodedMessage", [child.expression, origin(location)]));
          return ts.visitEachChild(child, observeReturns, context);
        };
        visited = ts.visitEachChild(visited, observeReturns, context) as Callable;
      }
      if (source === "packages/tui/src/native/backend.ts" && className === "NativeBackend"
        && (named === "handleEvent" || named === "confirm" || named === "resume" || constructor)) {
        wireBoundaries++;
        const event = named === "handleEvent";
        const incoming = factory.createObjectLiteralExpression([
          factory.createPropertyAssignment("verb", factory.createStringLiteral(event ? "e" : "r")),
          factory.createPropertyAssignment("body", factory.createIdentifier(event ? "event" : "hello")),
        ]);
        const observed = statement("wire", [factory.createStringLiteral("in"), incoming, origin(location)]);
        const live = factory.createPropertyAccessExpression(factory.createThis(), factory.createPrivateIdentifier("#live"));
        // handleEvent returns early while not live and resume while live; only an input the backend actually accepts is an observation.
        if (event) leading.push(factory.createIfStatement(live, observed));
        else if (named === "resume") leading.push(factory.createIfStatement(factory.createPrefixUnaryExpression(ts.SyntaxKind.ExclamationToken, live), observed));
        else leading.push(observed);
      }
      if (source === "packages/tui/src/native/backend.ts" && named === "#handleEvent") {
        const completed = (value: ts.Expression) => call("completedNativeEvent", [value, factory.createIdentifier("event"), origin(location)]);
        const observeReturns: ts.Visitor = child => {
          if (child !== visited && callable(child)) return child;
          if (ts.isReturnStatement(child)) return factory.updateReturnStatement(child, completed(child.expression ?? factory.createVoidZero()));
          return ts.visitEachChild(child, observeReturns, context);
        };
        visited = ts.visitEachChild(visited, observeReturns, context) as Callable;
        trailing.push(factory.createExpressionStatement(completed(factory.createVoidZero())));
      }
      if (source === "packages/tui/src/native/reconcile.ts" && named === "#resolveRefs") {
        const observeReturns: ts.Visitor = child => {
          if (child !== visited && callable(child)) return child;
          if (ts.isReturnStatement(child) && child.expression && ts.isObjectLiteralExpression(child.expression)
            && child.expression.properties.some(property => ts.isPropertyAssignment(property) && nodeName(property.name) === "selected" && property.initializer.getText(file) === "item.id")) {
            return factory.updateReturnStatement(child, call("completedNativeReference", [child.expression, factory.createIdentifier("selected"),
              factory.createPropertyAccessExpression(factory.createIdentifier("item"), "id"), origin(metadata(child, "reference:selected"))]));
          }
          return ts.visitEachChild(child, observeReturns, context);
        };
        visited = ts.visitEachChild(visited, observeReturns, context) as Callable;
      }
      if (/^(?:describe|nativeSheet)/.test(named ?? "") || nativeImplementationLines.has(line(node))) {
        const returned = (value: ts.Expression, original: ts.Node) => call("returnedValue", [value, origin(metadata(original, `return:${named ?? "native"}`)), receiver]);
        const observeReturns: ts.Visitor = child => {
          if (child !== visited && callable(child)) return child;
          if (ts.isReturnStatement(child)) return factory.updateReturnStatement(child, returned(child.expression ?? factory.createVoidZero(), child));
          return ts.visitEachChild(child, observeReturns, context);
        };
        if (ts.isBlock(visited.body!)) visited = ts.visitEachChild(visited, observeReturns, context) as Callable;
        else visited = withBody(visited, factory.createBlock([factory.createReturnStatement(returned(visited.body! as ts.Expression, node.body!))], true));
      }
      if (leading.length === 0 && trailing.length === 0) return visited;
      const body = visited.body!;
      const statements = ts.isBlock(body) ? [...body.statements] : [factory.createReturnStatement(body)];
      let directives = 0;
      while (directives < statements.length && ts.isExpressionStatement(statements[directives]!)
        && ts.isStringLiteral((statements[directives] as ts.ExpressionStatement).expression)) directives++;
      return withBody(visited, factory.createBlock([...statements.slice(0, directives), ...leading, ...statements.slice(directives), ...trailing], true));
    };
    return node => ts.visitNode(node, visitor) as ts.SourceFile;
  }]);
  try {
    if (source === "packages/coding-agent/src/sdk.ts" && sessionBindings !== 1) {
      throw new Error(`Expected exactly one actual SDK AgentSession construction; found ${sessionBindings}.`);
    }
    const result = transformed.transformed[0]!;
    const imported = factory.createImportDeclaration(undefined, factory.createImportClause(false, undefined,
      factory.createNamedImports([factory.createImportSpecifier(false, factory.createIdentifier("probe"), factory.createIdentifier(probeName))])),
      factory.createStringLiteral(probeModuleUrl));
    const output = factory.updateSourceFile(result, [imported, ...result.statements]);
    return { contents: ts.createPrinter({ newLine: ts.NewLineKind.LineFeed }).printFile(output), visits, sessionBindings, wireBoundaries };
  } finally {
    transformed.dispose();
  }
}
