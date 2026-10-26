import { createModels } from "../../target/durable-integration.RMAHq5/pi/packages/ai/src/models.ts";
import { fauxAssistantMessage, fauxProvider, fauxToolCall } from "../../target/durable-integration.RMAHq5/pi/packages/ai/src/providers/faux.ts";
import { BACKGROUND_CONTEXT } from "../../target/durable-integration.RMAHq5/pi/packages/chord/src/context/index.ts";
import { createRegistry, defineExtension, defineTool, Harness } from "../../target/durable-integration.RMAHq5/pi/packages/durable/src/index.ts";
import { Type } from "../../target/durable-integration.RMAHq5/pi/node_modules/typebox/build/index.mjs";
import { openNodeSqliteStorage } from "../../target/durable-integration.RMAHq5/pi/packages/durable/src/storage/sqlite/node.ts";

const path = process.argv[2];
if (path === undefined) throw new Error("SQLite path argument is required");
const faux = fauxProvider();
const models = createModels();
models.setProvider(faux.provider);
faux.setResponses([fauxAssistantMessage([fauxToolCall("unsafe", {}, { id: "unsafe-1" })], { stopReason: "toolUse" })]);
const registry = createRegistry();
const unsafe = defineExtension({
	name: "unsafe",
	tools: [defineTool({
		name: "unsafe",
		description: "An externally visible non-idempotent action",
		parameters: Type.Object({}),
		execute: async () => {
			process.stdout.write("UNSAFE_DISPATCHED\n");
			await new Promise<void>(() => {});
			return { output: [] };
		},
	})],
});
registry.install(unsafe);
setInterval(() => {}, 60_000);
const harness = await Harness.open(await openNodeSqliteStorage(path), { models, registry, settings: {} }, BACKGROUND_CONTEXT);
const root = await harness.root(BACKGROUND_CONTEXT, { agent: { model: { provider: "faux", modelId: "faux-1" }, extensions: [unsafe] } });
const submission = await root.submit({ type: "input", content: "perform the unsafe action", requestId: "unsafe-interrupted" }, BACKGROUND_CONTEXT);
process.stdout.write(`SUBMISSION:${submission.id}\n`);
await new Promise<void>(() => {});
