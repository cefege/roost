import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import { Harness } from "@earendil-works/pi-durable";
import { openNodeSqliteStorage } from "@earendil-works/pi-durable/storage/sqlite/node";
import { resolve } from "node:path";
import { readConfig } from "./config.ts";
import { loadDaemons } from "./daemons.ts";
import { envFor } from "./env.ts";
import { createAgentHostServices } from "./host.ts";
import { openHostDatabase } from "./host-db.ts";
import { createModelService } from "./models.ts";
import { watchHarness } from "./harness-watchdog.ts";
import { createAgentRegistry } from "./registry.ts";

function logLine(level: string, message: string, fields: Record<string, unknown> = {}): void {
  process.stdout.write(`${JSON.stringify({ level, message, ...fields })}\n`);
}
function exitFatally(message: string, cause: unknown): never {
  logLine("error", message, { error: cause === undefined ? null : String(cause instanceof Error && cause.cause ? `${cause.message}: ${String(cause.cause)}` : cause) });
  process.exit(1);
}
process.on("uncaughtException", error => exitFatally("uncaught exception", error));
process.on("unhandledRejection", error => exitFatally("unhandled rejection", error));

const config = readConfig();
await loadDaemons();
const hostDb = openHostDatabase(config.dataDir);
const models = createModelService(hostDb.credentials, hostDb.db);
const storage = await openNodeSqliteStorage(resolve(config.dataDir, "durable.sqlite"));
const harness = await Harness.open(storage, { models: models.models, registry: createAgentRegistry(), env: envFor() }, BACKGROUND_CONTEXT);
// Kubernetes restarts the container, which is the "reopen" pi asks for.
const watchdog = watchHarness(harness, (reason, cause) => exitFatally(reason, cause));
await harness.resume();
const services = createAgentHostServices({ harness, db: hostDb.db, config, models, onInternalError: () => void watchdog.check("internal error") });
const address = await services.http.listen();
logLine("info", "agent host listening", { address: `${address.address}:${address.port}` });

for (const signal of ["SIGINT", "SIGTERM"] as const) process.once(signal, async () => {
  watchdog.markShuttingDown();
  watchdog.stop();
  await harness.close(BACKGROUND_CONTEXT);
  hostDb.close();
  process.exit(0);
});
