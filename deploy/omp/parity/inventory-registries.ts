import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";
import { record, SourceIndex, symbolic } from "./inventory-source";
import { settingsPresentationInventory } from "./inventory-settings";
import { subcommandInventory } from "./inventory-commands";

const COMMANDS = "packages/coding-agent/src/slash-commands/builtin-registry.ts";
const SETTINGS = "packages/coding-agent/src/config/all-settings.ts";
const SETTINGS_UI = "packages/coding-agent/src/config/settings-ui.ts";
const SETTING_REGISTRY = "packages/coding-agent/src/config/registry.ts";

function objectRegistry(source: SourceIndex, filename: string, name: string, category: string): InventoryEntry[] {
	const registry = source.valueNamed(filename, name);
	if (!record(registry)) throw new Error(`${filename}#${name} is not a statically enumerable object registry`);
	return Object.entries(registry).map(([key, value], order) => {
		if (key.startsWith("$")) throw new Error(`Unresolved ${category} registry entry: ${key}`);
		const node = source.fields.get(registry)?.get(key) ?? source.named(filename, name);
		const definition = value && typeof value === "object" ? source.origins.get(value) : undefined;
		return source.entry(`${category}:${key}`, category, node, { registry: name, order, definition: definition ? source.origin(definition) : undefined, value: source.serialize(value), obligations: symbolic(value) ? ["runtime-factory-presentation"] : [] });
	});
}

export function registryInventory(source: SourceIndex): InventoryEntry[] {
	const entries: InventoryEntry[] = [];
	const commands = source.valueNamed(COMMANDS, "BUILTIN_SLASH_COMMAND_REGISTRY");
	if (!Array.isArray(commands)) throw new Error("Builtin command registry is not an array");
	for (const [order, command] of commands.entries()) {
		if (!record(command) || typeof command.name !== "string") throw new Error(`Unenumerated builtin command at index ${order}: ${JSON.stringify(source.serialize(command))}`);
		const node = source.origins.get(command);
		if (!node) throw new Error(`Missing command provenance for ${command.name}`);
		const fields = source.fields.get(command);
		const aliases = command.aliases ?? [];
		if (!Array.isArray(aliases) || !aliases.every(alias => typeof alias === "string")) throw new Error(`Unenumerated aliases for /${command.name}`);
		const details = {
			order, definition: source.serialize(command), aliases, exposure: "ui",
			declaration: { ...source.origin(node), start: node.getStart(), end: node.getEnd() },
			handlers: Object.fromEntries(["handle", "handleTui"].map(name => [name, { available: command[name] !== undefined && command[name] !== null, declaration: fields?.get(name) ? source.origin(fields.get(name)!) : undefined, implementation: source.serialize(command[name]) }])),
			subcommands: source.serialize(command.subcommands ?? []),
			completion: { dynamicDescription: fields?.has("getTuiAutocompleteDescription") ?? false, source: COMMANDS, materializer: "materializeBuiltinSlashCommand" },
			obligations: symbolic(command.subcommands) || Array.isArray(command.subcommands) && command.subcommands.some(symbolic) ? ["runtime-command-subcommands"] : [],
		};
		entries.push(source.entry(`command:${command.name}`, "command", node, details));
		for (const alias of aliases) entries.push(source.entry(`command:${alias}`, "command", fields?.get("aliases") ?? node, { aliasOf: command.name, canonical: `command:${command.name}`, handlers: details.handlers, subcommands: details.subcommands, exposure: "ui", declaration: details.declaration }));
		entries.push(...subcommandInventory(source, command, aliases, node, details.handlers));
	}
	entries.push(...objectRegistry(source, "packages/tui/src/tools/index.ts", "toolRenderers", "tool"));
	entries.push(...objectRegistry(source, "packages/tui/src/components/composer/registry.ts", "BUILTIN_COMPOSER_STYLES", "composer"));
	entries.push(...objectRegistry(source, "packages/tui/src/status-line/segments.ts", "SEGMENTS", "segment"));
	const scenesFile = "packages/tui/src/setup/wizard.ts";
	const scenes = source.valueNamed(scenesFile, "ALL_SCENES");
	if (!Array.isArray(scenes)) throw new Error("Setup scene registry is not an array");
	for (const [order, scene] of scenes.entries()) {
		if (!record(scene) || typeof scene.id !== "string") throw new Error(`Unenumerated setup scene: ${JSON.stringify(source.serialize(scene))}`);
		const node = source.origins.get(scene);
		if (!node) throw new Error(`Missing setup scene provenance for ${scene.id}`);
		entries.push(source.entry(`scene:${scene.id}`, "scene", node, { order, definition: source.serialize(scene), obligations: scene.ansiOnly === true ? ["native-scene-unavailable"] : [] }));
	}
	return entries;
}

/** Keep registered config-only paths as well as visible rows; OMP owns visibility and ordering. */
export function settingsInventory(source: SourceIndex): InventoryEntry[] {
	const definitions = new Map<ts.Node, { node: ts.CallExpression; value: Record<string, unknown> }>();
	for (const file of source.files) source.walk(file, node => {
		if (!ts.isCallExpression(node) || !node.arguments[0]) return;
		const declaration = source.declaration(node.expression);
		if (!declaration || source.relative(declaration) !== SETTING_REGISTRY || !ts.isFunctionDeclaration(declaration) || declaration.name?.text !== "register") return;
		const value = source.value(node.arguments[0]);
		if (!record(value) || typeof value.id !== "string") throw new Error(`Unenumerated setting registration at ${JSON.stringify(source.origin(node))}`);
		definitions.set(node, { node, value });
	});
	const domains = source.named(SETTINGS, "DOMAINS");
	if (!ts.isVariableDeclaration(domains) || !domains.initializer || !ts.isArrayLiteralExpression(domains.initializer)) throw new Error("Setting domains are not a source array");
	const ordered: Array<{ node: ts.CallExpression; value: Record<string, unknown>; domain: string }> = [];
	const addDomain = (expression: ts.Expression): void => {
		const module = source.symbol(expression);
		if (!module) throw new Error(`Unresolved settings domain: ${expression.getText()}`);
		const exported = source.checker.getExportsOfModule(module);
		const declarations = new Set<ts.Node>();
		for (const symbol of exported) {
			const resolved = symbol.flags & ts.SymbolFlags.Alias ? source.checker.getAliasedSymbol(symbol) : symbol;
			for (const declaration of resolved.declarations ?? []) source.walk(declaration, node => { if (definitions.has(node)) declarations.add(node); });
		}
		const values = [...declarations].map(node => definitions.get(node)!).sort((left, right) => left.node.pos - right.node.pos);
		for (const definition of values) ordered.push({ ...definition, domain: expression.getText() });
	};
	for (const domain of domains.initializer.elements) { if (ts.isSpreadElement(domain)) throw new Error(`Unenumerated setting domain spread: ${domain.getText()}`); addDomain(domain); }
	const placed = source.named(SETTINGS, "PLACED_DOMAINS");
	if (!ts.isVariableDeclaration(placed) || !placed.initializer || !ts.isArrayLiteralExpression(placed.initializer)) throw new Error("Placed setting domains are not a source array");
	for (const element of placed.initializer.elements) {
		if (!ts.isObjectLiteralExpression(element)) throw new Error(`Unenumerated placed domain: ${element.getText()}`);
		const domain = element.properties.find(property => property.name && source.name(property.name) === "domain");
		const before = element.properties.find(property => property.name && source.name(property.name) === "before");
		if (!domain || !before || !ts.isPropertyAssignment(domain) || !ts.isPropertyAssignment(before)) throw new Error(`Invalid placed setting domain: ${element.getText()}`);
		const start = ordered.length;
		addDomain(domain.initializer);
		const additions = ordered.splice(start);
		const anchor = source.declaration(before.initializer);
		const anchorDefinitions = new Set<ts.Node>();
		if (anchor) source.walk(anchor, node => { if (definitions.has(node)) anchorDefinitions.add(node); });
		const index = ordered.findIndex(definition => anchorDefinitions.has(definition.node));
		if (index < 0) throw new Error(`Unresolved setting domain insertion anchor: ${before.initializer.getText()}`);
		ordered.splice(index, 0, ...additions);
	}
	const conditions = new Map<string, { node: ts.Node; value: unknown }>();
	const conditionsFunction = source.named(SETTINGS_UI, "settingsConditions");
	source.walk(conditionsFunction, node => {
		if (!ts.isReturnStatement(node) || !node.expression || !ts.isObjectLiteralExpression(node.expression)) return;
		for (const property of node.expression.properties) if (property.name) {
			const name = source.name(property.name);
			if (name) conditions.set(name, { node: property, value: source.value(property) });
		}
	});
	const settings = ordered.map((definition, order) => {
		const { value, node, domain } = definition;
		const ui = record(value.ui) ? value.ui : undefined;
		const condition = typeof ui?.condition === "string" ? conditions.get(ui.condition) : undefined;
		if (typeof ui?.condition === "string" && !condition) throw new Error(`Unresolved settings visibility condition ${ui.condition} for ${value.id}`);
		return source.entry(`setting:${value.id}`, "setting", node, {
			order, domain, definition: source.serialize(value), configOnly: !ui,
			options: source.serialize(ui?.options ?? value.values),
			visibility: condition ? { name: ui?.condition, ...source.origin(condition.node), expression: condition.node.getText(), controller: source.serialize(condition.value) } : null,
			presentation: { source: "packages/tui/src/overlays/settings-defs.ts", host: SETTINGS_UI, owner: "OMP" },
			obligations: ui?.options === "runtime" || symbolic(ui?.options) || Array.isArray(ui?.options) && ui.options.some(symbolic) ? ["runtime-setting-options"] : [],
		});
	});
	return [...settings, ...settingsPresentationInventory(source, ordered, conditions, settings)];
}
