import type * as ThemeLoader from "@omp-source/packages/tui/src/theme/loader";
import type * as ThemeModule from "@omp-source/packages/tui/src/theme/theme";
import type * as ComposerRegistry from "@omp-source/packages/tui/src/overlays/composer-shape-registry";
import type * as ModelCatalog from "@omp-source/packages/catalog/src/models";
import type * as ModelThinking from "@omp-source/packages/catalog/src/model-thinking";
import type * as SettingsModule from "@omp-source/packages/coding-agent/src/config/settings";
import type * as SettingsUi from "@omp-source/packages/coding-agent/src/config/settings-ui";
import type * as PluginSettings from "@omp-source/packages/coding-agent/src/extensibility/plugins/settings-host";
import type * as SelectorModule from "@omp-source/packages/tui/src/overlays/settings-selector";
import type { Api, Model } from "@omp-source/packages/catalog/src/types";
import type { Effort } from "@omp-source/packages/catalog/src/effort";
import type { SubmenuOption } from "@omp-source/packages/tui/src/overlays/settings-defs";
import ts from "typescript";
import type { InterfaceInventory, InventoryEntry } from "./inventory-types";
import { SourceIndex } from "./inventory-source";
import { sourceSpan } from "./inventory-setting-controls";
import type { SourceSpan } from "./inventory-setting-controls";

const SELECTOR = "packages/tui/src/overlays/settings-selector.ts";
export type CanonicalSourceLoader = <T>(relative: string) => Promise<T>;
export interface RuntimeThinkingVariant { id: string; provider: string; modelId: string; model: Model<Api>; efforts: readonly Effort[] }
export interface RuntimeSettingsCatalog { themes: readonly string[]; composers: readonly SubmenuOption[]; thinking: readonly RuntimeThinkingVariant[] }
export interface RuntimeSettingOption { value: string; label: string; detail?: string }
export interface RuntimeSettingOptionState { id: string; settingId: string; options: readonly RuntimeSettingOption[]; model?: { provider: string; id: string }; efforts: readonly Effort[]; producer: SourceSpan }
export interface RuntimeSettingsInventory extends RuntimeSettingsCatalog { states: RuntimeSettingOptionState[]; factories: Array<{ settingId: string; producer: SourceSpan; contextFields: string[] }> }
export interface RuntimeSettingOptionContext { sourceRoot: string; workspace: string; load: CanonicalSourceLoader }
interface SourceRuntimeFactory { settingId: string; producer: SourceSpan; contextFields: string[]; node: ts.IfStatement }

/** Called only inside the runner's fresh HOME, registered source loader and network guard. */
export async function readRuntimeSettingsCatalog(load: CanonicalSourceLoader): Promise<RuntimeSettingsCatalog> {
	const themes = await load<typeof ThemeLoader>("packages/tui/src/theme/loader.ts");
	const composers = await load<typeof ComposerRegistry>("packages/tui/src/overlays/composer-shape-registry.ts");
	const catalog = await load<typeof ModelCatalog>("packages/catalog/src/models.ts");
	const thinking = await load<typeof ModelThinking>("packages/catalog/src/model-thinking.ts");
	const variants = new Map<string, RuntimeThinkingVariant>();
	for (const provider of catalog.getBundledProviders()) for (const model of catalog.getBundledModels(provider)) {
		const efforts = thinking.getSupportedEfforts(model);
		const id = `efforts:${JSON.stringify(efforts)}`;
		if (!variants.has(id)) variants.set(id, { id, provider, modelId: model.id, model, efforts });
	}
	if (variants.size === 0) throw new Error("Canonical bundled model catalog has no settings capability contexts");
	return { themes: await themes.getAvailableThemes(), composers: composers.getComposerShapeOptions(), thinking: [...variants.values()].sort((left, right) => left.id.localeCompare(right.id)) };
}

function runtimeFactories(source: SourceIndex): SourceRuntimeFactory[] {
	const methods: ts.MethodDeclaration[] = [];
	source.walk(source.file(SELECTOR), node => { if (ts.isMethodDeclaration(node) && node.name.getText() === "#submenuOptions") methods.push(node); });
	if (methods.length !== 1) throw new Error(`Expected one canonical runtime settings option factory; found ${methods.length}`);
	const factories: SourceRuntimeFactory[] = [];
	source.walk(methods[0]!, node => {
		if (!ts.isIfStatement(node)) return;
		const paths: string[] = [];
		source.walk(node.expression, expression => {
			if (ts.isBinaryExpression(expression) && expression.operatorToken.kind === ts.SyntaxKind.EqualsEqualsEqualsToken && ts.isPropertyAccessExpression(expression.left) && expression.left.name.text === "path" && ts.isStringLiteralLike(expression.right)) paths.push(expression.right.text);
		});
		if (paths.length === 0) return;
		const fields = new Set<string>();
		source.walk(node.thenStatement, expression => {
			if (ts.isPropertyAccessExpression(expression) && ts.isPropertyAccessExpression(expression.expression) && expression.expression.name.getText() === "#context" && expression.expression.expression.kind === ts.SyntaxKind.ThisKeyword) fields.add(expression.name.text);
		});
		for (const settingId of paths) factories.push({ settingId, producer: sourceSpan(source, node), contextFields: [...fields], node });
	});
	return factories;
}

/** Materialize the real selector's options; this does not import modules until isolation is ready. */
export async function materializeSettingOptions(inventory: InterfaceInventory, context: RuntimeSettingOptionContext): Promise<RuntimeSettingsInventory> {
	const source = new SourceIndex(context.sourceRoot);
	const catalog = await readRuntimeSettingsCatalog(context.load);
	const { Settings } = await context.load<typeof SettingsModule>("packages/coding-agent/src/config/settings.ts");
	const { createSettingsHost } = await context.load<typeof SettingsUi>("packages/coding-agent/src/config/settings-ui.ts");
	const { createPluginSettingsHost } = await context.load<typeof PluginSettings>("packages/coding-agent/src/extensibility/plugins/settings-host.ts");
	const { SettingsSelectorComponent } = await context.load<typeof SelectorModule>(SELECTOR);
	const { initTheme } = await context.load<typeof ThemeModule>("packages/tui/src/theme/theme.ts");
	const store = await Settings.loadIsolated({ cwd: context.workspace });
	// The selector's row glyphs read the process theme, exactly as the interactive app initializes it first.
	await initTheme(false);
	const host = createSettingsHost(store);
	const plugins = createPluginSettingsHost(context.workspace);
	const factories = runtimeFactories(source);
	const states: RuntimeSettingOptionState[] = [];
	const byId = new Map(inventory.entries.map(entry => [entry.id, entry]));
	const add = (entry: InventoryEntry): void => { if (byId.has(entry.id)) throw new Error(`Duplicate runtime settings obligation: ${entry.id}`); byId.set(entry.id, entry); inventory.entries.push(entry); };
	for (const factory of factories) {
		const entry = byId.get(`setting:${factory.settingId}`);
		if (!entry) throw new Error(`Runtime factory has no registered setting: ${factory.settingId}`);
		if (factory.contextFields.some(field => !["availableThinkingLevels", "availableThemes"].includes(field))) throw new Error(`Unresolved canonical runtime option context for ${factory.settingId}: ${factory.contextFields.join(", ")}`);
		const variants = factory.contextFields.includes("availableThinkingLevels") ? catalog.thinking : [catalog.thinking[0]!];
		const factoryId = `setting-options-factory:${factory.settingId}`;
		const factoryNode = factory.node;
		add(source.entry(factoryId, "setting-options-factory", factoryNode, { settingId: factory.settingId, exposure: "ui", producer: factory.producer, contextFields: factory.contextFields, materialization: { source: "actual-SettingsSelectorComponent.describe", home: process.env.HOME, workspace: context.workspace }, states: [] }));
		for (const variant of variants) {
			const stateId = factory.contextFields.includes("availableThinkingLevels") ? variant.id : "registered";
			const component = new SettingsSelectorComponent({ settings: host, plugins, availableThinkingLevels: [...variant.efforts], thinkingLevel: undefined, availableThemes: [...catalog.themes], providers: [], model: variant.model }, {
				onChange: () => { throw new Error("Runtime option materialization unexpectedly changed a setting"); },
				onCancel: () => { throw new Error("Runtime option materialization unexpectedly closed settings"); },
			});
			for (const character of factory.settingId) component.handleInput(character);
			const described = component.describe({ cols: 120, dark: true, reduceMotion: true, supports: kind => kind === "prefs", feature: () => false });
			if (described.k !== "prefs") throw new Error(`Canonical settings factory did not describe prefs for ${factory.settingId}`);
			const row = described.p?.sections.flatMap(section => section.rows).find(row => row.id === factory.settingId);
			if (!row || row.control.k !== "choice") throw new Error(`Canonical runtime setting ${factory.settingId} has no visible choice row`);
			const options = row.control.options.map(option => ({ value: option.value, label: option.label, detail: option.detail }));
			const state: RuntimeSettingOptionState = { id: stateId, settingId: factory.settingId, options, model: factory.contextFields.includes("availableThinkingLevels") ? { provider: variant.provider, id: variant.modelId } : undefined, efforts: variant.efforts, producer: factory.producer };
			states.push(state);
			const stateEntryId = `setting-options-state:${factory.settingId}=${stateId}`;
			add(source.entry(stateEntryId, "setting-options-state", factoryNode, { ...state, exposure: "ui", factory: factoryId, settingId: factory.settingId, value: options.map(option => option.value), sourceCatalog: { themes: "packages/tui/src/theme/loader.ts#getAvailableThemes", composers: "packages/tui/src/overlays/composer-shape-registry.ts#getComposerShapeOptions", model: "packages/catalog/src/models.ts#getBundledModels", capability: "packages/catalog/src/model-thinking.ts#getSupportedEfforts" } }));
			(byId.get(factoryId)!.details.states as string[]).push(stateEntryId);
			for (const [optionIndex, option] of options.entries()) {
				const id = `setting-choice:${factory.settingId}=${option.value}`;
				let choice = byId.get(id);
				if (!choice) {
					choice = source.entry(id, "setting-choice", factoryNode, { settingId: factory.settingId, value: option.value, label: option.label, description: option.detail, control: entry.details.control, optionIndex, exposure: "ui", declaration: factory.producer });
					add(choice);
				}
				const observed = Array.isArray(choice.details.runtimeStates) ? choice.details.runtimeStates as string[] : [];
				observed.push(stateEntryId);
				choice.details.runtimeStates = observed;
				choice.details.runtimeFactory = { entry: factoryId, producer: factory.producer, nativeKind: row.control.k, value: option.value };
				choice.details.nativeValues = [{ kind: row.control.k, value: option.value, producer: factory.producer }];
			}
		}
		entry.details.obligations = (entry.details.obligations as string[]).filter(obligation => obligation !== "runtime-setting-options");
		entry.details.runtimeFactory = factoryId;
		entry.details.runtimeStates = states.filter(state => state.settingId === factory.settingId).map(state => `setting-options-state:${state.settingId}=${state.id}`);
	}
	const materialized = { ...catalog, states, factories: factories.map(({ node: _node, ...factory }) => factory) };
	inventory.runtimeSettings = materialized;
	inventory.entries.sort((left, right) => left.id.localeCompare(right.id));
	return materialized;
}
