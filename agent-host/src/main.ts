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
import { createAgentRegistry } from "./registry.ts";

const config = readConfig();
await loadDaemons();
const hostDb = openHostDatabase(config.dataDir);
const models = createModelService(hostDb.credentials, hostDb.db);
const storage = await openNodeSqliteStorage(resolve(config.dataDir, "durable.sqlite"));
const harness = await Harness.open(storage, { models: models.models, registry: createAgentRegistry(), env: envFor() }, BACKGROUND_CONTEXT);
await harness.resume();
const services = createAgentHostServices({ harness, db: hostDb.db, config, models });
const address = await services.http.listen();
process.stdout.write(`${JSON.stringify({ level: "info", message: "agent host listening", address: `${address.address}:${address.port}` })}\n`);


for (const signal of ["SIGINT", "SIGTERM"] as const) process.once(signal, async () => {
  await harness.close(BACKGROUND_CONTEXT);
  hostDb.close();
  process.exit(0);
});
