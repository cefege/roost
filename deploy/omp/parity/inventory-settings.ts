import ts from "typescript";
import type { InventoryEntry } from "./inventory-types";
import { record, SourceIndex, symbolic } from "./inventory-source";
import { conditionRowGates, nativeControlProjections, predicateProof, projectionDetails, sourceSpan } from "./inventory-setting-controls";

const DEFINITIONS = "packages/tui/src/overlays/settings-defs.ts";
const HOST = "packages/coding-agent/src/config/settings-ui.ts";
const REGISTRY = "packages/coding-agent/src/config/registry.ts";
interface Registration { node: ts.Node; value: Record<string, unknown> }
interface Condition { node: ts.Node; value: unknown }

/** Read the real getters and constructor field expressions used by the host projection. */
function settingProjection(source: SourceIndex, definition: Record<string, unknown>): Record<string, unknown> {
	const declaration = source.named(REGISTRY, "Setting");
	if (!ts.isClassDeclaration(declaration)) throw new Error("Setting registry owner is not a class");
	const constructor = declaration.members.find(ts.isConstructorDeclaration);
	if (!constructor?.body || constructor.parameters.length !== 1 || !ts.isIdentifier(constructor.parameters[0]!.name)) throw new Error("Unenumerated Setting constructor");
	const fields = new Map<string, unknown>();
	const pending = new Set<string>();
	const instance: Record<string, unknown> = new Proxy({}, {
		get(_target, key) {
			if (typeof key !== "string") return undefined;
			if (fields.has(key)) return fields.get(key);
			if (pending.has(key)) return source.unknown(declaration, `recursive Setting field ${key}`);
			pending.add(key);
			try {
				const getter = declaration.members.find(member => ts.isGetAccessorDeclaration(member) && source.name(member.name) === key);
				let value: unknown;
				if (getter && ts.isGetAccessorDeclaration(getter)) value = source.invokeSource(getter, [], new Map([["this", instance]]));
				else {
					const assignment = constructor.body!.statements.flatMap(statement => ts.isExpressionStatement(statement) && ts.isBinaryExpression(statement.expression) ? [statement.expression] : []).find(expression => expression.operatorToken.kind === ts.SyntaxKind.EqualsToken && ts.isPropertyAccessExpression(expression.left) && expression.left.expression.kind === ts.SyntaxKind.ThisKeyword && expression.left.name.text === key);
					value = assignment ? source.value(assignment.right, new Map([[constructor.parameters[0]!.name.getText(), definition], ["this", instance]])) : source.unknown(declaration, `unresolved Setting field ${key}`);
				}
				fields.set(key, value);
				return value;
			} finally { pending.delete(key); }
		},
	});
	return instance;
}

function discriminants(source: SourceIndex): Array<{ type: string; node: ts.Node }> {
	const declaration = source.named(DEFINITIONS, "SettingDef");
	if (!ts.isTypeAliasDeclaration(declaration) || !ts.isUnionTypeNode(declaration.type)) throw new Error("SettingDef is not a finite discriminated union");
	return declaration.type.types.map(type => {
		const target = ts.isTypeReferenceNode(type) ? source.declaration(type.typeName) : type;
		if (!target || !ts.isInterfaceDeclaration(target)) throw new Error(`Unenumerated SettingDef variant: ${type.getText()}`);
		const field = target.members.find(member => ts.isPropertySignature(member) && source.name(member.name) === "type");
		if (!field || !ts.isPropertySignature(field) || !field.type || !ts.isLiteralTypeNode(field.type) || !ts.isStringLiteralLike(field.type.literal)) throw new Error(`Nonliteral SettingDef discriminator: ${target.name.text}`);
		return { type: field.type.literal.text, node: field };
	});
}

export function settingsPresentationInventory(source: SourceIndex, registrations: readonly Registration[], conditions: ReadonlyMap<string, Condition>, settings: InventoryEntry[]): InventoryEntry[] {
	const entries: InventoryEntry[] = [];
	const settingsById = new Map(settings.map(entry => [entry.id, entry]));
	const variants = discriminants(source);
	const native = nativeControlProjections(source);
	const gates = conditionRowGates(source);
	for (const variant of variants) if (!native.has(variant.type)) throw new Error(`Canonical setting control ${variant.type} has no native producer`);
	const rowsByControl = new Map(variants.map(variant => [variant.type, [] as string[]]));
	const host = source.named(HOST, "createSettingsHost");
	const projections: ts.ObjectLiteralExpression[] = [];
	source.walk(host, node => {
		if (!ts.isObjectLiteralExpression(node)) return;
		const names = new Set(node.properties.flatMap(property => property.name ? [source.name(property.name)] : []));
		if (names.has("path") && names.has("enumValues") && names.has("credential")) projections.push(node);
	});
	if (projections.length !== 1) throw new Error(`Expected one canonical settings host entry projection; found ${projections.length}`);
	const projection = projections[0]!;
	const adapter = source.named(DEFINITIONS, "entryToSettingDef");
	if (!ts.isFunctionDeclaration(adapter)) throw new Error("Settings display adapter is not a source function");
	const projectionProof = { host: { ...source.origin(projection), expression: projection.getText() }, adapter: { ...source.origin(adapter), expression: adapter.getText() } };
	const conditionValues = Object.fromEntries([...conditions].map(([name, condition]) => [name, condition.value]));
	for (const { node, value } of registrations) {
		const path = String(value.id);
		const setting = settingsById.get(`setting:${path}`);
		if (!setting) throw new Error(`Missing setting registration obligation: ${path}`);
		setting.details.declaration = { ...source.origin(node), start: node.getStart() };
		const instance = settingProjection(source, value);
		const ui = instance.ui;
		if (ui === undefined) {
			setting.details.exposure = "config-only";
			setting.details.configOnly = true;
			setting.details.presentationProof = { ...projectionProof, ui: { getter: source.origin(source.named(REGISTRY, "Setting")), value: "undefined" } };
			continue;
		}
		setting.details.exposure = "ui";
		setting.details.settingId = path;
		const displayEntry = source.value(projection, new Map<string, unknown>([["setting", instance], ["ui", ui], ["note", ""], ["conditions", conditionValues]]));
		const displayed = source.invokeSource(adapter, [displayEntry]);
		setting.details.presentationProof = projectionProof;
		// OMP's adapter returns null for settings it deliberately keeps off the panel
		// (numbers without options, lists without an editor): no row can exist.
		if (displayed === null) {
			setting.details.exposure = "ui-hidden";
			setting.details.presentationProof = { ...projectionProof, display: { adapter: source.origin(adapter), value: "null" } };
			continue;
		}
		if (!record(displayed) || typeof displayed.type !== "string" || !rowsByControl.has(displayed.type)) {
			setting.details.exposure = "unresolved";
			setting.details.obligations = [...setting.details.obligations as string[], "unresolved-setting-presentation"];
			setting.details.presentationBoundary = source.serialize(displayed);
			continue;
		}
		rowsByControl.get(displayed.type)!.push(path);
		setting.details.control = displayed.type;
		const nativeProjection = native.get(displayed.type)!;
		setting.details.nativeProjection = projectionDetails(source, nativeProjection);
		setting.details.tab = record(ui) ? ui.tab : undefined;
		const options = displayed.options ?? displayed.values;
		if (Array.isArray(options)) for (const [index, option] of options.entries()) {
			const choice = typeof option === "string" ? { value: option, label: option } : option;
			if (!record(choice) || typeof choice.value !== "string") {
				setting.details.obligations = [...setting.details.obligations as string[], "unresolved-setting-choice"];
				continue;
			}
			const origin = typeof option === "object" && option ? source.origins.get(option) : undefined;
			const nativeValues = nativeProjection.returns.flatMap(returned => {
				if (returned.kind !== nativeProjection.numericKind) return [{ kind: returned.kind, value: choice.value, producer: returned }];
				const value = nativeProjection.converter ? source.invokeSource(nativeProjection.converter, [choice.value]) : undefined;
				if (symbolic(value)) {
					setting.details.obligations = [...setting.details.obligations as string[], "unresolved-setting-choice-native-value"];
					return [];
				}
				return typeof value === "number" ? [{ kind: returned.kind, value, producer: returned, converter: sourceSpan(source, nativeProjection.converter!) }] : [];
			});
			entries.push(source.entry(`setting-choice:${path}=${choice.value}`, "setting-choice", origin ?? node, { settingId: path, value: choice.value, label: choice.label, description: choice.description, control: displayed.type, optionIndex: index, exposure: "ui", declaration: { ...source.origin(origin ?? node), start: (origin ?? node).getStart() }, projection: projectionProof, nativeProjection: projectionDetails(source, nativeProjection), nativeValues }));
		}
		if (record(ui) && typeof ui.condition === "string") {
			const condition = conditions.get(ui.condition);
			if (!condition) throw new Error(`Missing settings visibility predicate ${ui.condition}`);
			for (const outcome of [true, false]) entries.push(source.entry(`setting-condition:${path}:${outcome}`, "setting-condition", node, { settingId: path, condition: ui.condition, outcome, exposure: "ui", predicate: predicateProof(source, condition.node), presentation: { owner: "OMP", row: path, tab: ui.tab, gate: gates.find(gate => Array.isArray(gate.settingId) && gate.settingId.length > 0), gates } }));
		}
	}
	for (const variant of variants) entries.push(source.entry(`control:${variant.type}`, "control", variant.node, { type: variant.type, settings: rowsByControl.get(variant.type), exposure: "ui", declaration: { ...source.origin(variant.node), start: variant.node.getStart() }, projection: projectionProof, nativeProjection: projectionDetails(source, native.get(variant.type)!) }));
	for (const [name, condition] of conditions) for (const outcome of [true, false]) entries.push(source.entry(`setting-condition:${name}:${outcome}`, "setting-condition", condition.node, { condition: name, outcome, exposure: "ui", predicate: predicateProof(source, condition.node), settings: registrations.filter(({ value }) => record(value.ui) && value.ui.condition === name).map(({ value }) => value.id), presentation: { gates, gate: gates.find(gate => Array.isArray(gate.settingId) && gate.settingId.length > 0), tabs: [...new Set(registrations.flatMap(({ value }) => record(value.ui) && value.ui.condition === name ? [value.ui.tab] : []))] } }));
	const tabs = source.valueNamed(DEFINITIONS, "SETTING_TABS");
	const metadata = source.valueNamed(DEFINITIONS, "TAB_METADATA");
	if (!Array.isArray(tabs) || tabs.some(tab => typeof tab !== "string")) throw new Error("Settings tabs are not a canonical literal registry");
	for (const [order, tab] of tabs.entries()) entries.push(source.entry(`settings-tab:${tab}`, "settings-tab", source.named(DEFINITIONS, "SETTING_TABS"), { value: tab, order, label: record(metadata) && record(metadata[tab]) ? metadata[tab].label : undefined, exposure: "ui" }));
	return entries;
}
