import { execFile } from "node:child_process";
import path from "node:path";
import { promisify } from "node:util";
import type { InterfaceInventory } from "./inventory-types";
import { SourceIndex } from "./inventory-source";
import { schemaInventory } from "./inventory-schema";
import { registryInventory, settingsInventory } from "./inventory-registries";
import { componentInventory, featureInventory } from "./inventory-components";
import { runtimeSchemaInventory } from "./inventory-runtime-schema";
import { recordConcreteSubclasses } from "./inventory-abstract-bases";
import { classifyNativeAbsence } from "./inventory-native-classify";
import { classifyHostKindFallbacks } from "./inventory-kind-gates";
import { classifyUnadvertisedFeatures } from "./inventory-host-features";
import { classifyHostUnproduced, classifyUnemittedOperations, rustHostKeys } from "./inventory-host-audit";
import { classifyRelayOwnedReplies } from "./inventory-relay-owned";

/** Derive the pinned fork's interface without running its modules, controllers or config loaders. */
export async function readInventory(sourceRoot: string): Promise<InterfaceInventory> {
	const root = path.resolve(sourceRoot);
	const { stdout } = await promisify(execFile)("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" });
	const revision = stdout.trim();
	if (!/^[a-f0-9]{40,64}$/.test(revision)) throw new Error(`Cannot identify OMP source revision in ${root}`);
	const source = new SourceIndex(root);
	const components = componentInventory(source);
	const entries = [
		...schemaInventory(source, components),
		...registryInventory(source),
		...settingsInventory(source),
		...components,
		...featureInventory(source),
	];
	recordConcreteSubclasses(entries);
	classifyNativeAbsence(source, entries);
	classifyHostUnproduced(entries);
	classifyRelayOwnedReplies(entries, source, rustHostKeys());
	classifyUnemittedOperations(source, entries);
	classifyHostKindFallbacks(entries);
	classifyUnadvertisedFeatures(entries);
	const seen = new Set<string>();
	for (const entry of entries) {
		if (seen.has(entry.id)) throw new Error(`Duplicate canonical inventory identity: ${entry.id}`);
		seen.add(entry.id);
	}
	entries.sort((left, right) => left.id.localeCompare(right.id));
	return { revision, entries, runtimeSchema: runtimeSchemaInventory(source, entries) };
}

if (import.meta.main) {
	const args = process.argv.slice(2);
	if (args.length !== 2 || args[0] !== "--source" || !args[1]) throw new Error("Usage: bun run inventory.ts --source ROOT");
	process.stdout.write(`${JSON.stringify(await readInventory(args[1]), null, 2)}\n`);
}
