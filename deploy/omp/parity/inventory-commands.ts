import type { Node } from "typescript";
import type { InventoryEntry } from "./inventory-types";
import { record, SourceIndex, symbolic } from "./inventory-source";

/** Aliases dispatch the same registry object, but each public invocation remains an obligation. */
export function subcommandInventory(source: SourceIndex, command: Record<string, unknown>, aliases: readonly string[], node: Node, handlers: unknown): InventoryEntry[] {
	const subcommands = command.subcommands ?? [];
	if (symbolic(subcommands)) return [];
	if (!Array.isArray(subcommands)) throw new Error(`Unenumerated subcommand registry for /${command.name}`);
	const entries: InventoryEntry[] = [];
	for (const [order, subcommand] of subcommands.entries()) {
		if (symbolic(subcommand)) continue;
		if (!record(subcommand) || typeof subcommand.name !== "string") throw new Error(`Unenumerated subcommand for /${command.name} at ${order}`);
		const declaration = source.origins.get(subcommand) ?? source.fields.get(command)?.get("subcommands") ?? node;
		const canonical = `subcommand:${command.name} ${subcommand.name}`;
		for (const token of [String(command.name), ...aliases]) {
			entries.push(source.entry(`subcommand:${token} ${subcommand.name}`, "subcommand", declaration, {
				command: command.name, token, invocation: `/${token} ${subcommand.name}`, name: subcommand.name, canonical,
				aliasOf: token === command.name ? undefined : canonical, order, usage: subcommand.usage,
				definition: source.serialize(subcommand), handlers, exposure: "ui",
				declaration: { ...source.origin(declaration), start: declaration.getStart(), end: declaration.getEnd() },
				registry: { ...source.origin(node), expression: node.getText() },
			}));
		}
	}
	return entries;
}
