import { resolve as resolvePath } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const toolsDirectory = fileURLToPath(new URL(".", import.meta.url));
const checkout = resolvePath(toolsDirectory, "../../target/durable-integration.RMAHq5/pi");
const sources = new Map([
	["@earendil-works/pi-ai", "packages/ai/src/index.ts"],
	["@earendil-works/pi-durable", "packages/durable/src/index.ts"],
	["@earendil-works/chord", "packages/chord/src/index.ts"],
	["@earendil-works/chord/context", "packages/chord/src/context/index.ts"],
	["@earendil-works/chord/delta", "packages/chord/src/delta/index.ts"],
	["@earendil-works/chord/node", "packages/chord/src/node.ts"],
	["@earendil-works/chord/sqlite", "packages/chord/src/sqlite/index.ts"],
	["@earendil-works/chord/sqlite/node", "packages/chord/src/sqlite/node.ts"],
	["@earendil-works/pi-telemetry", "packages/telemetry/src/index.ts"],
	["@earendil-works/pi-ai/models", "packages/ai/src/models.ts"],
]);

export async function resolve(specifier, context, nextResolve) {
	let sourcePath = sources.get(specifier);
	if (sourcePath === undefined && specifier.startsWith("@earendil-works/pi-ai/utils/")) {
		sourcePath = `packages/ai/src/utils/${specifier.slice("@earendil-works/pi-ai/utils/".length)}.ts`;
	}
	if (sourcePath === undefined && specifier.startsWith("@earendil-works/pi-ai/providers/")) {
		sourcePath = `packages/ai/src/providers/${specifier.slice("@earendil-works/pi-ai/providers/".length)}.ts`;
	}
	if (sourcePath === undefined) return nextResolve(specifier, context);
	return { url: pathToFileURL(resolvePath(checkout, sourcePath)).href, shortCircuit: true };
}
